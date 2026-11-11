#!/bin/sh
# 부팅 매트릭스: 지원한다고 말하는 구성마다 실제로 부팅해 확인합니다.
#
# 각 행은 tools/qemu-boot-test.sh(자가 검증 전체 통과)이거나, 거부해야 하는
# 구성에서 커널이 메모리에 남긴 부트 로그의 사유를 확인하는 것입니다.
# 사용법: tools/boot-matrix.sh [행 이름 ...]   (인자가 없으면 전부)
# 환경:   K0_MATRIX_HVF=1 이면 HVF 행도 실행(Apple Silicon의 macOS 전용)
set -u

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
t="target/aarch64-unknown-none-softfloat"
img4="$root/$t/debug/k0-kernel"
img16="$root/target/g16/aarch64-unknown-none-softfloat/debug/k0-kernel"
imgpin="$root/target/pin/aarch64-unknown-none-softfloat/debug/k0-kernel"

cargo build -p k0-kernel --offline || exit 2
cargo build -p k0-kernel --offline --no-default-features --features granule-16k --target-dir target/g16 || exit 2
# UEFI 로더: 위에서 빌드한 바로 그 커널을 내장함
efi4="$(tools/build-loader.sh "$img4")" || exit 2
efi16="$(tools/build-loader.sh "$img16")" || exit 2

failed=""
ran=0
want() {
    [ "$#" -eq 1 ] && return 0
    name="$1"; shift
    for w in "$@"; do [ "$w" = "$name" ] && return 0; done
    return 1
}

# 실패한 행의 기록을 그대로 출력합니다. CI에서는 컨테이너가 사라지면 파일을
# 볼 수 없으므로, 한 번의 실행으로 원인을 알 수 있어야 합니다.
show() {
    for f in "$@"; do
        [ -s "$f" ] || continue
        echo "---- $f"
        tail -n 60 "$f"
    done
    echo "----"
}

# 통과 행: 이름, ELF, 그 뒤는 환경 변수 대입들
pass() {
    name="$1"; elf="$2"; shift 2
    want "$name" $only || return 0
    ran=$((ran + 1))
    if env "$@" K0_BOOT_LOG="$root/target/matrix-$name.log" tools/qemu-boot-test.sh "$elf" > "target/matrix-$name.out" 2>&1; then
        echo "matrix: $name pass"
    else
        echo "matrix: $name FAIL"
        show "target/matrix-$name.out"
        failed="$failed $name"
    fi
}

# 거부 행: 이름, ELF, 적재 물리 주소, 로그에 있어야 할 사유, 그 뒤는 환경 변수 대입들
# 사유가 로그에 나타날 때까지 1초 간격으로 읽습니다(느린 호스트에서도 기다림).
refuse() {
    name="$1"; elf="$2"; load="$3"; reason="$4"; shift 4
    want "$name" $only || return 0
    ran=$((ran + 1))
    sock="/tmp/k0-matrix-$$.sock"
    rm -f "$sock"
    env "$@" K0_QEMU_MONITOR="$sock" tools/qemu-virt-runner.sh "$elf" > "target/matrix-$name.serial" 2>&1 < /dev/null &
    pid=$!
    waited=0
    seen=no
    while [ "$waited" -lt "${K0_MATRIX_REFUSE_WAIT:-60}" ]; do
        sleep 1
        waited=$((waited + 1))
        tools/boot-log.py "$elf" "$load" "$sock" > "target/matrix-$name.log" 2> "target/matrix-$name.out" || true
        if grep -qF -- "$reason" "target/matrix-$name.log"; then
            seen=yes
            break
        fi
        kill -0 "$pid" 2>/dev/null || break
    done
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    rm -f "$sock"
    if [ "$seen" = yes ] && ! grep -q 'k0: entering root task' "target/matrix-$name.log"; then
        echo "matrix: $name pass (refused: $reason)"
    else
        echo "matrix: $name FAIL (no refusal after ${waited}s)"
        show "target/matrix-$name.log" "target/matrix-$name.out" "target/matrix-$name.serial"
        failed="$failed $name"
    fi
}

