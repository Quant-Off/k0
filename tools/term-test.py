#!/usr/bin/env python3
"""운영자 터미널 시험: 이미지를 QEMU로 부팅해 직렬 포트로 명령을 넣고 답을 대조합니다.

사용법: tools/term-test.py <이미지> [--vault ed25519|none|absent] [--seed rng-seed|rng-seed+rndr]

  --vault ed25519  볼트와 Ed25519가 든 이미지(기본)
  --vault none     볼트는 있고 알고리즘이 없는 이미지
  --vault absent   볼트가 없는 이미지
  --seed           `vault status`의 seed 줄에 기대하는 값(기본 rng-seed)

환경 변수
  K0_TERM_VAULT    --vault의 기본값
  K0_TERM_SEED     --seed의 기본값
  K0_TERM_LOG      직렬 출력 기록 파일(기본 target/term-test.log)
  K0_TERM_TIMEOUT  프롬프트가 뜰 때까지 기다리는 초(기본 120)
  K0_QEMU_*        tools/qemu-virt-runner.sh가 읽는 머신 구성
  K0_BOOT_*        부팅 구간의 판정에 그대로 전달(tools/qemu-boot-test.sh).
                   K0_BOOT_INPUT, K0_BOOT_SERVICE, K0_BOOT_FAULTS는 이 스크립트가
                   이미지의 종류에 맞게 정함

직렬 포트는 QEMU의 표준 입출력에 파이프로 붙입니다. 모니터를 유닉스 소켓으로
따로 빼야 표준 입출력이 직렬 전용이 되어 입력 바이트가 그대로 전달됩니다.
부팅 구간(프롬프트 전)의 판정은 tools/qemu-boot-test.sh의 판정 전용 모드에
맡기고, 이 스크립트는 그 뒤의 대화만 봅니다. 표준 라이브러리만 씁니다.
"""
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

PROMPT = b"k0> "
BANNER = b"K0 operator console, type 'help'\n"
# 누구나 아는 시드의 공개키. 서비스의 키가 이 값이면 진짜 부트 시드가 볼트에
# 닿지 않은 것임(RFC 8032 첫 시험 벡터, 그리고 전부 0인 시드)
KNOWN_KEYS = (
    b"d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
    b"3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29",
)
FAIL_RE = re.compile(rb"root: FAIL|k0: EXCEPTION|k0: PANIC|k0: FATAL|k0: UNEXPECTED|k0: task fault|k0: all tasks|k0: tick ")


class Failure(Exception):
    pass


class Session:
    """QEMU 하나와 그 직렬 포트를 쥐는 객체입니다."""

    def __init__(self, root, image, log_path, boot_timeout):
        self.root = root
        self.log_path = log_path
        self.boot_timeout = boot_timeout
        self.buf = bytearray()
        self.pos = 0
        self.lock = threading.Condition()
        self.tmp = tempfile.mkdtemp(prefix="k0-term.", dir="/tmp")
        self.sock = os.path.join(self.tmp, "monitor")
        env = dict(os.environ, K0_QEMU_MONITOR=self.sock)
        self.err = open(log_path + ".qemu", "wb")
        self.proc = subprocess.Popen(
            [os.path.join(root, "tools/qemu-virt-runner.sh"), image],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.err,
            env=env,
            cwd=root,
        )
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        with open(self.log_path, "wb") as log:
            while True:
                data = os.read(self.proc.stdout.fileno(), 65536)
                if not data:
                    break
                log.write(data)
                log.flush()
                # 커널은 콘솔에서 줄바꿈을 CR LF로 내보냄
                data = data.replace(b"\r", b"")
                with self.lock:
                    self.buf += data
                    self.lock.notify_all()

    def close(self):
        try:
            s = socket.socket(socket.AF_UNIX)
            s.settimeout(2)
            s.connect(self.sock)
            s.sendall(b"quit\n")
            s.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.err.close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def monitor(self, command):
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(2)
        s.connect(self.sock)
        s.sendall(command.encode() + b"\n")
        time.sleep(0.3)
        s.close()

    def send(self, data):
        self.proc.stdin.write(data)
        self.proc.stdin.flush()

    def tail(self):
        with self.lock:
            return bytes(self.buf[max(self.pos - 200, 0):][-600:]).decode(errors="replace")

    def expect(self, want, timeout=20, what=None):
        """아직 읽지 않은 출력이 정확히 `want`로 시작할 때까지 기다립니다."""
        deadline = time.time() + timeout
        with self.lock:
            while True:
                have = bytes(self.buf[self.pos:self.pos + len(want)])
                if have == want:
                    self.pos += len(want)
                    return
                if not want.startswith(have):
                    raise Failure("%s: expected %r, got %r" % (what or "output", want, bytes(self.buf[self.pos:self.pos + len(want) + 80])))
                left = deadline - time.time()
                if left <= 0 or self.proc.poll() is not None:
                    raise Failure("%s: timed out waiting for %r, got %r" % (what or "output", want, have))
                self.lock.wait(min(left, 1))

    def wait_for(self, marker, timeout):
        """`marker`가 나올 때까지 기다리고 그 앞까지의 출력을 돌려줍니다."""
        deadline = time.time() + timeout
        with self.lock:
            while True:
                at = self.buf.find(marker, self.pos)
                if at >= 0:
                    before = bytes(self.buf[self.pos:at])
                    self.pos = at + len(marker)
                    return before
                left = deadline - time.time()
                if left <= 0 or self.proc.poll() is not None:
                    raise Failure("timed out waiting for %r, last output %r" % (marker, bytes(self.buf[-300:])))
                self.lock.wait(min(left, 1))

    def run(self, line, reply, typed=None, echo=None, timeout=20):
        """한 줄을 입력하고, 되돌아온 글자와 답과 다음 프롬프트를 대조합니다."""
        keys = line.encode() + b"\r" if typed is None else typed
        self.send(keys)
        shown = (line.encode() if echo is None else echo) + b"\n"
        self.expect(shown + reply.encode() + PROMPT, timeout, what=repr(line))


