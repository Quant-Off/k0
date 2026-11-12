#!/usr/bin/env python3
"""UEFI 로더(PE)를 실행하지 않고 검사합니다.

부팅 시험이 보여 주지 못하는 두 가지를 봅니다.
1. Secure Boot와 메모리 보호가 있는 펌웨어가 받아들이는 형태인지:
   ARM64 EFI 응용 프로그램, NX_COMPAT, 쓰기와 실행이 함께 되는 섹션 없음.
2. 커널로 넘어가기 전에 블록, 작업 공간, 트램펄린의 캐시를 비우는 호출이 모두 있고,
   그 뒤에 명령 캐시를 버리고, MMU를 끄는 명령의 순서가 맞는지.
   QEMU의 TCG에는 캐시가 없어 이 명령이 빠져도 부팅이 되므로, 자동으로는
   여기서만 잡을 수 있습니다. 비우는 범위가 옳은지와 실제 하드웨어에서 옳게
   동작하는지까지 보여 주지는 못합니다.

사용법: pe-check.py <로더 .efi>
"""
import os
import re
import subprocess
import sys


def tool(name):
    run = lambda *a: subprocess.run(a, capture_output=True, text=True, check=True).stdout
    host = [l.split(": ")[1] for l in run("rustc", "-vV").splitlines() if l.startswith("host: ")][0]
    return os.path.join(run("rustc", "--print", "sysroot").strip(), "lib", "rustlib", host, "bin", name)


def out(*cmd):
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout


def main():
    efi = sys.argv[1]
    problems = []

    headers = out(tool("llvm-readobj"), "--file-headers", "--sections", efi)
    for need in ("IMAGE_FILE_MACHINE_ARM64", "IMAGE_SUBSYSTEM_EFI_APPLICATION", "IMAGE_DLL_CHARACTERISTICS_NX_COMPAT"):
        if need not in headers:
            problems.append(f"missing {need}")
    sections = re.findall(r"Section \{(.*?)\n  \}", headers, re.S)
    if not sections:
        problems.append("no sections found")
    for body in sections:
        name = re.search(r"Name: (\S+)", body).group(1)
        if "IMAGE_SCN_MEM_WRITE" in body and "IMAGE_SCN_MEM_EXECUTE" in body:
            problems.append(f"section {name} is writable and executable")

    # 명령만 남김: "주소: 인코딩 <탭> 니모닉 <탭> 피연산자"
    code = []
    addr = []
    for line in out(tool("llvm-objdump"), "-d", efi).splitlines():
        m = re.match(r"\s*([0-9a-f]+):\s+[0-9a-f]{8}\s+(.*)$", line)
        if m:
            addr.append(int(m.group(1), 16))
            code.append(" ".join(m.group(2).split()))

    def where(text):
        return [i for i, c in enumerate(code) if c == text]

    def call_target(c):
        m = re.match(r"bl 0x([0-9a-f]+)", c)
        return int(m.group(1), 16) if m else None

    # 캐시를 비우는 함수는 하나뿐이고(dc civac가 든 함수), 커널로 넘어가는 함수는
    # 명령 캐시를 버리기 전에 그 함수를 세 번(블록, 작업 공간, 트램펄린) 불러야 함
    flush_at = [i for i, c in enumerate(code) if c.startswith("dc civac,")]
    targets = sorted({t for t in map(call_target, code) if t is not None})
    if len(flush_at) != 1:
        problems.append("expected exactly one dc civac (the cache flush loop)")
    else:
        entry = max([t for t in targets if t <= addr[flush_at[0]]], default=None)
        # 컴파일러가 루프 본문을 함수의 어디에 놓을지는 정해져 있지 않으므로
        # 함수의 처음부터 루프 조금 뒤까지에서 배리어를 찾음
        start = addr.index(entry) if entry in addr else flush_at[0]
        if "dsb sy" not in code[start : flush_at[0] + 64]:
            problems.append("dsb sy is missing from the cache flush function")
        flush = where("ic ialluis")
        if len(flush) != 1 or code[flush[0] + 1 : flush[0] + 3] != ["dsb ish", "isb"]:
            problems.append("ic ialluis must appear once, followed by dsb ish and isb")
        else:
            calls = 0
            for c in reversed(code[: flush[0]]):
                if c == "ret":
                    break
                calls += call_target(c) == entry
            if entry is None or calls < 3:
                problems.append(f"the block, the work area and the trampoline must all be flushed before ic ialluis (found {calls} calls)")
    for el in ("EL1", "EL2"):
        write = where(f"msr SCTLR_{el}, x4")
        if len(write) != 1:
            problems.append(f"expected one write to SCTLR_{el}")
            continue
        i = write[0]
        want = [
            f"mrs x4, SCTLR_{el}",
            "and x4, x4, #0xfffffffffffffffe",
            "and x4, x4, #0xfffffffffffffffb",
            "dsb sy",
            "isb",
            f"msr SCTLR_{el}, x4",
            "isb",
        ]
        if code[i - 5 : i + 2] != want:
            problems.append(f"MMU-off sequence for {el} differs: {code[i - 5 : i + 2]}")
    jump = where("br x9")
    if not jump or code[jump[-1] - 3 : jump[-1]] != ["mov x1, xzr", "mov x2, xzr", "mov x3, xzr"]:
        problems.append("the jump to the kernel must clear x1 to x3")

    if problems:
        for p in problems:
            print(f"pe-check: {p}")
        sys.exit(1)
    print(f"pe-check: ok ({len(sections)} sections, none writable and executable, cache flush and MMU-off sequence present)")


if __name__ == "__main__":
    main()
