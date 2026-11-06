"""Compare Conduit's load-balancing strategies under mixed-cost traffic and failures (issue #629).

    python scripts/lb_compare.py [--binary PATH] [--duration SECONDS] [--strategies a,b] [--scenarios x,y]
                                 [--json OUT.json]

For every strategy and scenario it starts mock upstreams (in this process), starts the Conduit binary with a
generated config, drives it with a closed-loop load (a fixed number of client threads, each sending the next
request when the last one is answered) and prints Markdown tables. Standard library only, Python 3.11+.

Scenarios (all with 4 upstreams unless said otherwise):

  mixed     90% light requests (5 ms, 1 KiB) and 10% heavy ones (100 ms, 256 KiB). What matters: the latency of
            the light requests (do they queue behind heavy ones?) and the tail of everything.
  slow-one  Like mixed, but upstream 0 takes 8x as long for every request. What matters: how much traffic the
            slow one still gets, and the tail.
  weights   3 upstreams with weights 3:1:1 (light requests only). What matters: the share of upstream 0 (60%
            would be proportional). Strategies that ignore weights are shown for contrast.
  outage    Like mixed; upstream 0 dies for the middle third of the run (its connections are dropped) and comes
            back. What matters: errors seen by clients, and upstream 0's share before, during and after.

Conduit's site listener binds every interface (the site port has no bind address; only the admin API
does), so for the minutes a run takes the proxy under test is reachable from the network; the mock
upstreams and the admin API listen on 127.0.0.1 only. Run it on a machine you are happy to expose that way.

Absolute numbers depend on the machine and the build (a debug build adds CPU cost per request); compare
strategies against each other within one run. Each cell is one run of --duration seconds after a 2 s warm-up;
a single run is a sample, not a distribution: repeat before drawing a conclusion from a small difference.
"""

from __future__ import annotations

