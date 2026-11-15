#!/usr/bin/env python3
"""Small servers and clients that speak the opening of real wire protocols, for the `protocols` e2e scenario.

    wire.py server PROTOCOL PORT        PROTOCOL: redis mqtt postgres smtp ssh rtmp ftp
    wire.py client PROTOCOL HOST PORT   prints one JSON object

The servers are not the real products; they answer the exchange a real client opens with, byte for byte (RESP, MQTT 3.1.1,
the PostgreSQL v3 startup with the SSL request, SMTP, the SSH identification strings, the RTMP handshake, FTP control and
a passive data connection), so what is checked is that Conduit's TCP mode carries that protocol, not that a product works.
The clients use the standard library's own protocol clients where it has one (smtplib, ftplib). Standard library only.
"""
import ftplib
import json
import os
import smtplib
import socket
import socketserver
import struct
import sys
import threading


def bind_address():
    """The address of this container: the servers listen there and not on every interface (BIND_ADDRESS overrides)."""
    return os.environ.get("BIND_ADDRESS") or socket.gethostbyname(socket.gethostname())


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        part = sock.recv(n - len(buf))
        if not part:
            raise ConnectionError("closed")
        buf += part
    return buf


# ── servers ───────────────────────────────────────────────────────────────────────────────────────────────────────
class Redis(socketserver.StreamRequestHandler):
    store = {}

    def handle(self):
        while True:
            head = self.rfile.readline()
            if not head:
                return
            args = []
            for _ in range(int(head[1:])):
                length = int(self.rfile.readline()[1:])
                args.append(self.rfile.read(length + 2)[:-2])
            cmd = args[0].upper()
            if cmd == b"PING":
                self.wfile.write(b"+PONG\r\n")
            elif cmd == b"SET":
                self.store[args[1]] = args[2]
                self.wfile.write(b"+OK\r\n")
            elif cmd == b"GET":
                value = self.store.get(args[1])
                self.wfile.write(b"$-1\r\n" if value is None else b"$%d\r\n%s\r\n" % (len(value), value))
            else:
                self.wfile.write(b"-ERR unknown command\r\n")
            self.wfile.flush()


class Mqtt(socketserver.BaseRequestHandler):
    def handle(self):
        sock = self.request
        while True:
            try:
                first = recv_exact(sock, 1)[0]
            except ConnectionError:
                return
            length, shift = 0, 0
            while True:  # the variable-length "remaining length"
                b = recv_exact(sock, 1)[0]
                length |= (b & 0x7F) << shift
                shift += 7
                if not b & 0x80:
                    break
            body = recv_exact(sock, length)
            kind = first >> 4
            if kind == 1:  # CONNECT -> CONNACK (session present 0, accepted)
                sock.sendall(b"\x20\x02\x00\x00")
            elif kind == 3 and (first >> 1) & 3 == 1:  # PUBLISH QoS 1 -> PUBACK with its packet id
                topic_len = struct.unpack(">H", body[:2])[0]
                sock.sendall(b"\x40\x02" + body[2 + topic_len:4 + topic_len])
            elif kind == 8:  # SUBSCRIBE -> SUBACK granting QoS 1
                sock.sendall(b"\x90\x03" + body[:2] + b"\x01")
            elif kind == 12:  # PINGREQ -> PINGRESP
                sock.sendall(b"\xd0\x00")
            elif kind == 14:  # DISCONNECT
                return


class Postgres(socketserver.BaseRequestHandler):
    def handle(self):
        sock = self.request
        length, code = struct.unpack(">II", recv_exact(sock, 8))
        if code == 80877103:  # SSLRequest: this server has no TLS, so 'N'; the client goes on in cleartext
            sock.sendall(b"N")
            length, code = struct.unpack(">II", recv_exact(sock, 8))
        params = recv_exact(sock, length - 8).split(b"\x00")
        user = params[params.index(b"user") + 1].decode()
        sock.sendall(b"R\x00\x00\x00\x08\x00\x00\x00\x00")  # AuthenticationOk
        sock.sendall(b"S" + struct.pack(">I", 4 + len(b"server_version\x0016.0\x00")) + b"server_version\x0016.0\x00")
        sock.sendall(b"Z\x00\x00\x00\x05I")  # ReadyForQuery (idle)
        note = f"hello {user}".encode() + b"\x00"
        while True:  # one simple query is answered with one text row, then Terminate ends it
            kind = sock.recv(1)
            if not kind or kind == b"X":
                return
            (n,) = struct.unpack(">I", recv_exact(sock, 4))
            query = recv_exact(sock, n - 4)
            if kind == b"Q":
                row = struct.pack(">H", 1) + struct.pack(">I", len(note) - 1) + note[:-1]
                desc = struct.pack(">H", 1) + b"greeting\x00" + struct.pack(">IHIhiH", 0, 0, 25, -1, -1, 0)
                sock.sendall(b"T" + struct.pack(">I", 4 + len(desc)) + desc)
                sock.sendall(b"D" + struct.pack(">I", 4 + len(row)) + row)
                sock.sendall(b"C\x00\x00\x00\x0dSELECT 1\x00")
                sock.sendall(b"Z\x00\x00\x00\x05I")
                del query


