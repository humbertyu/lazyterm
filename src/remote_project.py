"""Connection-scoped POSIX project supervisor, sent over SSH without installation."""
import os
import select
import signal
import subprocess
import sys
import time


def supervise(directory, command):
    os.setsid()
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)

    def reset_signals():
        signal.signal(signal.SIGTERM, signal.SIG_DFL)
        signal.signal(signal.SIGHUP, signal.SIG_DFL)

    child = subprocess.Popen(
        ["sh", "-c", command],
        cwd=os.path.expanduser(directory),
        stdin=subprocess.DEVNULL,
        preexec_fn=reset_signals,
    )
    heartbeat = time.monotonic()
    result = 1
    try:
        while True:
            result = child.poll()
            if result is not None:
                break
            readable, _, _ = select.select([0], [], [], 0.2)
            if readable:
                if not os.read(0, 4096):
                    result = 143
                    break
                heartbeat = time.monotonic()
            if time.monotonic() - heartbeat > 20:
                result = 143
                break
    finally:
        # This living group leader reserves the process group until cleanup ends.
        os.killpg(os.getpgrp(), signal.SIGTERM)
        deadline = time.monotonic() + 2
        while True:
            child.poll()
            processes = subprocess.Popen(
                ["ps", "-axo", "pid=,pgid=,stat="],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            output, _ = processes.communicate()
            if processes.returncode:
                raise RuntimeError("Could not inspect the remote process group")
            members = [
                fields for row in output.splitlines()
                if len(fields := row.split()) >= 3
                and fields[1] == str(os.getpgrp())
                and fields[0] not in (str(os.getpid()), str(processes.pid))
                and not fields[2].startswith("Z")
            ]
            if not members:
                break
            if time.monotonic() >= deadline:
                os.killpg(os.getpgrp(), signal.SIGKILL)
            time.sleep(0.05)
    return result if result >= 0 else 128 - result


# The forked guard becomes a new session leader even if sshd's shell was one.
pid = os.fork()
if pid == 0:
    try:
        os._exit(supervise(sys.argv[1], sys.argv[2]))
    except BaseException as error:
        print(f"Remote Project supervisor failed: {error}", file=sys.stderr, flush=True)
        os._exit(1)
_, status = os.waitpid(pid, 0)
sys.exit(os.waitstatus_to_exitcode(status) if os.WIFEXITED(status) else 137)
