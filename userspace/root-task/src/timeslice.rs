//! 타이머 선점과, 틱 사이에 CPU를 받은 태스크가 처음 맞는 틱에서 끊기지 않는지를
//! 보는 자가 검증 모듈입니다.
//!
//! # Features
//! 커널은 틱과 틱 사이에 CPU를 받은 태스크에게 처음 맞는 틱 하나를 넘기게 합니다.
//! 그렇지 않으면 틱 직전에 양보하거나 블록하는 태스크가 뒤에 선 태스크에게 주기의
//! 끝자락만 주고 다음 주기를 통째로 다시 받습니다. 이 모듈은 그 규칙이 양보
//! 경로와 블록 경로 양쪽에서 지켜지는지 확인합니다.
//!
//! EL0에는 시계가 없으므로 시간을 재지 않고 실행 순서만으로 판정합니다. 태스크
//! 넷을 씁니다. 넘겨주는 태스크, 넘겨받아 끝없이 도는 태스크, 처음 실행될 때
//! 표지를 남기는 태스크, 그리고 준비 단계의 도우미입니다. 루트가 틱 하나짜리
//! 기한 대기에 들어가면 준비 큐의 순서는 넘겨주는 태스크, 도는 태스크, 표지
//! 태스크가 됩니다. 넘겨주는 태스크는 곧바로 양보하거나(양보 경로) 블록하므로
//! (블록 경로) 도는 태스크는 틱 사이에 CPU를 받습니다.
//!
//! - 규칙이 지켜지면 도는 태스크는 첫 틱을 넘기고, 루트의 기한인 둘째 틱에서야
//!   끊깁니다. 그 틱에서 루트는 준비 큐에서 도는 태스크보다 앞에 서므로, 루트가
//!   다시 실행될 때까지 도는 태스크는 표지를 볼 수 없습니다.
//! - 규칙이 없으면 도는 태스크는 첫 틱에서 끊기고, 표지 태스크가 실행된 뒤에
//!   다시 돌면서 표지를 봅니다. 루트는 그 뒤인 둘째 틱에서야 깨어납니다.
//!
//! 루트가 깨어났다는 사실 자체가 양보하지 않는 태스크도 타이머에 끊긴다는 것을
//! 보여 줍니다(도는 태스크는 CPU를 스스로 내놓지 않음).
//!
//! 준비 단계에 틱이 끼어들어도 판정이 흔들리지 않게, 루트는 먼저 도우미에게
//! 양보했다가 도우미가 블록할 때 CPU를 돌려받습니다. 그러면 루트 자신이 틱
//! 사이에 들어온 태스크가 되어, 태스크 셋을 준비 큐에 넣고 대기에 들어가기까지의
//! 시스템 콜 네 개 사이에 온 틱 하나는 루트를 끊지 못합니다. 그 구간에 틱이 둘
//! 오려면 호스트가 그 사이에 1초 넘게 멈춰야 합니다.
//!
//! 판정은 기한의 계산에 기대고 있습니다. 틱 하나짜리 대기는 그때의 틱 수에 2를
//! 더한 틱에서 끝납니다(k0-ipc의 `deadline_of`). 루트가 대기에 들어가는 순간에
//! 틱이 이미 걸려 있던 드문 경우에는 규칙이 없는 커널도 통과할 수 있습니다(그
//! 틱이 넘겨주는 태스크의 면제를 쓰는 대신 기한은 한 틱 이르게 잡히므로). 규칙이
//! 있는 커널이 실패하는 틱 배치는 위의 1초 넘는 멈춤뿐입니다.
//!
//! 이 검증은 시작할 때 준비 큐에 다른 태스크가 없어야 합니다. 앞선 검증의
//! 태스크가 모두 종료된 자리에서만 부릅니다. 끝나면 만든 케이퍼빌리티를 전부
//! 지우고 받은 슬롯을 돌려줍니다.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::{
    alloc_slot, cap_delete, check, fail, put_str, recv, release_slots, retype, retype_at,
    tcb_resume, yield_now, Boot, NEXT_SLOT, TEST_VA,
};
use k0_abi::{err, ipc, obj, perm};

/// 표지 태스크가 실행됐으면 1
static MARK: AtomicU64 = AtomicU64::new(0);
/// 도는 태스크가 표지를 본 적이 있으면 1
static SAW: AtomicU64 = AtomicU64::new(0);
/// 도는 태스크의 진행(삭제된 뒤에 멈췄는지 보는 데 씀)
static SPINS: AtomicU64 = AtomicU64::new(0);
/// 아무도 보내지 않는 엔드포인트의 슬롯. 태스크들이 여기서 기한 없이 블록됨
static PARK: AtomicU64 = AtomicU64::new(0);

const BY_YIELD: u64 = 1;
const BY_BLOCK: u64 = 0;

/// 태스크 스택이 놓이는 첫 그래뉼 번호(TEST_VA 기준). 사이사이는 가드로 비워 둠
const STACK_BASE: u64 = 50;

/// 아무도 보내지 않는 엔드포인트에서 블록되는 함수입니다. 루트가 이 태스크를
/// 삭제할 때까지 돌아오지 않습니다.
fn park() -> ! {
    let _ = recv(PARK.load(Relaxed) as i64, 0);
    loop {
        yield_now();
    }
}

