#!/bin/sh
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
elf="${1:-$root/target/aarch64-unknown-none-softfloat/debug/k0-kernel}"
[ "$#" -gt 0 ] && shift
timeout_s="${K0_BOOT_TIMEOUT:-120}"
require="${K0_BOOT_REQUIRE:-}"
log="${K0_BOOT_LOG:-$root/target/qemu-boot-test.log}"

if [ ! -f "$elf" ]; then
    echo "boot-test: kernel ELF not found: $elf" >&2
    echo "boot-test: build it first with: cargo build -p k0-kernel --offline" >&2
    exit 2
fi

mkdir -p "$(dirname "$log")"
: > "$log"

"$root/tools/qemu-virt-runner.sh" "$elf" "$@" > "$log" 2>&1 < /dev/null &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT INT TERM

fail_re='root: FAIL|k0: EXCEPTION|k0: PANIC|k0: FATAL|k0: UNEXPECTED|rejected|failed|check fail|k0: all tasks|corrupt|overflow|no frame window|too many root task segments'

elapsed=0
state=timeout
while [ "$elapsed" -lt "$timeout_s" ]; do
    if grep -Eq "$fail_re" "$log"; then
        state=failure
        break
    fi
    if sed -n '/^root: sched tests pass/,$p' "$log" | grep -q '^k0: tick '; then
        state=done
        break
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
        state=exited
        break
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done

kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
trap - EXIT INT TERM

missing="$(tr -d '\r' < "$log" | awk '
BEGIN {
    n = split("k0: entry phase 1 (pre-MMU)|k0: dtb = |k0: mmu on (identity + higher-half alias)|k0: jumping to higher half|k0: higher half (pc = |k0: w^x checks pass|k0: memory |k0: root task integrity ok|k0: entropy dtb=|k0: pac=|k0: irq on (timer 1s)|k0: kernel table pool |k0: root task loaded|k0: untyped |k0: root cnode ready|k0: bootinfo page ready|k0: user w^x checks pass|k0: entering root task (el0)|root: hello from EL0|root: control byte filter [?]|root: retype/map tests pass|root: capability tests pass|root: untyped reuse test pass|root: child exit test pass|root: ipc tests pass|k0: task fault dabort-lower|k0: task fault fp-simd|root: fault isolation tests pass|root: preempt test pass|root: secret at |root: confined task reported|k0: task fault dabort-lower|root: isolation tests pass|k0: task fault dabort-lower|k0: task fault iabort-lower|k0: task fault dabort-lower|k0: unknown syscall 65535, killing task|root: violation tests pass|root: teardown tests pass|root: revocation tests pass|root: sched tests pass|k0: tick ", want, "|")
    i = 1
}
i <= n && index($0, want[i]) == 1 { i++ }
END {
    if (i <= n) print want[i]
}')"

# 자가 검증이 일부러 일으키는 폴트는 종류와 개수가 정해져 있습니다. 순서는 위
# 마커 목록이 확인하고, 여기서는 개수를 정확히 맞춥니다. 사라진 태스크가 다시
# 실행되거나 격리 태스크가 엉뚱한 이유로 죽으면 폴트 줄이 늘어나므로, 메시지가
# 오지 않는 것만으로 판정하는 검증들이 헛돌지 않게 하는 장치입니다.
#   null 역참조, FP/SIMD, 루트 전용 주소 읽기, 읽기 전용 페이지 쓰기,
#   스택으로의 분기, 커널 주소 읽기 = 6
want_faults="${K0_BOOT_FAULTS:-6}"
if [ -z "$missing" ]; then
    faults="$(tr -d '\r' < "$log" | grep -c '^k0: task fault ' || true)"
    unknown="$(tr -d '\r' < "$log" | grep -c '^k0: unknown syscall ' || true)"
    if [ "$faults" != "$want_faults" ]; then
        missing="exactly $want_faults task fault lines (found $faults)"
    elif [ "$unknown" != 1 ]; then
        missing="exactly 1 unknown syscall line (found $unknown)"
    fi
fi

# 격리 태스크의 폴트 주소는 루트 태스크가 알려 준 루트 전용 주소여야 합니다.
# 다른 주소에서 죽었다면 격리 때문이 아니라 엉뚱한 이유로 죽은 것입니다.
if [ -z "$missing" ]; then
    secret="$(tr -d '\r' < "$log" | sed -n 's/^root: secret at 0x0*//p' | head -n 1)"
    if [ -z "$secret" ] || ! tr -d '\r' < "$log" \
        | sed -n '/^root: confined task reported/,/^root: isolation tests pass/p' \
        | grep -q "^k0: task fault dabort-lower .* far 0x${secret})"; then
        missing="confined task fault at the root-only address 0x${secret}"
    fi
fi

if [ -z "$missing" ] && [ -n "$require" ]; then
    set -f
    old_ifs="$IFS"
    IFS='|'
    for marker in $require; do
        if ! tr -d '\r' < "$log" | grep -qF -- "$marker"; then
            missing="$marker"
            break
        fi
    done
    IFS="$old_ifs"
    set +f
fi

if [ "$state" = done ] && [ -z "$missing" ]; then
    echo "boot-test: pass (${elapsed}s, log $log)"
    exit 0
fi

echo "boot-test: FAIL (state $state after ${elapsed}s)" >&2
if [ -n "$missing" ]; then
    echo "boot-test: first missing marker: $missing" >&2
fi
grep -En "$fail_re" "$log" >&2 || true
echo "boot-test: last lines of $log" >&2
tail -n 40 "$log" >&2
exit 1
