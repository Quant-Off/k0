//! 라운드 로빈 스케줄러 크레이트입니다.
//!
//! # Features
//! 커널 heap 없이 TCB에 내장된 링크(intrusive list)로 준비 큐를 만듭니다.
//! 큐 노드 할당이 없어 큐 길이가 태스크 수로 자연히 상한되므로 스케줄러가
//! 커널 자원 고갈의 원인이 되지 못합니다. 이양(handoff) 후 커널은 예외
//! 컨텍스트에서만 실행되고, 벡터가 사용자 컨텍스트를 현재 TCB에 저장한
//! 상태에서 전환이 일어나므로 전환은 `__current_context` 포인터 갱신과
//! TTBR0 교체(주소 공간이 다를 때만, ASID 미사용이라 전체 TLB 무효화
//! 동반)로 끝납니다. 선점 신호는 진입 페이즈 2의 타이머 틱입니다.
//! IPC 블록은 [block_and_switch]로 현재 태스크를 준비 큐 밖(엔드포인트
//! 대기 큐나 reply 링크)에서 재우고, 깨우기는 [enqueue]입니다.
//!
//! 기한이 있는 IPC 대기자는 준비 큐나 대기 큐와 별개인 기한 목록에도
//! 들어갑니다([arm_timeout]). 블록된 태스크가 블록 상태를 벗어나는 길은
//! 깨어남([enqueue])과 종료(k0-ipc의 teardown) 둘뿐이고 둘 다 태스크를 이
//! 목록에서 빼므로, 목록에는 지금 블록돼 있는 태스크만 남습니다. 목록은
//! 정렬하지 않습니다. 틱마다 [expired]가 부르지만, 가장 이른 기한이 아직 오지
//! 않았으면 목록을 훑지 않습니다. 훑는 비용은 기한을 건 대기자 수에
//! 비례하고 그 수는 TCB 수를 넘지 못합니다.
//!
//! # Errors
//! 전환 대상이 없으면 현재 태스크가 그대로 계속됩니다(에러 아님). 현재
//! 태스크가 종료됐거나 블록됐는데 준비된 태스크가 없으면
//! [block_and_switch]가 false를 반환하고, 호출자(커널 정책)는 다음
//! 인터럽트를 기다렸다가 다시 시도합니다. 기한 목록의 링크가 어긋나 있으면
//! `k0_arch::fatal`로 정지합니다.

#![no_std]

use core::cell::UnsafeCell;

use k0_task::{TaskState, Tcb, TcbQueue};

/// 스케줄러 상태 구조체입니다. `current`는 TCB의 커널 VA(0 = 없음)입니다.
struct Sched {
    current: u64,
    ready: TcbQueue,
    /// 기한이 있는 대기자 목록의 머리(TCB의 커널 VA, 0 = 없음)
    timed: u64,
    /// 목록에 든 기한 중 가장 이른 값의 하한. 대기자가 빠질 때는 고치지
    /// 않으므로 실제보다 이를 수 있고(그러면 한 번 헛되이 훑을 뿐), 늦을 수는
    /// 없음
    timed_min: u64,
}

struct SyncCell(UnsafeCell<Sched>);

/// # Safety
/// 부트 코어의 초기화 시퀀스와 이양 후의 예외 컨텍스트(단일 코어, 예외
/// 진입 시 DAIF 마스크)에서만 접근하므로 동시 접근이 없습니다.
unsafe impl Sync for SyncCell {}

static SCHED: SyncCell = SyncCell(UnsafeCell::new(Sched {
    current: 0,
    ready: TcbQueue::new(),
    timed: 0,
    timed_min: 0,
}));

/// 준비 큐 꼬리에 TCB를 붙이는 함수입니다.
///
/// # Safety
/// `tcb`는 유효한 커널 VA의 TCB이고 큐에 없어야 합니다.
unsafe fn push(s: &mut Sched, tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 tcb는 유효하고 큐 밖에 있음
    unsafe {
        (*tcb).state = TaskState::Ready;
        s.ready.push(tcb);
    }
}

