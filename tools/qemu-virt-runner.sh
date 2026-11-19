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
#   K0_QEMU_EXTRA        QEMU에 더 넘길 인자(공백으로 나눔)
#   K0_IMAGE_TEXT_OFFSET Image 헤더의 text_offset을 이 값으로 바꾼 사본을 부팅
#                        (같은 이미지를 다른 물리 주소에 적재시키는 시험)
#   K0_IMAGE_DTB_POINTER 커널이 받는 DTB 포인터(x0)를 바꾼 사본을 부팅. end면
#                        주소 범위의 맨 끝(0xFFFFFFFFFFFFFFF8)이고, 아니면 그
#                        주소(16비트 조각 하나로 적을 수 있는 값). QEMU가 손대지
#                        않은 디바이스 트리를 -device loader로 그 주소에 실어
#                        부팅할 때도 씀(-dtb로 준 트리는 QEMU가 memory 노드를
#                        자기 것으로 바꿈)
#
# 인자가 .efi로 끝나면(tools/build-loader.sh가 만든 UEFI 로더) 펌웨어를 거쳐
# 부팅합니다. 이때 머신 기본값은 virt,gic-version=3,acpi=off 입니다(EDK2는
# ACPI가 꺼져 있을 때만 디바이스 트리를 넘김).
#   K0_QEMU_FIRMWARE     UEFI 펌웨어 이미지(기본: Homebrew QEMU의 EDK2, 없으면
#                        Debian의 /usr/share/AAVMF/AAVMF_CODE.fd)
#   K0_QEMU_ESP          값이 있으면 -kernel 대신 FAT 드라이브의
#                        EFI/BOOT/BOOTAA64.EFI로 부팅(이동식 매체 경로)
set -eu

elf="$1"
shift

machine="${K0_QEMU_MACHINE:-virt,gic-version=3}"

case "$elf" in
*.efi)
    machine="${K0_QEMU_MACHINE:-virt,gic-version=3,acpi=off}"
    fw="${K0_QEMU_FIRMWARE:-}"
    if [ -z "$fw" ]; then
        for c in "$(brew --prefix qemu 2>/dev/null || true)/share/qemu/edk2-aarch64-code.fd" /usr/share/AAVMF/AAVMF_CODE.fd; do
            if [ -f "$c" ]; then
                fw="$c"
                break
            fi
        done
    fi
    if [ -z "$fw" ]; then
        echo "runner: no UEFI firmware found, set K0_QEMU_FIRMWARE" >&2
        exit 2
    fi
    if [ -n "${K0_QEMU_ESP:-}" ]; then
        # 로더 옆의 디렉터리를 FAT 드라이브로 내보냄. 파일은 매번 덮어씀
        esp="${elf}.esp"
        mkdir -p "$esp/EFI/BOOT"
        cp "$elf" "$esp/EFI/BOOT/BOOTAA64.EFI"
        set -- -drive "file=fat:rw:$esp,format=raw,if=none,id=esp" -device virtio-blk-device,drive=esp "$@"
    else
        set -- -kernel "$elf" "$@"
    fi
    # 네트워크 장치를 빼서 펌웨어가 PXE를 시도하지 않게 함
    set -- -drive "if=pflash,format=raw,readonly=on,file=$fw" -net none "$@"
    ;;
*)
    host="$(rustc -vV | sed -n 's/^host: //p')"
    objcopy="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-objcopy"
    img="${elf}.bin"

    "$objcopy" -O binary "$elf" "$img"

    # 콘솔 드라이버 없이 빌드한 커널은 직렬 포트에 아무것도 쓰지 않음. 빈 화면을
    # 고장으로 오해하지 않게 한 줄 알림(이미지에 들어 있는 줄을 그대로 읽음)
    if grep -aqF 'k0: console drivers [none]' "$img"; then
        echo "runner: this kernel has no console driver and prints nothing here (build with --features console to see output)" >&2
    fi

    if [ -n "${K0_IMAGE_TEXT_OFFSET:-}" ] || [ -n "${K0_IMAGE_DTB_POINTER:-}" ]; then
        tag="${K0_IMAGE_TEXT_OFFSET:-}"
        [ -z "${K0_IMAGE_DTB_POINTER:-}" ] || tag="${tag:+$tag.}dtb-${K0_IMAGE_DTB_POINTER}"
        patched="${elf}.${tag}.bin"
        python3 - "$img" "$patched" "${K0_IMAGE_TEXT_OFFSET:-}" "${K0_IMAGE_DTB_POINTER:-}" <<'PATCH'
import struct, sys
data = bytearray(open(sys.argv[1], "rb").read())
if sys.argv[3]:
    data[8:16] = struct.pack("<Q", int(sys.argv[3], 0))
if sys.argv[4]:
    # 헤더의 첫 명령은 진입점으로 가는 분기이고 둘째 자리는 비어 있음. 첫 자리에
    # x0을 정하는 명령 하나를 넣고 분기를 한 칸 미룸
    if sys.argv[4] == "end":
        set_x0 = 0x928000E0  # movn x0, #7
    else:
        value = int(sys.argv[4], 0)
        chunks = [(hw, (value >> (16 * hw)) & 0xFFFF) for hw in range(4)]
        used = [c for c in chunks if c[1]]
        if value >> 64 or len(used) != 1:
            sys.exit("runner: K0_IMAGE_DTB_POINTER must be 'end' or a value with one 16-bit chunk set")
        set_x0 = 0xD2800000 | used[0][0] << 21 | used[0][1] << 5  # movz x0, #chunk, lsl #(16 * hw)
    code0, code1 = struct.unpack("<II", data[:8])
    if code0 >> 26 != 0b000101 or code1 != 0:
        sys.exit("runner: the image header does not start with a branch and an empty word")
    data[:8] = struct.pack("<II", set_x0, code0 - 1)
open(sys.argv[2], "wb").write(data)
PATCH
        img="$patched"
    fi

    set -- -kernel "$img" "$@"
    ;;
esac
[ -z "${K0_QEMU_MONITOR:-}" ] || set -- -monitor "unix:${K0_QEMU_MONITOR},server,nowait" "$@"
[ -z "${K0_QEMU_ACCEL:-}" ] || set -- -accel "$K0_QEMU_ACCEL" "$@"
[ -z "${K0_QEMU_MEM-512M}" ] || set -- -m "${K0_QEMU_MEM-512M}" "$@"
[ -z "${K0_QEMU_SMP-1}" ] || set -- -smp "${K0_QEMU_SMP-1}" "$@"
[ -z "${K0_QEMU_CPU-cortex-a72}" ] || set -- -cpu "${K0_QEMU_CPU-cortex-a72}" "$@"

# shellcheck disable=SC2086
exec qemu-system-aarch64 -machine "$machine" -nographic "$@" ${K0_QEMU_EXTRA:-}