import argparse
import http.client
import json
import os
import random
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STRATEGIES = [
    "round-robin",
    "random",
    "weighted-round-robin",
    "least-conn",
    "least-response-time",
    "p2c",
    "least-bandwidth",
]
SCENARIOS = ["mixed", "slow-one", "weights", "outage"]
CLIENTS = 32
WARMUP_SECONDS = 2.0
LIGHT_MS, HEAVY_MS = 5, 100
LIGHT_BYTES, HEAVY_BYTES = 1024, 256 * 1024
HEAVY_RATIO = 0.10


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Upstream:
    """A mock HTTP/1.1 upstream whose speed can change and which can be killed and revived."""

    def __init__(self, index: int):
        self.index = index
        self.port = free_port()
        self.speed = 1.0  # multiplier on the service time
        self.down = False
        self.hits = 0
        self._server: ThreadingHTTPServer | None = None
        self._start()

    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def _start(self) -> None:
        upstream = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):  # silence
                pass

            def do_GET(self):
                if upstream.down:
                    self.close_connection = True
                    return
                heavy = "h=1" in self.path
                time.sleep((HEAVY_MS if heavy else LIGHT_MS) * upstream.speed / 1000.0)
                if upstream.down:
                    self.close_connection = True
                    return
                body = b"x" * (HEAVY_BYTES if heavy else LIGHT_BYTES)
                upstream.hits += 1
                self.send_response(200)
                self.send_header("X-Upstream", str(upstream.index))
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        class QuietServer(ThreadingHTTPServer):
            daemon_threads = True

            def handle_error(self, request, client_address):  # a reset when Conduit stops is expected
                pass

        server = QuietServer(("127.0.0.1", self.port), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self._server = server

    def kill(self) -> None:
        self.down = True
        if self._server:
            self._server.shutdown()
            self._server.server_close()
            self._server = None

    def revive(self) -> None:
        self.down = False
        self._start()

    def stop(self) -> None:
        self.down = True
        if self._server:
            self._server.shutdown()
            self._server.server_close()
            self._server = None


class Conduit:
    def __init__(self, binary: str, upstream_urls: list[str], weights: list[int], strategy: str):
        self.port, self.admin = free_port(), free_port()
        config = {
            "global": {"admin": {"bind": f"127.0.0.1:{self.admin}"}},
            "sites": [
                {
                    "port": self.port,
                    "proxy": {
                        "/": {
                            "targets": [{"url": u, "weight": w} for u, w in zip(upstream_urls, weights)],
                            "strategy": strategy,
                        }
                    },
                }
            ],
        }
        self.dir = tempfile.TemporaryDirectory()
        path = os.path.join(self.dir.name, "conduit.json")
        with open(path, "w", encoding="utf-8") as f:
            json.dump(config, f)
        self.proc = subprocess.Popen(
            [binary, "--config", path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        try:
            self._wait_ready()
        except BaseException:
            self.stop()  # do not leave the child running when it never became ready
            raise

    def _wait_ready(self) -> None:
        deadline = time.time() + 30
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError("conduit exited during start-up (is the config valid for this build?)")
            try:
                c = http.client.HTTPConnection("127.0.0.1", self.admin, timeout=1)
                c.request("GET", "/status")
                if c.getresponse().status in (200, 401):
                    return
            except OSError:
                time.sleep(0.2)
        raise RuntimeError("conduit did not become ready in 30 s")

    def stop(self) -> None:
        self.proc.kill()
        self.proc.wait()
        self.dir.cleanup()


class Sample:
    __slots__ = ("t", "ms", "upstream", "heavy", "ok")

    def __init__(self, t, ms, upstream, heavy, ok):
        self.t, self.ms, self.upstream, self.heavy, self.ok = t, ms, upstream, heavy, ok


def drive(conduit_port: int, seconds: float, seed: int) -> list[Sample]:
    """Closed loop: CLIENTS threads, each one request at a time, for `seconds`."""
    samples: list[Sample] = []
    lock = threading.Lock()
    stop_at = time.time() + seconds

    def worker(n: int) -> None:
        rng = random.Random(seed * 1000 + n)
        mine: list[Sample] = []
        conn = http.client.HTTPConnection("127.0.0.1", conduit_port, timeout=30)
        while time.time() < stop_at:
            heavy = rng.random() < HEAVY_RATIO
            started = time.time()
            upstream, ok = -1, False
            try:
                conn.request("GET", "/?h=1" if heavy else "/")
                resp = conn.getresponse()
                resp.read()
                ok = resp.status == 200
                upstream = int(resp.getheader("X-Upstream", "-1")) if ok else -1
            except (OSError, http.client.HTTPException):
                conn.close()
                conn = http.client.HTTPConnection("127.0.0.1", conduit_port, timeout=30)
            mine.append(Sample(started, (time.time() - started) * 1000.0, upstream, heavy, ok))
        with lock:
            samples.extend(mine)

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(CLIENTS)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return samples


def percentile(values: list[float], p: float) -> float:
    if not values:
        return float("nan")
    values = sorted(values)
    return values[min(len(values) - 1, int(len(values) * p))]


def run_cell(binary: str, strategy: str, scenario: str, duration: float, seed: int) -> dict:
    count = 3 if scenario == "weights" else 4
    weights = [3, 1, 1] if scenario == "weights" else [1] * count
    upstreams = [Upstream(i) for i in range(count)]
    if scenario == "slow-one":
        upstreams[0].speed = 8.0
    conduit = None
    try:
        conduit = Conduit(binary, [u.url() for u in upstreams], weights, strategy)
        # warm-up (not recorded), so that connections exist and the first latency samples are in
        drive(conduit.port, WARMUP_SECONDS, seed + 1)
        for u in upstreams:
            u.hits = 0

        killer = None
        if scenario == "outage":

            def outage() -> None:
                time.sleep(duration / 3)
                upstreams[0].kill()
                time.sleep(duration / 3)
                upstreams[0].revive()

            killer = threading.Thread(target=outage, daemon=True)
            killer.start()
        t0 = time.time()
        samples = drive(conduit.port, duration, seed)
        if killer:
            killer.join()
    finally:
        if conduit:
            conduit.stop()
        for u in upstreams:
            u.stop()

    ok = [s for s in samples if s.ok]
    light = [s.ms for s in ok if not s.heavy]
    heavy = [s.ms for s in ok if s.heavy]
    share = [sum(1 for s in ok if s.upstream == i) / max(1, len(ok)) * 100 for i in range(count)]
    cell = {
        "requests": len(samples),
        "rps": len(samples) / duration,
        "errors": len(samples) - len(ok),
        "p50": percentile([s.ms for s in ok], 0.50),
        "p99": percentile([s.ms for s in ok], 0.99),
        "p999": percentile([s.ms for s in ok], 0.999),
        "light_p99": percentile(light, 0.99),
        "heavy_p50": percentile(heavy, 0.50),
        "share": share,
    }
    # upstream 0's share per second, to see oscillation (standard deviation) and the phases of an outage
    seconds = int(duration)
    per_second = []
    for sec in range(seconds):
        window = [s for s in ok if sec <= s.t - t0 < sec + 1]
        per_second.append(sum(1 for s in window if s.upstream == 0) / max(1, len(window)) * 100)
    cell["share0_stdev"] = statistics.pstdev(per_second) if per_second else float("nan")
    if scenario == "outage":
        third = max(1, seconds // 3)
        phases = (per_second[:third], per_second[third : 2 * third], per_second[2 * third :])
        cell["share0_phases"] = [statistics.fmean(p) if p else float("nan") for p in phases]
        cell["errors_phases"] = [
            sum(1 for s in samples if not s.ok and lo <= s.t - t0 < hi)
            for lo, hi in ((0, third), (third, 2 * third), (2 * third, seconds + 1))
        ]
    return cell


def table(scenario: str, results: dict[str, dict]) -> str:
    lines = [f"### {scenario}", ""]
    if scenario == "outage":
        lines += [
            "| strategy | rps | errors (before / during / after) | share of upstream 0 % (before / during / after) | p99 ms |",
            "|---|---:|---|---|---:|",
        ]
        for name, c in results.items():
            e, s = c["errors_phases"], c["share0_phases"]
            lines.append(
                f"| {name} | {c['rps']:.0f} | {e[0]} / {e[1]} / {e[2]} | "
                f"{s[0]:.0f} / {s[1]:.0f} / {s[2]:.0f} | {c['p99']:.0f} |"
            )
    else:
        lines += [
            "| strategy | rps | errors | p50 ms | p99 ms | p99.9 ms | light p99 ms | share per upstream % | share-0 stdev per s |",
            "|---|---:|---:|---:|---:|---:|---:|---|---:|",
        ]
        for name, c in results.items():
            shares = " / ".join(f"{x:.0f}" for x in c["share"])
            lines.append(
                f"| {name} | {c['rps']:.0f} | {c['errors']} | {c['p50']:.0f} | {c['p99']:.0f} | "
                f"{c['p999']:.0f} | {c['light_p99']:.0f} | {shares} | {c['share0_stdev']:.1f} |"
            )
    return "\n".join(lines) + "\n"


def find_binary(explicit: str | None) -> str:
    if explicit:
        return explicit
    exe = "conduit.exe" if os.name == "nt" else "conduit"
    for profile in ("release", "debug"):
        path = os.path.join("target", profile, exe)
        if os.path.exists(path):
            return path
    sys.exit("no target/release or target/debug conduit binary: build one or pass --binary")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary")
    ap.add_argument("--duration", type=float, default=12.0, help="seconds per run after the warm-up")
    ap.add_argument("--strategies", default=",".join(STRATEGIES))
    ap.add_argument("--scenarios", default=",".join(SCENARIOS))
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--json", help="also write the raw results here")
    args = ap.parse_args()

    binary = find_binary(args.binary)
    strategies = [s for s in args.strategies.split(",") if s]
    scenarios = [s for s in args.scenarios.split(",") if s]
    print(f"binary: {binary}, {CLIENTS} clients, {args.duration:.0f} s per run after a {WARMUP_SECONDS:.0f} s warm-up\n")
    everything: dict[str, dict] = {}
    for scenario in scenarios:
        results: dict[str, dict] = {}
        for strategy in strategies:
            print(f"  running {scenario} / {strategy} ...", file=sys.stderr, flush=True)
            results[strategy] = run_cell(binary, strategy, scenario, args.duration, args.seed)
        everything[scenario] = results
        print(table(scenario, results))
    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(everything, f, indent=2)


if __name__ == "__main__":
    main()