/// 준비 큐 머리를 꺼내는 함수입니다. 비어 있으면 0
///
/// # Safety
/// 큐의 링크들이 유효한 TCB를 가리켜야 합니다.
unsafe fn pop(s: &mut Sched) -> u64 {
    // SAFETY: 함수 계약을 큐로 중계함
    unsafe { s.ready.pop() as u64 }
}

/// 다음 태스크로 실행 문맥을 전환하는 함수입니다.
///
/// 벡터의 복귀 경로(`__user_restore`)가 `__current_context`를 다시 읽기
/// 때문에 포인터 갱신만으로 전환이 완성됩니다.
///
/// # Safety
/// `next`는 준비 큐에서 나온 유효한 TCB여야 하고, 현재 태스크의 컨텍스트
/// 저장이 끝난 예외 컨텍스트여야 합니다(벡터가 보장).
unsafe fn switch_to(s: &mut Sched, next: u64) {
    let t = next as *mut Tcb;
    // SAFETY: 함수 계약대로 t는 유효한 TCB임
    unsafe {
        (*t).state = TaskState::Running;
        s.current = next;
        k0_arch::usermode::set_current(&raw mut (*t).ctx);
        // ASID를 쓰지 않으므로 주소 공간이 다를 때만 교체(전체 TLB 무효화 동반)
        if (*t).ttbr0_pa != k0_mm::current_user_root() {
            k0_mm::install_user_ttbr0((*t).ttbr0_pa);
        }
    }
}

/// 기한 목록에서 TCB를 빼고 기한을 지우는 함수입니다. 기한이 없는 TCB에는
/// 아무 일도 하지 않습니다.
///
/// 이웃의 링크가 이 TCB를 가리키는지 먼저 확인하고, 어긋나 있으면 아무것도
/// 바꾸지 않은 채 정지합니다.
///
/// # Safety
/// `tcb`는 유효한 TCB이고, 기한이 있다면 목록의 링크들이 유효한 TCB를
/// 가리켜야 합니다.
unsafe fn disarm(s: &mut Sched, tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 tcb와 그 이웃은 유효한 TCB임
    unsafe {
        if (*tcb).deadline == 0 {
            return;
        }
        let me = tcb as u64;
        let next = (*tcb).tnext as *mut Tcb;
        let prev = (*tcb).tprev as *mut Tcb;
        let prev_ok = if prev.is_null() { s.timed == me } else { (*prev).tnext == me };
        let next_ok = next.is_null() || (*next).tprev == me;
        if !prev_ok || !next_ok {
            k0_arch::fatal("timeout list corrupt");
        }
        if prev.is_null() {
            s.timed = next as u64;
        } else {
            (*prev).tnext = next as u64;
        }
        if !next.is_null() {
            (*next).tprev = prev as u64;
        }
        (*tcb).tnext = 0;
        (*tcb).tprev = 0;
        (*tcb).deadline = 0;
    }
}

/// 블록된 태스크에 기한을 걸어 기한 목록에 넣는 함수입니다.
///
/// # Arguments
/// `tcb` - 방금 블록 상태가 된 TCB
/// `deadline` - 기한이 되는 틱 수(0이 아니어야 함)
///
/// # Safety
/// 시스템 콜 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고, `tcb`는
/// 유효한 TCB이며 기한 목록에 없어야 합니다.
pub unsafe fn arm_timeout(tcb: *mut Tcb, deadline: u64) {
    if deadline == 0 {
        return;
    }
    // SAFETY: 함수 계약대로 접근이 배타적이고 tcb는 목록 밖의 유효한 TCB임.
    //         머리는 비었거나 유효한 TCB임
    unsafe {
        let s = &mut *SCHED.0.get();
        if (*tcb).deadline != 0 {
            k0_arch::fatal("timeout armed twice");
        }
        let head = s.timed as *mut Tcb;
        (*tcb).deadline = deadline;
        (*tcb).tprev = 0;
        (*tcb).tnext = head as u64;
        if head.is_null() {
            s.timed_min = deadline;
        } else {
            (*head).tprev = tcb as u64;
            s.timed_min = s.timed_min.min(deadline);
        }
        s.timed = tcb as u64;
    }
}

