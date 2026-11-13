#!/usr/bin/env bash
# Runs a built conduit binary in a clean Debian container and checks that it does what a user depends on: it starts,
# serves a static file, proxies to another conduit, and takes a multipart upload (issue #696, #700).
#
#   scripts/smoke-binary.sh BINARY [IMAGE]       default image: i386/debian:12
#
# Needs Docker. i386/debian:12 is a 32-bit Debian userland, so the 32-bit x86 release binary runs there natively on an
# x86-64 host. It is the nearest thing in CI to the old 32-bit distributions the binary is built for (glibc 2.17+
# baseline, kernel 3.2+): the binary is built on Ubuntu 16.04's glibc by `cross`, and the container has Debian 12's.
# The test after it, `cross test` of the standard crates, runs the same code on the same target but in the build
# image; this one runs the finished artifact in a different distribution, which is what the release ships.
#
# Two instances run in the container: a backend that serves a static file, and a front that proxies to it and also
# accepts uploads. The checks, in order:
#   - the ELF class is the one the image's userland has (32-bit in i386/debian);
#   - `conduit --version` and `conduit validate` work;
#   - the front answers a request that it proxies to the backend (the path the ARM tests under QEMU could not run);
#   - the front's health endpoint answers;
#   - an upload of a file is saved with the content it was sent with, and a file of a type that is not allowed is
#     refused with nothing left on disk.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: $0 BINARY [IMAGE]" >&2
  exit 2
fi
BIN=$(realpath "$1")
IMAGE="${2:-i386/debian:12}"

echo "== $IMAGE"
docker run --rm -v "$BIN:/usr/local/bin/conduit:ro" "$IMAGE" bash -euo pipefail -c '
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends curl ca-certificates >/dev/null

  # ELF class byte (offset 4): 1 = 32-bit, 2 = 64-bit; it must match the userland of this image.
  class=$(od -An -tu1 -j4 -N1 /usr/local/bin/conduit | tr -d " ")
  case "$(dpkg --print-architecture)" in
    i386|armhf|armel) want=1 ;;
    *) want=2 ;;
  esac
  [ "$class" = "$want" ] || { echo "ELF class $class, expected $want" >&2; exit 1; }

  conduit --version

  # a failed check shows what the two instances said
  trap '"'"'rc=$?; if [ $rc -ne 0 ]; then echo "--- backend log"; cat /srv/backend.log 2>/dev/null; echo "--- front log"; cat /srv/front.log 2>/dev/null; fi'"'"' EXIT

  mkdir -p /srv/www /srv/uploads
  echo "hello from the backend" > /srv/www/hello.txt

  cat > /srv/backend.yaml <<EOF
port: 9001
host: 127.0.0.1
static: /srv/www
EOF
  cat > /srv/front.yaml <<EOF
port: 9000
host: 127.0.0.1
healthCheck: true
upload:
  path: /files
  dir: /srv/uploads
  allowedMimeTypes:
    - "image/png"
proxy: "http://127.0.0.1:9001"
EOF
  conduit validate -c /srv/backend.yaml
  conduit validate -c /srv/front.yaml

  conduit -c /srv/backend.yaml >/srv/backend.log 2>&1 &
  conduit -c /srv/front.yaml >/srv/front.log 2>&1 &
  # Not a fixed sleep: wait until both answer, or fail with their logs.
  for port in 9001 9000; do
    up=
    for _ in $(seq 1 50); do
      if curl -s -o /dev/null "http://127.0.0.1:$port/"; then up=1; break; fi
      sleep 0.2
    done
    [ -n "$up" ] || { echo "nothing listens on $port"; cat /srv/backend.log /srv/front.log; exit 1; }
  done

  # the proxy path: front -> backend
  [ "$(curl -sf http://127.0.0.1:9000/hello.txt)" = "hello from the backend" ]
  # the front answers its health endpoint itself
  [ "$(curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:9000/__health__)" = "200" ]

  # an upload: the saved file has the bytes that were sent
  head -c 100000 /dev/urandom > /srv/in.png
  curl -sf -F "file=@/srv/in.png;type=image/png" http://127.0.0.1:9000/files > /srv/up.json
  grep -q "\"status\":\"ok\"" /srv/up.json
  [ "$(ls /srv/uploads | wc -l)" = "1" ]
  cmp /srv/in.png /srv/uploads/*.png
  # a type that is not allowed is refused and leaves nothing behind
  echo "text" > /srv/in.txt
  code=$(curl -s -o /dev/null -w "%{http_code}" -F "file=@/srv/in.txt;type=text/plain" http://127.0.0.1:9000/files)
  [ "$code" = "415" ] || [ "$code" = "400" ] || { echo "upload of a refused type answered $code" >&2; exit 1; }
  [ "$(ls /srv/uploads | wc -l)" = "1" ]

  echo "smoke test passed"
'
