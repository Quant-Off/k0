#!/bin/sh
# UEFI 로더 빌드: 커널 ELF 하나를 받아 그 커널을 내장한 로더(.efi)를 만듭니다.
#
# 로더는 커널을 스스로 빌드하지 않습니다. 부팅 매트릭스가 시험하는 바로 그
# 커널 바이너리를 내장하기 위해, 이미 빌드된 ELF를 raw Image로 바꿔 넘깁니다.
# 그래뉼은 Image 헤더에서 읽으므로 4K와 16K 커널에 같은 명령을 씁니다.
#
# 사용법: tools/build-loader.sh [커널 ELF]   (기본: 디버그 4K 커널)
# 결과:   <커널 ELF>.efi (경로를 표준 출력의 마지막 줄에 씀)
# 환경:   K0_LOADER_FEATURES  로더의 cargo feature(예: refuse-halt)
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
elf="${1:-$root/target/aarch64-unknown-none-softfloat/debug/k0-kernel}"
if [ ! -f "$elf" ]; then
    echo "build-loader: kernel ELF not found: $elf" >&2
    echo "build-loader: build it first with: cargo build -p k0-kernel --offline" >&2
    exit 2
fi
elf="$(cd "$(dirname "$elf")" && pwd)/$(basename "$elf")"

host="$(rustc -vV | sed -n 's/^host: //p')"
objcopy="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-objcopy"
img="${elf}.bin"
"$objcopy" -O binary "$elf" "$img"

cd "$root"
set -- build -p k0-loader --offline --target aarch64-unknown-uefi --target-dir target/loader
[ -z "${K0_LOADER_FEATURES:-}" ] || set -- "$@" --features "$K0_LOADER_FEATURES"
K0_KERNEL_IMAGE="$img" cargo "$@" >&2

# 변형마다 같은 자리에 빌드되므로 곧바로 커널 옆으로 옮겨 둠
cp target/loader/aarch64-unknown-uefi/debug/k0-loader.efi "${elf}.efi"
echo "${elf}.efi"