# 직렬 로그로 판정하는 거부 행: 이름, 로더, 로그에 있어야 할 사유, 그 뒤는 환경
# 변수 대입들. 로더는 부트 서비스가 살아 있을 때 거부하므로 사유가 펌웨어
# 콘솔(직렬 포트)에 나옵니다. 커널의 첫 줄이 나오면 거부가 아닙니다.
refuse_serial() {
    name="$1"; elf="$2"; reason="$3"; shift 3
    want "$name" $only || return 0
    ran=$((ran + 1))
    env "$@" tools/qemu-virt-runner.sh "$elf" > "target/matrix-$name.serial" 2>&1 < /dev/null &
    pid=$!
    waited=0
    seen=no
    while [ "$waited" -lt "${K0_MATRIX_REFUSE_WAIT:-60}" ]; do
        sleep 1
        waited=$((waited + 1))
        if grep -aqF -- "$reason" "target/matrix-$name.serial"; then
            seen=yes
            break
        fi
        kill -0 "$pid" 2>/dev/null || break
    done
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    if [ "$seen" = yes ] && ! grep -aq 'k0: entry phase' "target/matrix-$name.serial"; then
        echo "matrix: $name pass (refused: $reason)"
    else
        echo "matrix: $name FAIL (no refusal after ${waited}s)"
        show "target/matrix-$name.serial"
        failed="$failed $name"
    fi
}

only="$*"

# 물리 타이머를 쓰지 않음: 하이퍼바이저 게스트에는 가상 타이머만 보장되므로
# 이미지에 물리 타이머 레지스터 접근이 하나도 없어야 함
if want no-physical-timer $only; then
    ran=$((ran + 1))
    host="$(rustc -vV | sed -n 's/^host: //p')"
    objdump="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-objdump"
    if "$objdump" -d "$img4" | grep -qiE 'cntp_(ctl|tval|cval)_el0|cntpct_el0'; then
        echo "matrix: no-physical-timer FAIL (the image touches the physical timer)"
        failed="$failed no-physical-timer"
    else
        echo "matrix: no-physical-timer pass"
    fi
fi

# 인터럽트 컨트롤러와 진입 EL
pass gic3-el1        "$img4"
pass gic3-el1-a53    "$img4" K0_QEMU_CPU=cortex-a53
pass gic3-el1-max    "$img4" K0_QEMU_CPU=max K0_BOOT_REQUIRE="rndr=on|k0: pac=on bti=present pan=on"
pass gic3-el2        "$img4" K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on
pass gic3-el2-max    "$img4" K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on K0_QEMU_CPU=max
pass gic2-el1        "$img4" K0_QEMU_MACHINE=virt,gic-version=2
pass gic2-el2        "$img4" K0_QEMU_MACHINE=virt,gic-version=2,virtualization=on
pass gic3-secure     "$img4" K0_QEMU_MACHINE=virt,gic-version=3,secure=on
pass gic2-secure     "$img4" K0_QEMU_MACHINE=virt,gic-version=2,secure=on
pass smp4            "$img4" K0_QEMU_SMP=4

# 그래뉼
pass 16k-max         "$img16" K0_QEMU_CPU=max
refuse 16k-on-a72    "$img16" 0x40080000 "k0: mmu enable failed"

# 같은 이미지를 다른 물리 주소에 적재 (헤더의 text_offset만 바꿈)
pass load-2m         "$img4" K0_IMAGE_TEXT_OFFSET=0x200000 K0_BOOT_REUSE=skip
pass load-256m       "$img4" K0_IMAGE_TEXT_OFFSET=0x10000000 K0_BOOT_REUSE=skip
pass load-default    "$img4" K0_IMAGE_TEXT_OFFSET=0x0 K0_BOOT_REUSE=skip
# 그래뉼 경계에 맞지 않는 적재 주소는 거부 (16K 이미지를 4K만 맞는 주소에)
refuse load-misaligned "$img16" 0x40081000 "k0: mmu enable failed" K0_IMAGE_TEXT_OFFSET=0x81000 K0_QEMU_CPU=max

# 다른 보드 모양: RAM이 0에서 시작, GICv3와 PL011이 다른 주소, EL2 진입
pass versal          "$img4" K0_QEMU_MACHINE=xlnx-versal-virt K0_QEMU_CPU= K0_QEMU_SMP= K0_QEMU_MEM=2G

# 고정 모드: 기대한 플랫폼에서는 부팅하고 다른 플랫폼에서는 거부
# 기대값은 문서의 절차대로 얻습니다. 기본 구성을 한 번 부팅해 부트 로그의
# `k0: platform` 줄을 K0_PIN에 넣어 빌드합니다(QEMU 버전에 묶이지 않음).
if want pinned-match $only || want pinned-other $only; then
    pin=""
    if K0_BOOT_LOG="$root/target/matrix-pin-probe.log" tools/qemu-boot-test.sh "$img4" > target/matrix-pin-probe.out 2>&1; then
        pin="$(tr -d '\r' < target/matrix-pin-probe.log | sed -n 's/^k0: platform //p' | head -n 1)"
    fi
    if [ -n "$pin" ] && K0_PIN="$pin" cargo build -p k0-kernel --offline --target-dir target/pin; then
        echo "matrix: pinned to: $pin"
        pass pinned-match    "$imgpin"
        refuse pinned-other  "$imgpin" 0x40080000 "differs from the pinned build" K0_QEMU_MACHINE=virt,gic-version=2
    else
        echo "matrix: pinned rows FAIL (no platform line or the pinned build failed)"
        show target/matrix-pin-probe.out
        failed="$failed pinned-match pinned-other"
    fi
