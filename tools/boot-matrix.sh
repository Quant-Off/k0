#!/bin/sh
# 부팅 매트릭스: 지원한다고 말하는 구성마다 실제로 부팅해 확인합니다.
#
# 각 행은 아래 네 가지 판정 중 하나입니다.
#   - 직렬 판정: tools/qemu-boot-test.sh가 직렬 포트의 기록으로 자가 검증 전체
#     통과를 확인함. 콘솔 드라이버가 들어간 이미지에만 쓸 수 있음
#   - 메모리 판정: 같은 스크립트가 K0_BOOT_MEMLOG로 게스트 메모리의 부트 로그를
#     읽어 같은 판정을 하고, K0_BOOT_SERIAL=empty로 QEMU의 직렬 포트에 한
#     바이트도 나오지 않았음을, K0_BOOT_CONSOLE_PAGE=unmapped로 커널이 알린
#     디바이스 페이지 목록에 콘솔 페이지가 없음을 함께 확인함. 콘솔 드라이버가
#     없는 이미지용
#   - 거부 행: 거부해야 하는 구성에서 커널이 메모리에 남긴 부트 로그의 사유를 확인함
#   - 정적 행: 부팅하지 않고 이미지를 검사함(no-physical-timer, console-symbols)
# 입출력은 선택 기능이라 이미지마다 빌드에 콘솔 feature를 명시합니다. 이름 없이
# 빌드한 이미지(quiet)는 UART 코드가 아예 없고 출력은 부트 로그에만 남습니다.
# 사용법: tools/boot-matrix.sh [행 이름 ...]   (인자가 없으면 전부)
# 환경:   K0_MATRIX_HVF=1 이면 HVF 행도 실행(Apple Silicon의 macOS 전용)
set -u

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
t="target/aarch64-unknown-none-softfloat"
img4="$root/$t/debug/k0-kernel"
img16="$root/target/g16/aarch64-unknown-none-softfloat/debug/k0-kernel"
imgpin="$root/target/pin/aarch64-unknown-none-softfloat/debug/k0-kernel"
imgquiet="$root/target/quiet/aarch64-unknown-none-softfloat/debug/k0-kernel"
imgquiet16="$root/target/quiet16/aarch64-unknown-none-softfloat/debug/k0-kernel"
imgpl011="$root/target/pl011/aarch64-unknown-none-softfloat/debug/k0-kernel"
img16550="$root/target/u16550/aarch64-unknown-none-softfloat/debug/k0-kernel"

cargo build -p k0-kernel --offline --features console || exit 2
cargo build -p k0-kernel --offline --no-default-features --features granule-16k,console --target-dir target/g16 || exit 2
# 입출력 feature가 다른 이미지들(콘솔 없음, PL011 계열만, 16550만)
cargo build -p k0-kernel --offline --target-dir target/quiet || exit 2
cargo build -p k0-kernel --offline --no-default-features --features granule-16k --target-dir target/quiet16 || exit 2
cargo build -p k0-kernel --offline --features console-pl011 --target-dir target/pl011 || exit 2
cargo build -p k0-kernel --offline --features console-16550 --target-dir target/u16550 || exit 2
# 키 볼트(서명 오라클)를 넣은 이미지들: Ed25519 포함(4K, 16K), 알고리즘 없음
cargo build -p k0-kernel --offline --features console,vault-ed25519 --target-dir target/vault || exit 2
cargo build -p k0-kernel --offline --no-default-features --features granule-16k,console,vault-ed25519 --target-dir target/vault16 || exit 2
cargo build -p k0-kernel --offline --features console,vault --target-dir target/vaultna || exit 2
imgvault="$root/target/vault/aarch64-unknown-none-softfloat/debug/k0-kernel"
imgvault16="$root/target/vault16/aarch64-unknown-none-softfloat/debug/k0-kernel"
imgvaultna="$root/target/vaultna/aarch64-unknown-none-softfloat/debug/k0-kernel"
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
    # 모니터 소켓은 이 행만 쓰는 디렉토리에 둠(유닉스 소켓 경로는 짧아야 해서 /tmp 아래)
    sockdir="$(mktemp -d /tmp/k0-matrix.XXXXXX)"
    sock="$sockdir/monitor"
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
    rmdir "$sockdir" 2>/dev/null
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