/// 기한을 건 대기자가 하나라도 있는지 알려 주는 함수입니다.
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 합니다.
pub unsafe fn has_timed() -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적임
    unsafe { (*SCHED.0.get()).timed != 0 }
}

/// TCB의 기한을 취소하는 함수입니다. 기한이 없으면 아무 일도 하지 않습니다.
///
/// 태스크가 깨어나지 않고 블록 상태를 벗어나는 종료 처리에서 부릅니다.
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고,
/// `tcb`는 유효한 TCB여야 합니다.
pub unsafe fn disarm_timeout(tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 접근이 배타적이고 계약을 disarm으로 중계함
    unsafe { disarm(&mut *SCHED.0.get(), tcb) };
}

/// 기한이 지난 대기자를 목록에서 빼면서 하나씩 넘겨 주는 함수입니다.
///
/// 넘겨받은 쪽이 그 태스크를 대기 큐나 응답 링크에서 떼어 내고 [enqueue]로
/// 깨워야 합니다. 콜백이 스케줄러 상태를 다시 빌리므로 이 함수는 콜백을
/// 부르는 동안 스케줄러에 대한 참조를 들고 있지 않습니다.
///
/// # Arguments
/// `now` - 지금의 틱 수
/// `f` - 기한이 지난 TCB마다 한 번 불리는 콜백. 넘겨받은 TCB 외의 태스크를
/// 기한 목록에서 빼면 안 됨
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 합니다.
pub unsafe fn expired(now: u64, mut f: impl FnMut(*mut Tcb)) {
    // SAFETY: 함수 계약대로 접근이 배타적이고 목록의 링크는 유효한 TCB임.
    //         다음 링크는 콜백 전에 읽어 두며, 콜백은 넘겨받은 TCB만 다룸
    unsafe {
        let mut t = {
            let s = &*SCHED.0.get();
            if s.timed == 0 || now < s.timed_min {
                return;
            }
            s.timed as *mut Tcb
        };
        // 남는 대기자들의 가장 이른 기한을 다시 구함
        let mut earliest = u64::MAX;
        while !t.is_null() {
            let next = (*t).tnext as *mut Tcb;
            let deadline = (*t).deadline;
            if deadline <= now {
                disarm(&mut *SCHED.0.get(), t);
                f(t);
            } else {
                earliest = earliest.min(deadline);
            }
            t = next;
        }
        (*SCHED.0.get()).timed_min = earliest;
    }
}

/// 루트 태스크로 제어권을 이양하는 함수입니다. 복귀하지 않습니다.
///
/// 루트 태스크를 Running으로 등록하고 벡터의 EL0 경로가 저장/복원할
/// 컨텍스트를 지정한 뒤 EL0로 진입합니다. 이후 커널 코드는 예외로만
/// 실행됩니다.
///
/// # Arguments
/// `tcb` - 스폰을 마친 루트 태스크의 TCB
///
/// # Safety
/// TCB의 주소 공간(TTBR0)이 설치된 뒤에만 호출해야 합니다. 커널의 identity
/// 의존이 남아 있으면 안 됩니다(호출 순서는 kernel_main이 강제).
pub unsafe fn handoff(tcb: &'static mut Tcb) -> ! {
    tcb.state = TaskState::Running;
    // SAFETY: 이양 직전의 단일 부트 코어라 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    s.current = core::ptr::from_mut(tcb) as u64;
    k0_arch::usermode::set_current(&raw mut tcb.ctx);
    // SAFETY: 함수 계약대로 컨텍스트와 주소 공간이 준비된 발산 경로에서 호출됨
    unsafe { k0_arch::usermode::enter_user() }
}

/// TCB를 준비 큐에 넣어 실행 대상으로 만드는 함수입니다.
///
/// 구성(TCB_CONFIGURE)을 마친 태스크의 첫 진입과 IPC 블록에서 깨어나는
/// 태스크가 같은 경로를 씁니다. 깨어나는 태스크에 기한이 걸려 있었다면
/// 여기서 취소되므로, 지나간 대기의 기한이 나중의 대기를 끝내는 일은
/// 없습니다.
///
/// # Arguments
/// `tcb` - 실행을 기다릴 TCB의 커널 VA 포인터
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고,
/// `tcb`는 유효한 TCB이며 큐에 없고 실행 중(Running)이 아니어야 합니다.
pub unsafe fn enqueue(tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    // SAFETY: 함수 계약을 disarm과 push로 중계함
    unsafe {
        disarm(s, tcb);
        push(s, tcb);
    }
}