/// 다음 태스크에게 CPU를 넘겨주는 태스크의 진입점입니다.
///
/// # Arguments
/// `mode` - [BY_YIELD]면 먼저 양보하고, [BY_BLOCK]이면 곧바로 블록함
extern "C" fn giver_entry(mode: u64) -> ! {
    if mode == BY_YIELD {
        yield_now();
    }
    park()
}

/// 양보하지 않고 끝없이 도는 태스크의 진입점입니다. 표지를 보면 기록합니다.
extern "C" fn runner_entry(_arg: u64) -> ! {
    loop {
        SPINS.fetch_add(1, Relaxed);
        if MARK.load(Relaxed) != 0 {
            SAW.store(1, Relaxed);
        }
    }
}

/// 처음 실행될 때 표지를 남기고 블록되는 태스크의 진입점입니다.
extern "C" fn marker_entry(_arg: u64) -> ! {
    MARK.store(1, Relaxed);
    park()
}

/// 곧바로 블록되는 도우미 태스크의 진입점입니다.
extern "C" fn helper_entry(_arg: u64) -> ! {
    park()
}

/// 검증이 쓰는 슬롯과 스택 묶음 구조체입니다. 두 경로가 같은 슬롯을 다시 씁니다.
struct Stage {
    ut: u64,
    /// 루트가 기한 대기를 거는 엔드포인트
    wait: i64,
    /// 넘겨주는 태스크, 도는 태스크, 표지 태스크, 도우미의 TCB 슬롯과 스택 최상단
    tcb: [u64; 4],
    sp: [u64; 4],
}

/// 한 경로를 검증하는 함수입니다. 도는 태스크가 첫 틱을 넘겼으면 true
///
/// # Arguments
/// `st` - 슬롯과 스택
/// `mode` - [BY_YIELD] 또는 [BY_BLOCK]
fn one(st: &Stage, mode: u64) -> bool {
    MARK.store(0, Relaxed);
    SAW.store(0, Relaxed);
    let entries = [
        (giver_entry as usize as u64, mode),
        (runner_entry as usize as u64, 0),
        (marker_entry as usize as u64, 0),
        (helper_entry as usize as u64, 0),
    ];
    for (i, (entry, arg)) in entries.into_iter().enumerate() {
        check("slice tcb", retype_at(st.ut, obj::TCB, st.tcb[i]) == 0);
        check(
            "slice configure",
            super::tcb_configure(
                st.tcb[i] as i64,
                entry,
                st.sp[i],
                k0_abi::bootinfo::slot::ADDR_SPACE as i64,
                k0_abi::bootinfo::slot::CNODE as i64,
                arg,
            ) == 0,
        );
    }
    let [giver, runner, marker, helper] = st.tcb.map(|s| s as i64);

    // 루트를 틱 사이에 들어온 태스크로 만듦: 도우미가 블록하면서 CPU를 돌려줌
    check("slice resume helper", tcb_resume(helper) == 0);
    yield_now();

    check("slice resume giver", tcb_resume(giver) == 0);
    check("slice resume runner", tcb_resume(runner) == 0);
    check("slice resume marker", tcb_resume(marker) == 0);
    check("slice wait", recv(st.wait, ipc::timeout(1)).status == err::TIMEOUT);
    let (saw, marked) = (SAW.load(Relaxed), MARK.load(Relaxed));

    // 도는 태스크는 준비 큐에 있음. 삭제하면 더 실행되지 않아야 함
    check("slice delete runner", cap_delete(runner) == 0);
    let seen = SPINS.load(Relaxed);
    check("slice delete giver", cap_delete(giver) == 0);
    check("slice delete marker", cap_delete(marker) == 0);
    check("slice delete helper", cap_delete(helper) == 0);
    for _ in 0..4 {
        yield_now();
    }
    check("slice deleted runner stopped", seen != 0 && SPINS.load(Relaxed) == seen);
    // 표지 태스크는 어느 커널에서든 루트보다 먼저 실행됨
    check("slice marker ran", marked == 1);
    saw == 0
}

/// 타이머 선점과 틱 면제를 양보 경로와 블록 경로에서 검증하는 함수입니다.
///
/// # Arguments
/// `b` - 부팅 때 찾은 untyped와 그래뉼 크기
pub fn test(b: &Boot) {
    let (ut, fs) = (b.ut_main, b.frame_size);
    let first_slot = NEXT_SLOT.load(Relaxed);
    let wait = retype(ut, obj::ENDPOINT);
    let park = retype(ut, obj::ENDPOINT);
    check("slice endpoints", wait > 0 && park > 0);
    PARK.store(park as u64, Relaxed);
    let mut st = Stage {
        ut,
        wait,
        tcb: [0; 4],
        sp: [0; 4],
    };
    for i in 0..4 {
        let frame = retype(ut, obj::FRAME);
        let va = TEST_VA + (STACK_BASE + 2 * i as u64) * fs;
        check("slice stack", frame > 0 && super::map(frame, va, perm::RW) == 0);
        st.sp[i] = va + fs;
        st.tcb[i] = alloc_slot();
    }

    if !one(&st, BY_YIELD) {
        fail("slice: a task that got the cpu by a yield was cut at its first tick");
    }
    put_str("root: slice test yield pass\n");
    if !one(&st, BY_BLOCK) {
        fail("slice: a task that got the cpu by a block was cut at its first tick");
    }
    put_str("root: slice test block pass\n");

    // 태스크는 경로마다 지웠음. 스택(삭제가 매핑도 걷음)과 엔드포인트까지 지우고
    // 슬롯을 돌려줌
    release_slots(first_slot);
    put_str("root: preempt test pass\n");
}