# 콘솔 드라이버 심볼: feature가 코드에 그대로 반영되는지 이미지의 심볼로 확인
# 있어야 하는 심볼을 먼저 요구해서 심볼을 못 읽는 경우에 헛통과하지 않게 함
# 볼트의 코드가 feature대로만 들어가는지, 그리고 쓰기 가능한 static을 쓰지
# 않는지 루트 태스크의 ELF에서 확인합니다. 격리 태스크의 주소 공간에는 이미지의
# .data와 .bss가 없어서, 볼트나 암호 구현이 static에 쓰면 그 태스크가 죽습니다.
if want vault-symbols $only; then
    ran=$((ran + 1))
    host="$(rustc -vV | sed -n 's/^host: //p')"
    nm="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-nm"
    # 같은 타겟 디렉토리에 예전 빌드의 것이 남아 있을 수 있으므로 가장 새 것을 봄
    rt() { find "$root/target/$1" -path '*root-task-target*' -name root-task -type f -exec ls -t {} + 2>/dev/null | head -n 1; }
    crypto='dalek|sha2|subtle'
    bad=""
    for spec in ":0:0" "vault:1:1" "vaultna:1:0"; do
        dir="${spec%%:*}"; rest="${spec#*:}"; want_vault="${rest%%:*}"; want_alg="${rest#*:}"
        elf="$(rt "$dir" 2>/dev/null)"
        [ -n "$dir" ] || elf="$(rt aarch64-unknown-none-softfloat)"
        if [ -z "$elf" ]; then bad="$bad no-root-task-elf($dir)"; continue; fi
        s="$("$nm" -C "$elf" 2>&1)" || { bad="$bad llvm-nm-failed($dir)"; continue; }
        v=0; printf '%s\n' "$s" | grep -q 'k0_vault::' && v=1
        a=0; printf '%s\n' "$s" | grep -Eq "$crypto" && a=1
        [ "$v" = "$want_vault" ] || bad="$bad vault-code=$v($dir)"
        [ "$a" = "$want_alg" ] || bad="$bad crypto-code=$a($dir)"
        # 쓰기 가능한 데이터(d, D, b, B)에 볼트나 암호 구현의 심볼이 없어야 함
        if printf '%s\n' "$s" | awk '$2 ~ /^[dDbB]$/' | grep -Eq "k0_vault::|$crypto"; then
            bad="$bad writable-static($dir)"
        fi
    done
    if [ -z "$bad" ]; then
        echo "matrix: vault-symbols pass"
    else
        echo "matrix: vault-symbols FAIL (${bad# })"
        failed="$failed vault-symbols"
    fi
fi

if want console-symbols $only; then
    ran=$((ran + 1))
    host="$(rustc -vV | sed -n 's/^host: //p')"
    nm="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-nm"
    bad=""
    syms() { "$nm" -C "$1" 2>&1 || echo "nm-failed"; }
    s4="$(syms "$img4")"
    sq="$(syms "$imgquiet")"
    sp="$(syms "$imgpl011")"
    su="$(syms "$img16550")"
    case "$s4$sq$sp$su" in *nm-failed*) bad="$bad llvm-nm-failed" ;; esac
    has() { printf '%s\n' "$1" | grep -qF -- "$2"; }
    has "$s4" 'earlycon::uart::pl011::' || bad="$bad full-lacks-pl011"
    has "$s4" 'earlycon::uart::ns16550::' || bad="$bad full-lacks-ns16550"
    ! has "$sq" 'earlycon::uart::' || bad="$bad quiet-has-uart"
    has "$sp" 'earlycon::uart::pl011::' || bad="$bad pl011-lacks-pl011"
    ! has "$sp" 'earlycon::uart::ns16550::' || bad="$bad pl011-has-ns16550"
    has "$su" 'earlycon::uart::ns16550::' || bad="$bad 16550-lacks-ns16550"
    ! has "$su" 'earlycon::uart::pl011::' || bad="$bad 16550-has-pl011"
    # 이미지 파일에는 자기 구성을 알리는 줄이 정확히 한 가지만 들어 있어야 함
    # (부팅하지 않고 이미지가 어떤 입출력으로 빌드됐는지 알아보는 표지)
    label() { grep -aoE 'k0: console drivers \[[a-z0-9 ]*\]' "$1" | sort -u | tr '\n' '|'; }
    [ "$(label "$img4")" = "k0: console drivers [pl011 16550]|" ] || bad="$bad full-label"
    [ "$(label "$imgquiet")" = "k0: console drivers [none]|" ] || bad="$bad quiet-label"
    [ "$(label "$imgpl011")" = "k0: console drivers [pl011]|" ] || bad="$bad pl011-label"
    [ "$(label "$img16550")" = "k0: console drivers [16550]|" ] || bad="$bad 16550-label"
    if [ -z "$bad" ]; then
        echo "matrix: console-symbols pass"
    else
        echo "matrix: console-symbols FAIL (${bad# })"
        failed="$failed console-symbols"
    fi
fi