def boot_judge(root, image, log_path, boot, vault):
    """프롬프트 전의 출력을 부팅 시험의 판정 전용 모드로 넘깁니다."""
    path = log_path + ".boot"
    with open(path, "wb") as f:
        f.write(boot)
    # 터미널과 똑같이 배선한 적대적 태스크 둘이 폴트로 끝나므로 폴트가 둘 더 있음
    faults = 8 if vault == "absent" else 10
    env = dict(
        os.environ,
        K0_BOOT_JUDGE=path,
        K0_BOOT_LOG=path,
        K0_BOOT_INPUT="on",
        K0_BOOT_SERVICE="plain" if vault == "absent" else "vault",
        K0_BOOT_FAULTS=str(faults),
    )
    r = subprocess.run([os.path.join(root, "tools/qemu-boot-test.sh"), image], env=env, cwd=root, capture_output=True)
    if r.returncode != 0:
        raise Failure("boot part: " + (r.stdout + r.stderr).decode(errors="replace").strip())


def wiring(boot, vault):
    """터미널과 똑같이 배선한 적대적 태스크가 남긴 줄을 바이트까지 대조합니다."""
    # 여섯 바이트(ESC, BS, CR, NUL, DEL, 0x9B) 가운데 백스페이스만 그대로 나감
    if b"root: terminal console filter [?\x08????]\n" not in boot:
        raise Failure("the output filter let something other than backspace through")
    tail = boot[boot.index(b"root: terminal wiring holds\n"):]
    fars = re.findall(rb"k0: task fault dabort-lower \(esr 0x[0-9a-f]+ elr 0x[0-9a-f]+ far 0x([0-9a-f]+)\)", tail)
    secret = re.search(rb"root: secret at 0x([0-9a-f]+)", boot)
    if len(fars) != 2 or secret is None:
        raise Failure("expected two faults after the wiring line, found %r" % fars)
    # 첫 폴트는 읽기 전용 응답 프레임에 쓴 것, 둘째는 루트의 데이터를 읽은 것
    if vault != "absent" and int(fars[0], 16) != 0x5C0000:
        raise Failure("the first hostile fault is not the write to the response frame: %r" % fars[0])
    if int(fars[1], 16) != int(secret.group(1), 16):
        raise Failure("the second hostile fault is not the read of root data: %r" % fars[1])