class Smtp(socketserver.StreamRequestHandler):
    def handle(self):
        send = lambda line: (self.wfile.write(line.encode() + b"\r\n"), self.wfile.flush())
        send("220 smtp.test ESMTP e2e")
        data_mode, mail = False, []
        while True:
            line = self.rfile.readline()
            if not line:
                return
            text = line.decode().rstrip("\r\n")
            if data_mode:
                if text == ".":
                    data_mode = False
                    send("250 2.0.0 queued as " + str(len(mail)))
                else:
                    mail.append(text)
                continue
            word = text.split(" ", 1)[0].upper()
            if word == "EHLO":
                send("250-smtp.test")
                send("250 8BITMIME")
            elif word in ("MAIL", "RCPT", "RSET", "NOOP"):
                send("250 2.1.0 ok")
            elif word == "DATA":
                send("354 end data with <CR><LF>.<CR><LF>")
                data_mode = True
            elif word == "QUIT":
                send("221 2.0.0 bye")
                return
            else:
                send("502 5.5.2 not implemented")


class Ssh(socketserver.BaseRequestHandler):
    def handle(self):
        sock = self.request
        sock.sendall(b"SSH-2.0-e2e_server\r\n")
        client = b""
        while not client.endswith(b"\n"):
            client += sock.recv(1)
        sock.sendall(b"you said " + client)  # the server speaks next only after the client's identification string


class Rtmp(socketserver.BaseRequestHandler):
    def handle(self):
        sock = self.request
        c0 = recv_exact(sock, 1)
        c1 = recv_exact(sock, 1536)
        s1 = struct.pack(">II", 0, 0) + os.urandom(1528)
        sock.sendall(c0 + s1 + c1)  # S0, S1, S2 (S2 echoes C1)
        c2 = recv_exact(sock, 1536)
        assert c2 == s1, "C2 must echo S1"
        sock.sendall(b"handshake-done")


class Ftp(socketserver.StreamRequestHandler):
    """Control connection and passive data connections on $PASV_PORT; the 227 reply names $PASV_HOST and, when set,
    $PASV_ADVERTISE_PORT: where a client must connect, which behind a proxy is the proxy's address, not the server's."""

    def handle(self):
        send = lambda line: (self.wfile.write(line.encode() + b"\r\n"), self.wfile.flush())
        send("220 ftp.test ready")
        listener = None
        while True:
            line = self.rfile.readline()
            if not line:
                return
            word = line.decode().strip().split(" ", 1)[0].upper()
            if word == "USER":
                send("331 password please")
            elif word == "PASS":
                send("230 logged in")
            elif word in ("TYPE", "OPTS"):
                send("200 ok")
            elif word == "PASV":
                listener = socket.socket()
                listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                port = int(os.environ["PASV_PORT"])
                listener.bind((bind_address(), port))
                listener.listen(1)
                host = socket.gethostbyname(os.environ["PASV_HOST"]).replace(".", ",")
                shown = int(os.environ.get("PASV_ADVERTISE_PORT", port))  # the port a client is told to connect to
                send(f"227 Entering Passive Mode ({host},{shown >> 8},{shown & 255})")
            elif word == "LIST" and listener:
                send("150 here comes the listing")
                data, _ = listener.accept()
                data.sendall(b"-rw-r--r-- 1 e2e e2e 5 Jan 01 00:00 hello.txt\r\n")
                data.close()
                listener.close()
                send("226 done")
            elif word == "QUIT":
                send("221 bye")
                return
            else:
                send("502 not implemented")


class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True


HANDLERS = {"redis": Redis, "mqtt": Mqtt, "postgres": Postgres, "smtp": Smtp, "ssh": Ssh, "rtmp": Rtmp, "ftp": Ftp}


# ── clients ───────────────────────────────────────────────────────────────────────────────────────────────────────
def resp(*args):
    out = b"*%d\r\n" % len(args)
    for a in args:
        a = a.encode() if isinstance(a, str) else a
        out += b"$%d\r\n%s\r\n" % (len(a), a)
    return out


