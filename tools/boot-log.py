#!/usr/bin/env python3
"""실행 중인 QEMU에서 커널의 부트 로그를 물리 메모리로 읽어 출력합니다.

콘솔이 붙기 전에 부팅을 거부했거나 지원하는 UART가 없는 구성에서는 직렬
출력이 없으므로, 커널이 메모리에 남긴 로그(K0_BOOT_LOG)가 유일한 기록입니다.
로그의 물리 주소는 이미지가 적재된 주소와 ELF의 심볼 주소로 계산합니다.

사용법: boot-log.py <kernel ELF> <적재 물리 주소> <QEMU 모니터 소켓>
"""
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time

IMAGE_VA = 0xFFFF_8000_0000_0000
MAGIC = 0x6B30_2D62_6F6F_746C
HEADER = 24
SIZE = 16 * 1024


def symbol(elf, name):
    host = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    host = [l.split(": ")[1] for l in host.splitlines() if l.startswith("host: ")][0]
    sysroot = subprocess.run(
        ["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True
    ).stdout.strip()
    nm = os.path.join(sysroot, "lib", "rustlib", host, "bin", "llvm-nm")
    for line in subprocess.run([nm, elf], capture_output=True, text=True, check=True).stdout.splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[2] == name:
            return int(parts[0], 16)
    sys.exit(f"boot-log: symbol {name} not found in {elf}")


def main():
    elf, load_pa, sock_path = sys.argv[1], int(sys.argv[2], 0), sys.argv[3]
    pa = load_pa + symbol(elf, "K0_BOOT_LOG") - IMAGE_VA
    want = HEADER + SIZE
    out = tempfile.NamedTemporaryFile(delete=False).name
    try:
        mon = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            mon.connect(sock_path)
        except OSError as e:
            sys.exit(f"boot-log: cannot reach the QEMU monitor at {sock_path}: {e}")
        mon.sendall(f'pmemsave {pa:#x} {want} "{out}"\n'.encode())
        # 모니터는 완료를 알리지 않으므로 파일이 다 찰 때까지 기다림
        deadline = time.monotonic() + 5
        while os.path.getsize(out) < want and time.monotonic() < deadline:
            time.sleep(0.05)
        mon.close()
        data = open(out, "rb").read()
    finally:
        if os.path.exists(out):
            os.unlink(out)
    if len(data) < HEADER:
        sys.exit("boot-log: memory dump failed")
    magic, length, _user = struct.unpack("<QQQ", data[:HEADER])
    if magic != MAGIC:
        sys.exit("boot-log: no boot log at the expected address")
    sys.stdout.write(data[HEADER : HEADER + min(length, SIZE)].decode("latin-1"))


if __name__ == "__main__":
    main()