# 인터럽트 컨트롤러와 진입 EL
pass gic3-el1        "$img4" K0_BOOT_REQUIRE="k0: console drivers [pl011 16550]" K0_BOOT_CONSOLE_PAGE=mapped
pass gic3-el1-a53    "$img4" K0_QEMU_CPU=cortex-a53
# 키 볼트: 서명 오라클의 자가 검증까지 통과해야 함. 위반하는 클라이언트 둘이
# 폴트로 종료되므로 폴트 줄이 8개임
pass vault           "$imgvault" K0_BOOT_FAULTS=8 K0_BOOT_REQUIRE="k0: dtb seeds wiped 40B|root: oracle tests pass"
pass vault-max       "$imgvault" K0_QEMU_CPU=max K0_BOOT_FAULTS=8 K0_BOOT_REQUIRE="rndr=on|root: boot seed from rng-seed bytes 0x0000000000000020 and rndr|root: oracle tests pass"
pass vault-el2       "$imgvault" K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on K0_BOOT_FAULTS=8 K0_BOOT_REQUIRE="root: oracle tests pass"
pass vault-16k       "$imgvault16" K0_QEMU_CPU=max K0_BOOT_FAULTS=8 K0_BOOT_REQUIRE="root: oracle tests pass"
pass vault-no-alg    "$imgvaultna" K0_BOOT_FAULTS=8 K0_BOOT_REQUIRE="root: oracle tests pass"
pass gic3-el1-max    "$img4" K0_QEMU_CPU=max K0_BOOT_REQUIRE="rndr=on|k0: pac=on bti=present pan=on"
pass gic3-el2        "$img4" K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on
pass gic3-el2-max    "$img4" K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on K0_QEMU_CPU=max
pass gic2-el1        "$img4" K0_QEMU_MACHINE=virt,gic-version=2
pass gic2-el2        "$img4" K0_QEMU_MACHINE=virt,gic-version=2,virtualization=on
pass gic3-secure     "$img4" K0_QEMU_MACHINE=virt,gic-version=3,secure=on
pass gic2-secure     "$img4" K0_QEMU_MACHINE=virt,gic-version=2,secure=on
pass smp4            "$img4" K0_QEMU_SMP=4

# 입출력 feature: 콘솔 드라이버가 없는 이미지는 직렬 포트에 아무것도 쓰지 않고
# 판정은 메모리의 부트 로그로 함. QEMU virt의 UART는 PL011이므로 16550만 넣은
# 이미지도 이 머신에서는 맞는 드라이버가 없어 조용함
quietreq="k0: console drivers [none]|k0: no console (driver not built in)"
pass quiet             "$imgquiet" K0_BOOT_MEMLOG=0x40080000 K0_BOOT_SERIAL=empty K0_BOOT_CONSOLE_PAGE=unmapped K0_BOOT_REQUIRE="$quietreq"
pass quiet-16k         "$imgquiet16" K0_QEMU_CPU=max K0_BOOT_MEMLOG=0x40080000 K0_BOOT_SERIAL=empty K0_BOOT_CONSOLE_PAGE=unmapped K0_BOOT_REQUIRE="$quietreq"
pass console-pl011-only "$imgpl011" K0_BOOT_CONSOLE_PAGE=mapped K0_BOOT_REQUIRE="k0: console drivers [pl011]"
pass console-16550-only "$img16550" K0_BOOT_MEMLOG=0x40080000 K0_BOOT_SERIAL=empty K0_BOOT_CONSOLE_PAGE=unmapped K0_BOOT_REQUIRE="k0: console drivers [16550]|k0: no console (driver not built in)"

# 판정 자체의 점검: 콘솔 드라이버가 있는 이미지를 조용한 이미지의 기준으로
# 판정하면 직렬 포트가 비어 있지 않다는 이유로 떨어져야 함. 빈 직렬 포트 검사가
# 어느 날 헛돌게 되면 이 행이 알려 줌
if want serial-check $only; then
    ran=$((ran + 1))
    if env K0_BOOT_LOG="$root/target/matrix-serial-check.log" K0_BOOT_MEMLOG=0x40080000 K0_BOOT_SERIAL=empty \
        tools/qemu-boot-test.sh "$img4" > target/matrix-serial-check.out 2>&1; then
        echo "matrix: serial-check FAIL (a console image passed the empty serial judgement)"
        failed="$failed serial-check"
    elif grep -q 'first missing marker: an empty serial log' target/matrix-serial-check.out; then
        echo "matrix: serial-check pass (the console image is caught by its serial output)"
    else
        echo "matrix: serial-check FAIL (the judgement failed for another reason)"
        show target/matrix-serial-check.out
        failed="$failed serial-check"
    fi
fi