def client_redis(host, port):
    s = socket.create_connection((host, port), timeout=10)
    f = s.makefile("rwb")
    result = {}
    for name, cmd in (("ping", resp("PING")), ("set", resp("SET", "k", "v" * 100000)), ("get", resp("GET", "k"))):
        f.write(cmd)
        f.flush()
        line = f.readline().rstrip(b"\r\n")
        if line.startswith(b"$") and line != b"$-1":
            body = f.read(int(line[1:]) + 2)[:-2]
            result[name] = f"bulk of {len(body)} bytes"
        else:
            result[name] = line.decode()
    return result


def client_mqtt(host, port):
    s = socket.create_connection((host, port), timeout=10)
    connect = b"\x00\x04MQTT\x04\x02\x00\x3c" + b"\x00\x06e2e-01"
    s.sendall(b"\x10" + bytes([len(connect)]) + connect)
    connack = recv_exact(s, 4)
    topic = b"sensors/1"
    body = struct.pack(">H", len(topic)) + topic + b"\x00\x2a" + b"22.5"
    s.sendall(b"\x32" + bytes([len(body)]) + body)  # PUBLISH, QoS 1, packet id 42
    puback = recv_exact(s, 4)
    s.sendall(b"\xc0\x00")
    pingresp = recv_exact(s, 2)
    s.sendall(b"\xe0\x00")
    return {"connack": connack.hex(), "puback": puback.hex(), "pingresp": pingresp.hex()}


def client_postgres(host, port):
    s = socket.create_connection((host, port), timeout=10)
    s.sendall(struct.pack(">II", 8, 80877103))  # SSLRequest, as psql with sslmode=prefer sends first
    answer = recv_exact(s, 1)
    params = b"user\x00alice\x00database\x00app\x00\x00"
    s.sendall(struct.pack(">II", 8 + len(params), 196608) + params)
    seen = b""
    while not seen.endswith(b"Z\x00\x00\x00\x05I"):
        seen += s.recv(4096)
    query = b"SELECT 1\x00"
    s.sendall(b"Q" + struct.pack(">I", 4 + len(query)) + query)
    rows = b""
    while not rows.endswith(b"Z\x00\x00\x00\x05I"):
        rows += s.recv(4096)
    s.sendall(b"X\x00\x00\x00\x04")
    return {"ssl_answer": answer.decode(), "auth_ok": b"R\x00\x00\x00\x08\x00\x00\x00\x00" in seen,
            "server_version": b"16.0" in seen, "row": b"hello alice" in rows}


def client_smtp(host, port):
    with smtplib.SMTP(host, port, timeout=10) as smtp:
        code, banner = smtp.ehlo("client.test")
        reply = smtp.sendmail("a@client.test", ["b@smtp.test"], "Subject: e2e\r\n\r\n" + "line\r\n" * 2000)
    return {"ehlo": code, "failed_recipients": reply}


def client_ssh(host, port):
    s = socket.create_connection((host, port), timeout=10)
    banner = b""
    while not banner.endswith(b"\n"):
        banner += s.recv(1)
    s.sendall(b"SSH-2.0-e2e_client\r\n")
    answer = b""
    while not answer.endswith(b"\n"):
        answer += s.recv(1)
    return {"banner": banner.decode().strip(), "answer": answer.decode().strip()}


def client_rtmp(host, port):
    s = socket.create_connection((host, port), timeout=10)
    c1 = struct.pack(">II", 0, 0) + os.urandom(1528)
    s.sendall(b"\x03" + c1)
    s0 = recv_exact(s, 1)
    s1 = recv_exact(s, 1536)
    s2 = recv_exact(s, 1536)
    s.sendall(s1)  # C2 echoes S1
    done = recv_exact(s, len(b"handshake-done"))
    return {"version": s0[0], "s2_echoes_c1": s2 == c1, "done": done.decode()}


def client_ftp(host, port):
    ftp = ftplib.FTP(timeout=10)
    welcome = ftp.connect(host, port)
    ftp.login("anonymous", "e2e@client.test")
    ftp.set_pasv(True)
    lines = []
    ftp.retrlines("LIST", lines.append)
    ftp.quit()
    return {"welcome": welcome, "listing": lines}


CLIENTS = {"redis": client_redis, "mqtt": client_mqtt, "postgres": client_postgres, "smtp": client_smtp,
           "ssh": client_ssh, "rtmp": client_rtmp, "ftp": client_ftp}

if __name__ == "__main__":
    if len(sys.argv) >= 4 and sys.argv[1] == "server" and sys.argv[2] in HANDLERS:
        Server((bind_address(), int(sys.argv[3])), HANDLERS[sys.argv[2]]).serve_forever()
    elif len(sys.argv) >= 5 and sys.argv[1] == "client" and sys.argv[2] in CLIENTS:
        print(json.dumps(CLIENTS[sys.argv[2]](sys.argv[3], int(sys.argv[4]))))
    else:
        sys.exit(__doc__)