fi

# UEFI: 펌웨어(EDK2)를 거쳐 로더로 부팅. 펌웨어가 디바이스 트리를 넘기는
# 구성(acpi=off)만 다루고, 로더는 UEFI 메모리 맵으로 메모리 기술을 다시 씀.
# 메모리 배치가 펌웨어 빌드에 달려 있으므로 재사용 시험은 건너뛰어도 통과로 봄
if want uefi-pe $only; then
    ran=$((ran + 1))
    if tools/pe-check.py "$efi4" > target/matrix-uefi-pe.out 2>&1 && tools/pe-check.py "$efi16" >> target/matrix-uefi-pe.out 2>&1; then
        echo "matrix: uefi-pe pass"
    else
        echo "matrix: uefi-pe FAIL"
        show target/matrix-uefi-pe.out
        failed="$failed uefi-pe"
    fi
fi
pass uefi-gic3-el1   "$efi4" K0_BOOT_REUSE=skip
pass uefi-gic3-el2   "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=3,acpi=off,virtualization=on
pass uefi-gic2-el1   "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=2,acpi=off
pass uefi-gic2-el2   "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=2,acpi=off,virtualization=on
pass uefi-max        "$efi4" K0_BOOT_REUSE=skip K0_QEMU_CPU=max K0_BOOT_REQUIRE="k0: pac=on bti=present pan=on"
pass uefi-smp4       "$efi4" K0_BOOT_REUSE=skip K0_QEMU_SMP=4
pass uefi-16k        "$efi16" K0_BOOT_REUSE=skip K0_QEMU_CPU=max
# 펌웨어의 난수 프로토콜에서 받은 32바이트가 커널의 엔트로피에 더해지는지
# (QEMU의 rng-seed 32 + 로더의 32 + kaslr-seed 8 = 72바이트)
pass uefi-rng        "$efi4" K0_BOOT_REUSE=skip K0_QEMU_EXTRA="-device virtio-rng-pci" K0_BOOT_REQUIRE="k0-loader: entropy from the firmware|k0: entropy dtb=72B"
# 이동식 매체 경로: FAT 드라이브의 EFI/BOOT/BOOTAA64.EFI (부트 매니저 대기가 붙음)
pass uefi-esp        "$efi4" K0_BOOT_REUSE=skip K0_QEMU_ESP=1
# 펌웨어가 디바이스 트리를 주지 않는 기본 구성(ACPI)과, 그래뉼이 없는 CPU는 로더가 거부
refuse_serial uefi-no-dtb     "$efi4" "the firmware provides no device tree" K0_QEMU_MACHINE=virt,gic-version=3
refuse_serial uefi-16k-on-a72 "$efi16" "this cpu has no 16K translation granule"
# 내장한 커널 이미지의 한 바이트가 바뀐 로더는 해시 불일치로 거부
if want uefi-corrupt $only; then
    bad="${efi4%.efi}-corrupt.efi"
    python3 - "$efi4" "$bad" <<'FLIP'
import sys
data = bytearray(open(sys.argv[1], "rb").read())
at = data.index(b"ARM\x64")  # 내장한 Image 헤더의 매직, 그 뒤는 커널 코드
data[at + 4096] ^= 0x01
open(sys.argv[2], "wb").write(data)
FLIP
    refuse_serial uefi-corrupt "$bad" "embedded kernel image is corrupt"
fi

# 실제 Apple 코어 (하이퍼바이저 게스트)
if [ "${K0_MATRIX_HVF:-0}" = 1 ]; then
    pass uefi-hvf-el1 "$efi4" K0_BOOT_REUSE=skip K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host
    pass uefi-hvf-el2 "$efi4" K0_BOOT_REUSE=skip K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_QEMU_MACHINE=virt,gic-version=3,acpi=off,virtualization=on
    pass uefi-hvf-16k "$efi16" K0_BOOT_REUSE=skip K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host
    pass hvf-el1     "$img4" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host
    pass hvf-el2     "$img4" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on
    pass hvf-16k     "$img16" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host
    pass hvf-smp4    "$img4" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_QEMU_SMP=4 K0_QEMU_MEM=1G
fi

if [ -n "$failed" ]; then
    echo "matrix: FAILED:$failed"
    exit 1
fi
echo "matrix: all $ran rows pass"
