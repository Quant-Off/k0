#!/bin/sh
# 커널을 QEMU에서 부팅하고 루트 태스크의 자가 검증 줄을 순서대로 확인합니다.
#
# 사용법: tools/qemu-boot-test.sh [kernel ELF 또는 로더 .efi] [QEMU 인자 ...]
# 환경(머신 구성은 tools/qemu-virt-runner.sh 참고)
#   K0_BOOT_TIMEOUT  기다리는 시간(초), 기본 120
#   K0_BOOT_LOG      판정에 쓰는 기록을 둘 파일
#   K0_BOOT_REQUIRE  더 있어야 하는 줄들, `|`로 나눔
#   K0_BOOT_REUSE    skip이면 재사용 시험을 건너뛴 구성도 통과로 봄
#   K0_BOOT_FAULTS   기대하는 태스크 폴트 줄의 수, 기본 6
#   K0_BOOT_MEMLOG   이미지를 적재한 물리 주소. 주면 커널 줄을 직렬 포트가 아니라
#                    게스트 메모리의 부트 로그에서 읽어 판정함. 콘솔 드라이버 없이
#                    빌드한 이미지는 직렬 포트에 아무것도 쓰지 않으므로 이 방법뿐임
#   K0_BOOT_SERIAL   empty이면 직렬 포트에 한 바이트도 나오지 않아야 통과
#                    (K0_BOOT_MEMLOG와 함께 씀)
#   K0_BOOT_CONSOLE_PAGE  mapped 또는 unmapped. 플랫폼 줄의 콘솔 주소가 커널이
#                    알린 디바이스 페이지 목록(`k0: device pages`)에 있어야 하는지,
#                    없어야 하는지를 정함
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
elf="${1:-$root/target/aarch64-unknown-none-softfloat/debug/k0-kernel}"
[ "$#" -gt 0 ] && shift
timeout_s="${K0_BOOT_TIMEOUT:-120}"
require="${K0_BOOT_REQUIRE:-}"
log="${K0_BOOT_LOG:-$root/target/qemu-boot-test.log}"
memlog="${K0_BOOT_MEMLOG:-}"
serial_rule="${K0_BOOT_SERIAL:-}"
page_rule="${K0_BOOT_CONSOLE_PAGE:-}"
# 커널이 로그에 남기는 틱 줄의 수(crates/k0-arch/src/irq.rs의 LOGGED_TICKS)
logged_ticks=4

if [ ! -f "$elf" ]; then
    echo "boot-test: kernel ELF not found: $elf" >&2
    echo "boot-test: build it first with: cargo build -p k0-kernel --features console --offline" >&2
    exit 2
fi
if [ "$serial_rule" = empty ] && [ -z "$memlog" ]; then
    echo "boot-test: K0_BOOT_SERIAL=empty needs K0_BOOT_MEMLOG (nothing else is left to judge)" >&2
    exit 2
fi
case "$page_rule" in
"" | mapped | unmapped) ;;
*)
    echo "boot-test: K0_BOOT_CONSOLE_PAGE must be mapped or unmapped" >&2
    exit 2
    ;;
esac
case "$memlog:$elf" in
:*) ;;
*.efi)
    echo "boot-test: K0_BOOT_MEMLOG does not work with a UEFI loader (the load address is not fixed)" >&2
    exit 2
    ;;
esac

mkdir -p "$(dirname "$log")"
: > "$log"

# 메모리에서 읽을 때는 직렬 포트의 출력을 따로 받아 둡니다. QEMU 자신의 경고는
# 표준 오류로 나오므로 직렬 포트의 바이트와 섞이지 않게 나눕니다.
sock=""
sockdir=""
cleanup() {
    [ -z "$sock" ] || rm -f "$sock" "$log.new"
    [ -z "$sockdir" ] || rmdir "$sockdir" 2>/dev/null || true
}
if [ -n "$memlog" ]; then
    serial="$log.serial"
    : > "$log.reader"
    # 모니터 소켓은 이 실행만 쓰는 디렉토리에 둠. 유닉스 소켓 경로는 길이 제한이
    # 짧아서 TMPDIR이 아니라 /tmp 아래에 만듦
    sockdir="$(mktemp -d /tmp/k0-boot.XXXXXX)"
    sock="$sockdir/monitor"
    K0_QEMU_MONITOR="$sock" "$root/tools/qemu-virt-runner.sh" "$elf" "$@" > "$serial" 2> "$log.qemu" < /dev/null &
else
    serial="$log"
    "$root/tools/qemu-virt-runner.sh" "$elf" "$@" > "$log" 2>&1 < /dev/null &
fi
pid=$!
trap 'kill "$pid" 2>/dev/null || true; cleanup' EXIT INT TERM

