#!/usr/bin/env python3
"""UEFI 로더(PE)를 실행하지 않고 검사합니다.

부팅 시험이 보여 주지 못하는 두 가지를 봅니다.
1. Secure Boot와 메모리 보호가 있는 펌웨어가 받아들이는 형태인지:
   ARM64 EFI 응용 프로그램, NX_COMPAT, 쓰기와 실행이 함께 되는 섹션 없음.
2. 커널로 넘어가기 전의 캐시 정리와 MMU 끄기 명령이 있고 순서가 맞는지.
   QEMU의 TCG에는 캐시가 없어 이 명령이 빠져도 부팅이 되므로, 자동으로는
   여기서만 잡을 수 있습니다. 명령이 실제 하드웨어에서 옳게 동작하는지까지
   보여 주지는 못합니다.

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
    for line in out(tool("llvm-objdump"), "-d", efi).splitlines():
        m = re.match(r"\s*[0-9a-f]+:\s+[0-9a-f]{8}\s+(.*)$", line)
        if m:
            code.append(" ".join(m.group(1).split()))

    def where(text):
        return [i for i, c in enumerate(code) if c == text]

    if not any(c.startswith("dc cvac,") for c in code):
        problems.append("no data cache clean (dc cvac)")
    flush = where("ic ialluis")
    if len(flush) != 1 or code[flush[0] + 1 : flush[0] + 3] != ["dsb ish", "isb"]:
        problems.append("ic ialluis must appear once, followed by dsb ish and isb")
    # 트램펄린의 두 갈래에 하나씩, 그리고 캐시 정리 뒤에 하나
    if len(where("dsb sy")) < 3:
        problems.append("dsb sy is missing after the data cache clean")
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
    print(f"pe-check: ok ({len(sections)} sections, none writable and executable, handoff sequence present)")


if __name__ == "__main__":
    main()