# PL011 식별 검사: 디바이스 트리가 PL011이라고 가리킨 주소에 다른 장치가 있으면
# 커널은 그 주소에 쓰지 않고, 그 페이지를 매핑해 두지도 않고, 콘솔 없이
# 부팅해야 함. QEMU virt의 디바이스 트리를 떠서 콘솔 노드의 주소만 RTC(PL031)의
# 주소로 바꾼 것으로 부팅함
if want pl011-identity $only; then
    idt="$root/target/matrix-pl011-identity.dtb"
    if qemu-system-aarch64 -machine "virt,gic-version=3,dumpdtb=$idt.orig" -cpu cortex-a72 -m 512M -smp 1 -nographic > target/matrix-pl011-identity.dump 2>&1 < /dev/null \
        && python3 - "$idt.orig" "$idt" >> target/matrix-pl011-identity.dump 2>&1 <<'SWAP'
import struct, sys
data = bytearray(open(sys.argv[1], "rb").read())
old = struct.pack(">QQ", 0x0900_0000, 0x1000)  # pl011@9000000의 reg
new = struct.pack(">QQ", 0x0901_0000, 0x1000)  # pl031@9010000이 있는 자리
if data.count(old) != 1:
    sys.exit(f"expected exactly one pl011 reg in the device tree, found {data.count(old)}")
at = data.index(old)
data[at:at + 16] = new
# QEMU는 1MiB 버퍼를 통째로 떠 주고, -dtb로 받은 파일은 크기를 두 배로 늘려
# 싣습니다. 그대로 두면 커널의 DTB 크기 상한을 넘으므로 쓰인 데까지만 남김
_, _, off_struct, off_strings, _, _, _, _, size_strings, size_struct = struct.unpack(">10I", data[:40])
used = (max(off_struct + size_struct, off_strings + size_strings) + 7) & ~7
data[4:8] = struct.pack(">I", used)
open(sys.argv[2], "wb").write(data[:used])
SWAP
    then
        pass pl011-identity "$img4" K0_QEMU_EXTRA="-dtb $idt" K0_BOOT_MEMLOG=0x40080000 K0_BOOT_SERIAL=empty K0_BOOT_CONSOLE_PAGE=unmapped K0_BOOT_REQUIRE="con=pl011@0x9010000|k0: console is not a pl011, ignored"
    else
        ran=$((ran + 1))
        echo "matrix: pl011-identity FAIL (could not prepare the device tree)"
        show target/matrix-pl011-identity.dump
        failed="$failed pl011-identity"
    fi
fi

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
    if [ -n "$pin" ] && K0_PIN="$pin" cargo build -p k0-kernel --offline --features console --target-dir target/pin; then
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
# ACPI: 펌웨어가 디바이스 트리를 주지 않는 QEMU의 기본 구성(acpi=off 없음). 로더가
# ACPI 정적 테이블(MADT, GTDT, SPCR)로 디바이스 트리를 만들어 같은 경로로 부팅
acpi="k0-loader: device tree built from ACPI tables"
pass uefi-acpi-gic3-el1 "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=3 K0_BOOT_REQUIRE="$acpi"
pass uefi-acpi-gic3-el2 "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on K0_BOOT_REQUIRE="$acpi"
pass uefi-acpi-gic2-el1 "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=2 K0_BOOT_REQUIRE="$acpi"
pass uefi-acpi-smp4     "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=3 K0_QEMU_SMP=4 K0_BOOT_REQUIRE="$acpi"
pass uefi-acpi-16k      "$efi16" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=3 K0_QEMU_CPU=max K0_BOOT_REQUIRE="$acpi"
# ACPI에는 펌웨어의 씨앗이 없으므로 엔트로피는 펌웨어 난수 프로토콜의 32바이트뿐
pass uefi-acpi-rng      "$efi4" K0_BOOT_REUSE=skip K0_QEMU_MACHINE=virt,gic-version=3 K0_QEMU_EXTRA="-device virtio-rng-pci" K0_BOOT_REQUIRE="$acpi|k0: entropy dtb=32B"
# 그래뉼이 없는 CPU는 로더가 거부
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
    pass uefi-hvf-acpi "$efi4" K0_BOOT_REUSE=skip K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_QEMU_MACHINE=virt,gic-version=3 K0_BOOT_REQUIRE="$acpi"
    pass hvf-el1     "$img4" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host
    pass hvf-vault   "$imgvault" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_BOOT_FAULTS=8 K0_BOOT_REQUIRE="root: oracle tests pass"
    pass hvf-el2     "$img4" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on
    pass hvf-16k     "$img16" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host
    pass hvf-quiet   "$imgquiet" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_BOOT_MEMLOG=0x40080000 K0_BOOT_SERIAL=empty K0_BOOT_CONSOLE_PAGE=unmapped K0_BOOT_REQUIRE="$quietreq"
    pass hvf-smp4    "$img4" K0_QEMU_ACCEL=hvf K0_QEMU_CPU=host K0_QEMU_SMP=4 K0_QEMU_MEM=1G
fi

if [ -n "$failed" ]; then
    echo "matrix: FAILED:$failed"
    exit 1
fi
echo "matrix: all $ran rows pass"