fail_re='root: FAIL|k0: EXCEPTION|k0: PANIC|k0: FATAL|k0: UNEXPECTED|rejected|failed|check fail|k0: all tasks|corrupt|overflow|no frame window|too many root task segments|k0-loader: refused'

# UEFI 부팅(.efi)에서는 펌웨어가 같은 직렬 포트에 먼저 출력하고, 그 안에
# "failed" 같은 낱말이 섞여 있습니다. 실패 판정은 로더의 첫 줄부터 봅니다.
case "$elf" in
*.efi) judged() { tr -d '\r' < "$log" | sed -n '/k0-loader: start/,$p'; } ;;
*) judged() { cat "$log"; } ;;
esac

# 메모리에서 읽는 데 걸리는 시간도 세도록 벽시계로 잼
started="$(date +%s)"
elapsed=0
state=timeout
pass_ticks=""
while [ "$elapsed" -lt "$timeout_s" ]; do
    if [ -n "$memlog" ]; then
        # 읽기에 성공한 마지막 기록만 남김(모니터가 아직 안 떴거나 로그가 비어 있으면 실패함)
        if "$root/tools/boot-log.py" "$elf" "$memlog" "$sock" > "$log.new" 2> "$log.reader"; then
            mv "$log.new" "$log"
        fi
    fi
    if judged | grep -Eq "$fail_re"; then
        state=failure
        break
    fi
    if [ -n "$memlog" ]; then
        # 직렬 판정은 자가 검증이 끝난 뒤의 틱 한 번까지 기다려, 그동안 커널이
        # 살아 있고 실패 줄이 없음을 봅니다. 로그에는 처음 몇 번의 틱만 남으므로,
        # 마지막 자가 검증 줄을 처음 본 때의 틱 줄 수를 적어 두고 그 수가 늘 때까지
        # 같은 방식으로 기다립니다. 이미 남길 수 있는 틱이 다 찬 뒤라면 더 볼 수
        # 있는 것이 없으므로 한 번 더 읽고 끝냅니다.
        if grep -q '^root: sched tests pass' "$log"; then
            ticks="$(grep -c '^k0: tick ' "$log" || true)"
            if [ -z "$pass_ticks" ]; then
                pass_ticks="$ticks"
            elif [ "$pass_ticks" -ge "$logged_ticks" ] || [ "$ticks" -gt "$pass_ticks" ]; then
                state=done
                break
            fi
        fi
    elif sed -n '/^root: sched tests pass/,$p' "$log" | grep -q '^k0: tick '; then
        state=done
        break
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
        state=exited
        break
    fi
    sleep 1
    elapsed=$(($(date +%s) - started))
done

kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
trap - EXIT INT TERM
cleanup

# 맞는 크기의 untyped가 없는 메모리 배치에서는 루트 태스크가 재사용 시험을
# 건너뜁니다. 그런 구성임을 호출자가 K0_BOOT_REUSE=skip으로 선언한 경우에만
# 건너뜀 줄을 통과로 칩니다. 기본은 엄격합니다.
if [ "${K0_BOOT_REUSE:-run}" = skip ]; then
    reuse_sed='s/^root: untyped reuse test skipped.*/root: untyped reuse test pass/'
else
    reuse_sed=''
fi

markers="k0: entry phase 1 (pre-MMU)|k0: dtb = |k0: mmu on (identity + fixed image address)|k0: jumping to higher half|k0: higher half (pc = |k0: w^x checks pass|k0: memory |k0: platform |k0: root task integrity ok|k0: entropy dtb=|k0: pac=|k0: irq on (timer 1s)|k0: kernel table pool |k0: root task loaded|k0: kernel reserve |k0: untyped |k0: root cnode ready|k0: bootinfo page ready|k0: user w^x checks pass|k0: entering root task (el0)|root: hello from EL0|root: control byte filter [?]|root: retype/map tests pass|root: capability tests pass|root: untyped reuse test pass|root: child exit test pass|root: ipc tests pass|k0: task fault dabort-lower|k0: task fault fp-simd|root: fault isolation tests pass|root: preempt test pass|root: secret at |root: confined task reported|k0: task fault dabort-lower|root: isolation tests pass|k0: task fault dabort-lower|k0: task fault iabort-lower|k0: task fault dabort-lower|k0: unknown syscall 65535, killing task|root: violation tests pass|root: teardown tests pass|k0: idle (waiting for a timeout)|root: timeout tests pass|root: revocation tests pass|root: kernel reserve tests pass|root: sched tests pass|k0: tick "
# 메모리의 로그에는 처음 몇 번의 틱만 남습니다. 자가 검증이 그보다 오래 걸리는
# 호스트에서는 끝의 틱이 로그에 없을 수 있으므로 순서 목록에서는 빼고, 첫 틱이
# 기록됐는지와(아래) 자가 검증 뒤에도 틱이 이어지는지를(위의 대기) 따로 봅니다.
# 타이머 선점 자체는 루트 태스크의 선점 시험 줄이 확인합니다.
if [ -n "$memlog" ]; then
    markers="${markers%|k0: tick }"
