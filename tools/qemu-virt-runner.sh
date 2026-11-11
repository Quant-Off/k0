#!/bin/sh
# cargo runner: ELF -> raw Image 변환 후 QEMU 부팅
# ELF를 -kernel로 직접 넘기면 QEMU가 Linux 부트 프로토콜을 쓰지 않아
# x0에 DTB 주소가 전달되지 않기 때문에 반드시 raw Image로 변환
#
# 환경 변수로 머신 구성을 바꿉니다(부팅 매트릭스가 사용, 빈 값이면 옵션 생략)
#   K0_QEMU_MACHINE      기본 virt,gic-version=3
#   K0_QEMU_CPU          기본 cortex-a72
#   K0_QEMU_SMP          기본 1
#   K0_QEMU_MEM          기본 512M
#   K0_QEMU_ACCEL        기본 없음(TCG)
#   K0_QEMU_MONITOR      모니터용 유닉스 소켓 경로(부트 로그를 메모리에서 읽을 때)
#   K0_IMAGE_TEXT_OFFSET Image 헤더의 text_offset을 이 값으로 바꾼 사본을 부팅
#                        (같은 이미지를 다른 물리 주소에 적재시키는 시험)
set -eu

elf="$1"
shift

host="$(rustc -vV | sed -n 's/^host: //p')"
objcopy="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-objcopy"
img="${elf}.bin"

"$objcopy" -O binary "$elf" "$img"

if [ -n "${K0_IMAGE_TEXT_OFFSET:-}" ]; then
    patched="${elf}.${K0_IMAGE_TEXT_OFFSET}.bin"
    python3 - "$img" "$patched" "$K0_IMAGE_TEXT_OFFSET" <<'PY'
import struct, sys
data = bytearray(open(sys.argv[1], "rb").read())
data[8:16] = struct.pack("<Q", int(sys.argv[3], 0))
open(sys.argv[2], "wb").write(data)
PY
    img="$patched"
fi

set -- -kernel "$img" "$@"
[ -z "${K0_QEMU_MONITOR:-}" ] || set -- -monitor "unix:${K0_QEMU_MONITOR},server,nowait" "$@"
[ -z "${K0_QEMU_ACCEL:-}" ] || set -- -accel "$K0_QEMU_ACCEL" "$@"
[ -z "${K0_QEMU_MEM-512M}" ] || set -- -m "${K0_QEMU_MEM-512M}" "$@"
[ -z "${K0_QEMU_SMP-1}" ] || set -- -smp "${K0_QEMU_SMP-1}" "$@"
[ -z "${K0_QEMU_CPU-cortex-a72}" ] || set -- -cpu "${K0_QEMU_CPU-cortex-a72}" "$@"

exec qemu-system-aarch64 -machine "${K0_QEMU_MACHINE:-virt,gic-version=3}" -nographic "$@"
