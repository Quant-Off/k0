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
//! # Errors
//! 전환 대상이 없으면 현재 태스크가 그대로 계속됩니다(에러 아님). 마지막
//! 태스크의 종료는 [exit_current]가 false를 반환해 호출자(커널 정책)가
//! fail-secure 파킹하게 합니다.

#![no_std]

use core::cell::UnsafeCell;

use k0_task::{TaskState, Tcb};

/// 스케줄러 상태 구조체입니다. 값은 전부 TCB의 커널 VA(0 = 없음)입니다.
struct Sched {
    current: u64,
    head: u64,
    tail: u64,
}

struct SyncCell(UnsafeCell<Sched>);

/// # Safety
/// 부트 코어의 초기화 시퀀스와 이양 후의 예외 컨텍스트(단일 코어, 예외
/// 진입 시 DAIF 마스크)에서만 접근하므로 동시 접근이 없습니다.
unsafe impl Sync for SyncCell {}

static SCHED: SyncCell = SyncCell(UnsafeCell::new(Sched {
    current: 0,
    head: 0,
    tail: 0,
}));

/// 준비 큐 꼬리에 TCB를 붙이는 함수입니다.
///
/// # Safety
/// `tcb`는 유효한 커널 VA의 TCB이고 큐에 없어야 합니다.
unsafe fn push(s: &mut Sched, tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 tcb는 유효하고 큐 밖에 있음
    unsafe {
        (*tcb).state = TaskState::Ready;
        (*tcb).next = 0;
        if s.tail == 0 {
            s.head = tcb as u64;
        } else {
            (*(s.tail as *mut Tcb)).next = tcb as u64;
        }
    }
    s.tail = tcb as u64;
}

/// 준비 큐 머리를 꺼내는 함수입니다. 비어 있으면 0
///
/// # Safety
/// 큐의 링크들이 유효한 TCB를 가리켜야 합니다.
unsafe fn pop(s: &mut Sched) -> u64 {
    let head = s.head;
    if head == 0 {
        return 0;
    }
    // SAFETY: 함수 계약대로 head는 유효한 TCB임
    unsafe {
        s.head = (*(head as *mut Tcb)).next;
        (*(head as *mut Tcb)).next = 0;
    }
    if s.head == 0 {
        s.tail = 0;
    }
    head
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
/// 태스크가 같은 경로를 씁니다.
///
/// # Arguments
/// `tcb` - 실행을 기다릴 TCB의 커널 VA 포인터
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고,
/// `tcb`는 유효한 TCB이며 큐에 없고 현재 태스크가 아니어야 합니다.
pub unsafe fn enqueue(tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    // SAFETY: 함수 계약을 push로 중계함
    unsafe { push(s, tcb) };
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
/// # Errors
/// 준비된 태스크가 없으면 false를 반환합니다. 전 태스크가 블록된 교착이므로
/// 호출자는 fail-secure 파킹해야 합니다
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

/// 현재 태스크를 종료하고 다음 태스크로 넘어가는 함수입니다.
///
/// # Errors
/// 마지막 태스크였다면 false를 반환하며, 호출자는 fail-secure 파킹해야
/// 합니다
///
/// # Safety
/// [rotate]와 동일합니다.
pub unsafe fn exit_current() -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적임
    let s = unsafe { &mut *SCHED.0.get() };
    // SAFETY: current는 유효한 TCB임
    unsafe { (*(s.current as *mut Tcb)).state = TaskState::Dead };
    // SAFETY: 큐 링크는 유효함
    let next = unsafe { pop(s) };
    if next == 0 {
        return false;
    }
    // SAFETY: next는 방금 큐에서 나온 유효한 TCB임
    unsafe { switch_to(s, next) };
    true
}