def session(s, vault, seed):
    full_help = (
        "help\nvault status\nvault pubkey\nvault keygen <alg>     alg: %s\nvault zeroize confirm\nok\n"
        % ("ed25519" if vault == "ed25519" else "none")
    )
    if vault == "absent":
        s.run("help", "help\nok\n")
        s.run("vault status", "err no-vault\n")
        s.run("vault zeroize confirm", "err no-vault\n")
        s.run("status", "err unknown-command\n")
        return
    if vault == "none":
        # 알고리즘이 없는 이미지의 볼트는 시드를 받지 않음
        fresh = "state=fresh\nalg=none\nselftest=passed\nseed=none\nvalidated=no\nok\n"
    else:
        fresh = "state=seeded\nalg=none\nselftest=passed\nseed=%s\nvalidated=no\nok\n" % seed
    s.run("help", full_help)
    s.run("vault status", fresh)
    s.run("vault pubkey", "err no-key\n")
    # 오타를 지우고 고쳐 쓰기. 지울 때마다 BS, 공백, BS가 돌아옴
    s.run("vault status", fresh, typed=b"vault stauts\x08\x08\x7ftus\r", echo=b"vault stauts\x08 \x08\x08 \x08\x08 \x08tus")
    # 줄 취소는 실행하지 않고 새 프롬프트를 줌
    s.send(b"vault zeroize confirm\x03")
    s.expect(b"vault zeroize confirm^C\n" + PROMPT, what="cancel")
    s.send(b"vault zeroize confirm\x15")
    s.expect(b"vault zeroize confirm^U\n" + PROMPT, what="cancel")
    # 화살표 키는 화면에도 명령에도 들어가지 않음
    s.run("help", full_help, typed=b"he\x1b[A\x1b[1;5Clp\x1bOP\r")
    # CR LF는 줄 하나이고, 빈 줄은 프롬프트만 냄
    s.run("help", full_help, typed=b"help\r\n")
    s.run("", "", typed=b"\n")
    s.run("sign", "err unknown-command\n")
    s.run("vault sign", "err unknown-command\n")
    s.run("vault keygen", "err bad-argument\n")
    s.run("vault keygen rsa", "err unsupported-algorithm\n")
    s.run("x" * 64, "err unknown-command\n")
    s.send(b"y" * 65 + b"zzz\r")
    s.expect(b"y" * 64 + b"\nerr line-too-long\n" + PROMPT, what="long line")
    if vault == "none":
        s.run("vault keygen ed25519", "err unsupported-algorithm\n")
        s.run("vault status", fresh)
    else:
        s.run("vault keygen ed25519", "state=ready\nalg=ed25519\nok\n", timeout=60)
        s.run("vault keygen ed25519", "err key-exists\n")
        s.send(b"vault pubkey\r")
        s.expect(b"vault pubkey\npubkey=", what="pubkey")
        body = s.wait_for(b"\nok\n" + PROMPT, 20)
        if not re.fullmatch(rb"[0-9a-f]{32}\n {7}[0-9a-f]{32}", body):
            raise Failure("public key format: %r" % body)
        if body.replace(b"\n       ", b"") in KNOWN_KEYS:
            raise Failure("the service key comes from a publicly known seed: %r" % body)
        ready = "state=ready\nalg=ed25519\nselftest=passed\nseed=%s\nvalidated=no\nok\n" % seed
        s.run("vault status", ready)
        s.run("vault zeroize", "hint=type 'vault zeroize confirm' to erase the key\nerr confirm-required\n")
        s.run("vault status", ready)
    # 줄 끝 없는 홍수: 터미널은 아무것도 돌려보내지 않고, 커널은 계속 답함
    s.send(b"A" * 64)
    s.expect(b"A" * 64, what="flood head")
    for _ in range(50):
        s.send(b"B" * 4000)
    s.send(b"\r")
    s.expect(b"\nerr line-too-long\n" + PROMPT, timeout=120, what="flood")
    # 줄 끝이 섞인 홍수: 줄마다 답이 나오고 끝난 뒤에도 정상임
    burst = b"q\r" * 2000
    s.send(burst)
    for _ in range(2000):
        s.expect(b"q\nerr unknown-command\n" + PROMPT, timeout=120, what="burst")
    # 직렬 선의 break는 커널이 유실로 알리고, 터미널은 그 줄을 버림
    s.send(b"vault zeroize con")
    s.expect(b"vault zeroize con", what="partial line")
    s.monitor("chardev-send-break serial0")
    s.send(b"firm\r")
    s.expect(b"\nerr input-lost\n" + PROMPT, what="break")
    if vault == "ed25519":
        s.run("vault status", ready)
        s.run("vault zeroize confirm", "state=zeroized\nok\n")
        s.run("vault pubkey", "err zeroized\n")
        s.run("vault keygen ed25519", "err zeroized\n")
        s.run("vault status", "state=zeroized\nalg=none\nselftest=passed\nseed=none\nvalidated=no\nok\n")
    else:
        s.run("vault zeroize confirm", "state=zeroized\nok\n")
        # 알고리즘이 없는 이미지는 상태를 보기 전에 알고리즘부터 거절함
        s.run("vault keygen ed25519", "err unsupported-algorithm\n")


def main():
    args = sys.argv[1:]
    vault, seed = os.environ.get("K0_TERM_VAULT", "ed25519"), os.environ.get("K0_TERM_SEED", "rng-seed")
    image = None
    while args:
        a = args.pop(0)
        if a == "--vault":
            vault = args.pop(0)
        elif a == "--seed":
            seed = args.pop(0)
        else:
            image = a
    if image is None or vault not in ("ed25519", "none", "absent"):
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    log_path = os.environ.get("K0_TERM_LOG", os.path.join(root, "target/term-test.log"))
    boot_timeout = int(os.environ.get("K0_TERM_TIMEOUT", "120"))
    s = Session(root, os.path.abspath(image), log_path, boot_timeout)
    try:
        boot = s.wait_for(BANNER, boot_timeout)
        s.expect(PROMPT, what="first prompt")
        if not boot.endswith(b"root: service started\n"):
            raise Failure("the banner must follow the service line, got %r" % boot[-120:])
        boot_judge(root, os.path.abspath(image), log_path, boot, vault)
        wiring(boot, vault)
        start = s.pos
        session(s, vault, seed)
        with s.lock:
            talk = bytes(s.buf[start:])
        bad = FAIL_RE.search(talk)
        if bad:
            raise Failure("kernel line during the session: %r" % talk[bad.start(): bad.start() + 80])
    except Failure as e:
        print("term-test: FAIL %s" % e, file=sys.stderr)
        print("term-test: last output: %r" % s.tail(), file=sys.stderr)
        print("term-test: log in %s" % log_path, file=sys.stderr)
        return 1
    finally:
        s.close()
    print("term-test: pass (vault %s)" % vault)
    return 0


if __name__ == "__main__":
    sys.exit(main())