/// 현재 태스크를 큐 꼬리로 돌리고 다음 태스크를 실행하는 함수입니다.
///
/// 라운드 로빈 전환점이며 YIELD와 타이머 선점이 공유합니다. 준비된 다른
/// 태스크가 없으면 아무 일도 하지 않고 현재 태스크가 계속됩니다.
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트에서 벡터의 컨텍스트 저장이 끝난 뒤에만 호출해야
/// 합니다.
pub unsafe fn rotate() {
    // SAFETY: 함수 계약대로 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    // SAFETY: 큐 링크는 이 모듈만 만지므로 유효함
    let next = unsafe { pop(s) };
    if next == 0 {
        return;
    }
    let cur = s.current;
    // SAFETY: current는 handoff/switch_to가 유지하는 유효한 TCB이고 next는
    //         방금 큐에서 나옴
    unsafe {
        push(s, cur as *mut Tcb);
        switch_to(s, next);
    }
}

/// 현재 실행 중인 태스크의 TCB 포인터를 주는 함수입니다.
///
/// # Safety
/// 이양 후의 시스템 콜/IRQ 컨텍스트에서만 호출해야 합니다(그 전에는 0).
pub unsafe fn current() -> *mut Tcb {
    // SAFETY: 함수 계약대로 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    s.current as *mut Tcb
}

/// 블록 표시를 마친 현재 태스크를 두고 다음 태스크로 전환하는 함수입니다.
///
/// 현재 태스크를 준비 큐에 되돌리지 않는 것이 [rotate]와의 차이입니다.
/// 호출 전에 현재 태스크를 깨울 주체(엔드포인트 대기 큐나 수신자의 reply
/// 링크)에 걸어 두어야 유실되지 않습니다.
///
/// 블록된 채로 기다리던 현재 태스크 자신이 준비 큐에 돌아와 있으면 그
/// 태스크로 전환합니다(같은 컨텍스트로 복귀).
///
/// # Errors
/// 준비된 태스크가 없으면 false를 반환합니다. 호출자는 다음 인터럽트를
/// 기다렸다가 다시 불러야 합니다
///
/// # Safety
/// [rotate]와 동일합니다.
pub unsafe fn block_and_switch() -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    // SAFETY: 큐 링크는 유효함
    let next = unsafe { pop(s) };
    if next == 0 {
        return false;
    }
    // SAFETY: next는 방금 큐에서 나온 유효한 TCB임
    unsafe { switch_to(s, next) };
    true
}

/// 준비 큐에 있는 TCB를 큐에서 빼내는 함수입니다.
///
/// 준비 상태의 태스크가 종료 처리될 때 씁니다. 상태는 호출자가 바꿉니다.
///
/// # Errors
/// 큐 링크가 어긋나 있으면 false를 반환합니다. 큐 손상이므로 호출자는
/// fail-secure 정지해야 합니다
///
/// # Safety
/// 시스템 콜 컨텍스트여야 하고 `tcb`는 준비 큐에 든 유효한 TCB여야 합니다.
#[must_use]
pub unsafe fn remove_ready(tcb: *mut Tcb) -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    // SAFETY: 함수 계약을 큐로 중계함
    unsafe { s.ready.remove(tcb) }
}

/// 현재 태스크가 이번 커널 진입 중에 종료됐는지 확인하는 함수입니다.
///
/// # Safety
/// 이양 후의 시스템 콜/IRQ 컨텍스트에서만 호출해야 합니다.
pub unsafe fn current_is_dead() -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적이고 current는 유효한 TCB임. 종료된
    //         TCB의 메모리도 이번 커널 진입이 끝날 때까지는 재사용되지 않음
    unsafe {
        let s = &mut *SCHED.0.get();
        (*(s.current as *mut Tcb)).state == TaskState::Dead
    }
}
