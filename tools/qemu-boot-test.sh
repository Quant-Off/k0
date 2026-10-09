#!/bin/sh
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
elf="${1:-$root/target/aarch64-unknown-none-softfloat/debug/k0-kernel}"
timeout_s="${K0_BOOT_TIMEOUT:-120}"
log="${K0_BOOT_LOG:-$root/target/qemu-boot-test.log}"

if [ ! -f "$elf" ]; then
    echo "boot-test: kernel ELF not found: $elf" >&2
    echo "boot-test: build it first with: cargo build -p k0-kernel --offline" >&2
    exit 2
fi

mkdir -p "$(dirname "$log")"
: > "$log"

"$root/tools/qemu-virt-runner.sh" "$elf" > "$log" 2>&1 < /dev/null &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT INT TERM

fail_re='root: FAIL|k0: EXCEPTION|k0: PANIC|k0: UNEXPECTED|rejected|failed|check fail|k0: all tasks|corrupt|no frame window|too many root task segments|unknown syscall'

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
    n = split("k0: entry phase 1 (pre-MMU)|k0: dtb = |k0: mmu on (identity + higher-half alias)|k0: jumping to higher half|k0: higher half (pc = |k0: w^x checks pass|k0: memory |k0: root task integrity ok|k0: entropy dtb=|k0: pac=|k0: irq on (timer 1s)|k0: kernel table pool |k0: root task loaded|k0: untyped |k0: root cnode ready|k0: bootinfo page ready|k0: user w^x checks pass|k0: entering root task (el0)|root: hello from EL0|root: control byte filter [?]|root: retype/map tests pass|root: child exit test pass|root: ipc tests pass|k0: task fault dabort-lower|k0: task fault fp-simd|root: fault isolation tests pass|root: preempt test pass|root: sched tests pass|k0: tick ", want, "|")
    i = 1
}
i <= n && index($0, want[i]) == 1 { i++ }
END {
    if (i <= n) print want[i]
}')"

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
