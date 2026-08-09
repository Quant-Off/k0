#!/bin/sh
# cargo runner: ELF -> raw Image 변환 후 QEMU virt 부팅
# ELF를 -kernel로 직접 넘기면 QEMU가 Linux 부트 프로토콜을 쓰지 않아
# x0에 DTB 주소가 전달되지 않기 떄문에 반드시 raw Image로 변환
set -eu

elf="$1"
shift

host="$(rustc -vV | sed -n 's/^host: //p')"
objcopy="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-objcopy"
img="${elf}.bin"

"$objcopy" -O binary "$elf" "$img"

exec qemu-system-aarch64 \
    -machine virt,gic-version=3 -cpu cortex-a72 -smp 1 -m 512M \
    -nographic -kernel "$img" "$@"
