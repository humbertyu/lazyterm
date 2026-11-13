"""Disposable loopback SSH/SFTP server; requires Paramiko in a test-only venv."""

import json
import os
from pathlib import Path
import select
import socket
import subprocess
import sys
import threading
import time

import paramiko


ROOT = Path(sys.argv[1]).resolve()
REMOTE = ROOT / "remote"
REMOTE.mkdir()
KEY = paramiko.RSAKey.generate(2048)
PASSWORDS = {"tester": "fixture-password", "jump-user": "jump-password"}
LOCK = threading.Lock()


def record(filename, value):
    with LOCK:
        with (ROOT / filename).open("a") as output:
            output.write(json.dumps(value) + "\n")


class Files(paramiko.SFTPServerInterface):
    def path(self, path):
        result = (REMOTE / path.lstrip("/")).resolve()
        result.relative_to(REMOTE)
        return result

    def canonicalize(self, path):
        try:
            relative = str(self.path(path).relative_to(REMOTE))
            return "/" + ("" if relative == "." else relative)
        except ValueError:
            return "/"

    def list_folder(self, path):
        try:
            entries = []
            for entry in self.path(path).iterdir():
                attributes = paramiko.SFTPAttributes.from_stat(entry.stat())
                attributes.filename = entry.name
                entries.append(attributes)
            return entries
        except (OSError, ValueError):
            return paramiko.SFTP_NO_SUCH_FILE

    def stat(self, path):
        try:
            return paramiko.SFTPAttributes.from_stat(self.path(path).stat())
        except (OSError, ValueError):
            return paramiko.SFTP_NO_SUCH_FILE

    lstat = stat

    def open(self, path, flags, attr):
        try:
            descriptor = os.open(self.path(path), flags, 0o600)
            mode = "r+b" if flags & os.O_RDWR else "wb" if flags & os.O_WRONLY else "rb"
            file = os.fdopen(descriptor, mode)
            handle = paramiko.SFTPHandle(flags)
            handle.readfile = file
            handle.writefile = file
            return handle
        except (OSError, ValueError):
            return paramiko.SFTP_FAILURE


class Server(paramiko.ServerInterface):
    def __init__(self):
        self.forwarded = {}

    def get_allowed_auths(self, username):
        return "password"

    def check_auth_password(self, username, password):
        accepted = PASSWORDS.get(username) == password
        record("auth.jsonl", {"user": username, "accepted": accepted})
        return paramiko.AUTH_SUCCESSFUL if accepted else paramiko.AUTH_FAILED

    def check_channel_request(self, kind, channel_id):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_pty_request(self, *args):
        return True

    def check_channel_exec_request(self, channel, command):
        def execute():
            record("commands.jsonl", command.decode())
            try:
                result = subprocess.run(
                    command.decode(), shell=True, cwd=REMOTE, capture_output=True, timeout=10
                )
                channel.sendall(result.stdout)
                channel.sendall_stderr(result.stderr)
                channel.send_exit_status(result.returncode)
            finally:
                channel.close()

        threading.Thread(target=execute, daemon=True).start()
        return True

    def check_channel_direct_tcpip_request(self, channel_id, origin, destination):
        if destination[0] not in ("localhost", "127.0.0.1") or destination[1] != PORT:
            return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED
        self.forwarded[channel_id] = destination
        return paramiko.OPEN_SUCCEEDED

    def check_port_forward_request(self, address, port):
        # This fixture verifies authentication, not forwarding policy.
        return port


def forward(channel, destination):
    with socket.create_connection(destination) as stream:
        try:
            while True:
                ready, _, _ = select.select([channel, stream], [], [], 0.5)
                for source in ready:
                    data = source.recv(65536)
                    if not data:
                        return
                    (stream if source is channel else channel).sendall(data)
        finally:
            channel.close()


def serve(stream):
    transport = paramiko.Transport(stream)
    transport.add_server_key(KEY)
    transport.set_subsystem_handler("sftp", paramiko.SFTPServer, Files)
    server = Server()
    channels = []
    try:
        transport.start_server(server=server)
        while transport.is_active():
            channel = transport.accept(timeout=0.2)
            if channel is None:
                continue
            channels.append(channel)
            destination = server.forwarded.get(channel.get_id())
            if destination:
                threading.Thread(target=forward, args=(channel, destination), daemon=True).start()
    except (EOFError, OSError, paramiko.SSHException):
        pass
    finally:
        transport.close()


listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen()
PORT = listener.getsockname()[1]
(ROOT / "known_hosts").write_text(
    "".join(
        f"[{host}]:{PORT} {KEY.get_name()} {KEY.get_base64()}\n"
        for host in ("127.0.0.1", "localhost")
    )
)
(ROOT / "config").write_text(
    f"Host web\n HostName 127.0.0.1\n User tester\n Port {PORT}\n"
    f"Host jump\n HostName 127.0.0.1\n User jump-user\n Port {PORT}\n"
    f"Host via-jump\n HostName localhost\n User tester\n Port {PORT}\n ProxyJump jump\n"
    f"Host *\n UserKnownHostsFile {ROOT / 'known_hosts'}\n StrictHostKeyChecking yes\n"
    " PubkeyAuthentication no\n KbdInteractiveAuthentication no\n"
    " PreferredAuthentications password\n LogLevel ERROR\n"
)
(ROOT / "ready").write_text(str(PORT))
while True:
    stream, _ = listener.accept()
    threading.Thread(target=serve, args=(stream,), daemon=True).start()