fi

missing="$(tr -d '\r' < "$log" | sed "$reuse_sed" | awk -v markers="$markers" '
BEGIN {
    n = split(markers, want, "|")
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

# 타임아웃 n틱은 n주기보다 짧게 끝나면 안 됩니다. EL0에는 시계가 없으므로 틱
# 줄로 셉니다. 타임아웃 검증은 1틱짜리 대기를 두 번 차례로 끝까지 기다리고
# (CALL의 응답 기한, 그다음 루트 자신의 수신 기한), 각각 건 뒤 두 번째 틱에서
# 끝나므로 그 사이에 틱 줄이 적어도 4개 있어야 합니다. 메모리의 로그에는 처음
# 몇 번의 틱만 남으므로 직렬 판정에서만 셉니다.
if [ -z "$missing" ] && [ -z "$memlog" ]; then
    waited="$(tr -d '\r' < "$log" \
        | sed -n '/^root: teardown tests pass/,/^root: timeout tests pass/p' \
        | grep -c '^k0: tick ' || true)"
    if [ "$waited" -lt 4 ]; then
        missing="at least 4 timer ticks during the timeout tests (found $waited)"
    fi
fi

if [ -z "$missing" ] && [ -n "$memlog" ] && ! grep -q '^k0: tick 1$' "$log"; then
    missing="k0: tick 1"
fi
if [ -z "$missing" ] && [ -n "$memlog" ] && [ "$state" = timeout ] && [ -n "$pass_ticks" ]; then
    missing="a timer tick after the self-test (the log stayed at $pass_ticks tick lines)"
fi

# 커널은 사용자 공간으로 넘어가기 직전에 매핑해 둔 디바이스 페이지를 한 줄로
# 알립니다. 플랫폼 줄의 콘솔 주소가 그 목록에 있는지를 보고, 콘솔 드라이버가 없는
# 이미지가 UART 페이지를 매핑하지 않았음을(드라이버가 있는 이미지는 매핑했음을)
# 확인합니다. 양쪽을 다 요구하는 행이 있어야 이 검사가 헛돌지 않습니다.
if [ -z "$missing" ] && [ -n "$page_rule" ]; then
    con_at="$(tr -d '\r' < "$log" | sed -n 's/^k0: platform .*con=[a-z0-9]*@\(0x[0-9a-f]*\).*/\1/p' | head -n 1)"
    pages="$(tr -d '\r' < "$log" | sed -n 's/^k0: device pages \[\(.*\)\] of \(0x[0-9a-f]*\) bytes$/\2 \1/p' | head -n 1)"
    if [ -z "$con_at" ]; then
        missing="a console address in the k0: platform line"
    elif [ -z "$pages" ]; then
        missing="k0: device pages"
    else
        gran="${pages%% *}"
        list=" ${pages#* } "
        page="$(printf '0x%x' $(($con_at & ~($gran - 1))))"
        case "$list" in
        *" $page "*) found=mapped ;;
        *) found=unmapped ;;
        esac
        if [ "$found" != "$page_rule" ]; then
            missing="the console page $page to be $page_rule (it is $found, device pages:$list)"
        fi
    fi
fi

# 콘솔 드라이버가 없는 이미지는 직렬 포트에 아무것도 쓰지 않아야 합니다.
if [ -z "$missing" ] && [ "$serial_rule" = empty ] && [ -s "$serial" ]; then
    missing="an empty serial log (found $(wc -c < "$serial" | tr -d ' ') bytes in $serial)"
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
judged | grep -En "$fail_re" >&2 || true
if [ -z "$memlog" ] && ! grep -aq '^k0: ' "$log"; then
    echo "boot-test: the serial log has no kernel line. A kernel built without a console feature writes" >&2
    echo "boot-test: nothing to the serial port. Build with --features console, or judge the log" >&2
    echo "boot-test: in guest memory with K0_BOOT_MEMLOG=<load address>" >&2
fi
if [ -n "$memlog" ] && [ -s "$log.reader" ]; then
    echo "boot-test: last message of the memory log reader" >&2
    tail -n 5 "$log.reader" >&2
fi
echo "boot-test: last lines of $log" >&2
tail -n 40 "$log" >&2
exit 1
