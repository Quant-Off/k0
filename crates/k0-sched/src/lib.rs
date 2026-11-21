//! 시간 분할 스케줄러의 커널 쪽 크레이트입니다.
//!
//! # Features
//! 판정은 전부 k0-part의 `Core`가 합니다. 어느 도메인이 도는지, 틱이 현재
//! 태스크를 끊는지, 창의 경계에서 무엇을 하는지가 거기에 있고 호스트에서
//! 시험합니다. 이 크레이트는 그 판정이 다루는 실물을 맡습니다. 도메인마다 하나씩
//! 있는 준비 큐와 기한 목록, 현재 태스크의 설치(`__current_context` 포인터
//! 갱신과 TTBR0 교체)입니다. 커널은 사건의 이름으로만 알립니다([handoff],
//! [rotate], [block_and_switch], [on_tick], [poll], [set_table]). CPU를 받은
//! 경위나 "틱이 지나갔는지"를 커널이 골라 넘기는 자리는 없습니다.
//!
//! 커널 heap 없이 TCB에 내장된 링크(intrusive list)로 큐를 만듭니다. 큐 노드
//! 할당이 없어 큐 길이가 태스크 수로 자연히 상한되므로 스케줄러가 커널 자원
//! 고갈의 원인이 되지 못합니다. 이양(handoff) 후 커널은 예외 컨텍스트에서만
//! 실행되고, 벡터가 사용자 컨텍스트를 현재 TCB에 저장한 상태에서 전환이
//! 일어나므로 전환은 포인터 갱신과 TTBR0 교체(주소 공간이 다를 때만, ASID
//! 미사용이라 전체 TLB 무효화 동반)로 끝납니다. 도메인이 바뀔 때는 주소 공간이
//! 같아도 TLB를 비웁니다.
//!
//! 도메인에 관한 규칙은 다음과 같습니다.
//!
//! - 깨어나는 태스크는 자기 도메인의 큐에만 들어갑니다([enqueue]가 TCB에서
//!   도메인을 읽음). 다른 도메인이 도는 중에 깨어나도 실행은 자기 창에서
//!   합니다.
//! - 기한 목록도 도메인마다 따로 있고, 기한이 지났는지는 그 도메인이 도는
//!   틱에서만 봅니다([expired]). 그래서 타임아웃은 늦게 올 수는 있어도 일찍
//!   오지는 않고, 한 도메인의 대기자 수가 다른 도메인의 틱 처리 시간을 바꾸지
//!   못합니다.
//! - 도는 도메인에 준비된 태스크가 없으면 커널은 기다립니다. 다른 도메인의
//!   태스크를 대신 실행하지 않습니다.
//!
//! 상태는 둘로 나뉩니다. `CORE`(판정 상태)는 사건 함수만 만지고, 한 사건이
//! 끝나기 전에 다른 사건이 들어오면 정지합니다. `Q`(큐, 기한 목록, 현재
//! 태스크)는 함수 하나가 도는 동안만 빌립니다. 틱 사건 도중에 도는 도메인의
//! 일(콘솔 깨우기, 타임아웃)을 하는 콜백이 [enqueue]로 다시 들어오기 때문에,
//! 콜백을 부르는 동안에는 `Q`에 대한 참조를 들고 있지 않습니다.
//!
//! 지켜지는 불변식은 다음과 같습니다.
//!
//! - 상태가 Running인 TCB는 `Q.current`이고 `Core`의 실행 중 태스크입니다.
//! - `Core`에 실행 중 태스크가 없으면(커널이 기다리는 중) Running인 TCB도
//!   없습니다. 그동안 `Q.current`는 마지막으로 설치한 태스크를 가리킨 채
//!   남는데, "이 태스크가 현재 태스크인가"의 판정과 벡터가 복원할 컨텍스트로만
//!   쓰입니다. `current`를 바꾸는 자리는 `Env::run` 하나입니다.
//! - TCB의 도메인은 그 TCB가 어느 목록에든 들어 있는 동안 바뀌지 않습니다
//!   (구성은 Inactive에서만, 도메인 지정은 Stopped에서만 허용됨).
//!
//! # Errors
//! 전환 대상이 없으면 현재 태스크가 그대로 계속됩니다(에러 아님). 현재
//! 태스크가 종료됐거나 블록됐는데 그 도메인에 준비된 태스크가 없으면
//! [block_and_switch]가 false를 반환하고, 호출자(커널 정책)는 다음 인터럽트를
//! 기다렸다가 [poll]을 부릅니다. 기한 목록의 링크가 어긋나 있거나, 태스크가
//! 자기 도메인이 아닌 큐에 들어가려 하거나, 사건이 겹쳐 들어오면
//! `k0_arch::fatal`로 정지합니다.

#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use k0_part::{Core, Env, Next, MAX_DOMAINS};
use k0_task::{TaskState, Tcb, TcbQueue};

pub use k0_part::{Domain, Table, TableError};

/// 큐와 기한 목록, 설치된 태스크를 담는 구조체입니다. 값은 TCB의 커널
/// VA(0 = 없음)입니다.
struct Queues {
    /// 컨텍스트가 설치된 태스크. 커널이 기다리는 동안에는 Running이 아닐 수
    /// 있음
    current: u64,
    /// 도메인별 준비 큐
    ready: [TcbQueue; MAX_DOMAINS],
    /// 도메인별 기한 목록의 머리
    timed: [u64; MAX_DOMAINS],
    /// 목록에 든 기한 중 가장 이른 값의 하한. 대기자가 빠질 때는 고치지
    /// 않으므로 실제보다 이를 수 있고(그러면 한 번 헛되이 훑을 뿐), 늦을 수는
    /// 없음
    timed_min: [u64; MAX_DOMAINS],
}

struct SyncCell<T>(UnsafeCell<T>);

/// # Safety
/// 부트 코어의 초기화 시퀀스와 이양 후의 예외 컨텍스트(단일 코어, 예외
/// 진입 시 DAIF 마스크)에서만 접근하므로 동시 접근이 없습니다.
unsafe impl<T> Sync for SyncCell<T> {}

static Q: SyncCell<Queues> = SyncCell(UnsafeCell::new(Queues {
    current: 0,
    ready: [const { TcbQueue::new() }; MAX_DOMAINS],
    timed: [0; MAX_DOMAINS],
    timed_min: [0; MAX_DOMAINS],
}));

static CORE: SyncCell<Core<u64>> = SyncCell(UnsafeCell::new(Core::new()));

/// 사건 하나가 `CORE`를 쥐고 있는지
static IN_EVENT: AtomicBool = AtomicBool::new(false);

/// `CORE`를 쥐는 동안 살아 있는 표식 구조체입니다.
///
/// 참조를 만들기 전에 표식부터 세우므로, 사건이 겹쳐 들어와도 같은 상태에
/// 대한 두 번째 참조가 생기기 전에 정지합니다. 놓는 것은 Drop이 합니다.
struct Event(());

impl Event {
    fn begin() -> Self {
        if IN_EVENT.swap(true, Ordering::Relaxed) {
            k0_arch::fatal("scheduler event re-entered");
        }
        Self(())
    }

    fn core(&mut self) -> &mut Core<u64> {
        // SAFETY: 표식을 쥔 동안에는 다른 사건이 CORE에 닿지 못하고, 돌려주는
        //         참조의 수명이 이 표식의 빌림에 묶여 한 번에 하나만 존재함
        unsafe { &mut *CORE.0.get() }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        IN_EVENT.store(false, Ordering::Relaxed);
    }
}

/// TCB가 속한 도메인을 읽는 함수입니다.
///
/// # Safety
/// `tcb`는 유효한 TCB여야 합니다.
pub unsafe fn domain_of(tcb: *mut Tcb) -> Domain {
    // SAFETY: 함수 계약대로 tcb는 유효함
    match Domain::new(unsafe { (*tcb).domain }) {
        Some(d) => d,
        None => k0_arch::fatal("task domain out of range"),
    }
}

/// `Core`가 큐와 현재 태스크를 다루는 통로 구조체입니다.
///
/// 태스크 값은 TCB의 커널 VA입니다. `Core`가 넘겨주는 값은 [handoff]가 준 루트
/// TCB와 이 구조체의 `pop`이 큐에서 꺼낸 TCB뿐이라서 언제나 유효한 TCB입니다.
struct Kenv<'a> {
    /// 도는 도메인의 일(콘솔 깨우기, 타임아웃)을 하는 콜백. 틱과 대기 루프의
    /// 사건에만 있음
    serve: Option<&'a mut dyn FnMut(Domain, u64)>,
}

impl Kenv<'_> {
    const fn bare() -> Self {
        Self { serve: None }
    }

    /// 태스크를 자기 도메인의 큐에 넣기 전에 확인하는 함수입니다.
    fn check_home(d: Domain, t: u64) {
        // SAFETY: 구조체 설명대로 t는 유효한 TCB임
        if unsafe { domain_of(t as *mut Tcb) } != d {
            k0_arch::fatal("task queued outside its domain");
        }
    }
}

impl Env<u64> for Kenv<'_> {
    fn now(&self) -> u64 {
        k0_arch::irq::ticks()
    }

    fn domain(&self, t: u64) -> Domain {
        // SAFETY: 구조체 설명대로 t는 유효한 TCB임
        unsafe { domain_of(t as *mut Tcb) }
    }

    fn push(&mut self, d: Domain, t: u64) {
        Self::check_home(d, t);
        // SAFETY: 예외 컨텍스트라 접근이 배타적이고, 이 빌림은 이 함수 안에서
        //         끝남. t는 유효하고 큐 밖에 있음(Core가 막 끊었거나 깨어남)
        unsafe { push(&mut *Q.0.get(), d, t as *mut Tcb) };
    }

    fn push_front(&mut self, d: Domain, t: u64) {
        Self::check_home(d, t);
        let tcb = t as *mut Tcb;
        // SAFETY: push와 같음
        unsafe {
            let q = &mut *Q.0.get();
            (*tcb).state = TaskState::Ready;
            q.ready[d.index()].push_front(tcb);
        }
    }

    fn pop(&mut self, d: Domain) -> Option<u64> {
        // SAFETY: 접근이 배타적이고 큐 링크는 이 모듈만 만지므로 유효함
        let t = unsafe { (*Q.0.get()).ready[d.index()].pop() } as u64;
        (t != 0).then_some(t)
    }

    /// 현재 태스크를 바꾸는 유일한 자리입니다. 벡터의 복귀 경로
    /// (`__user_restore`)가 `__current_context`를 다시 읽기 때문에 포인터
    /// 갱신만으로 전환이 완성됩니다.
    fn run(&mut self, t: u64) {
        let tcb = t as *mut Tcb;
        // SAFETY: 접근이 배타적이고 t는 유효한 TCB이며, 앞 태스크의 컨텍스트
        //         저장은 벡터가 끝냄
        unsafe {
            (*tcb).state = TaskState::Running;
            (*Q.0.get()).current = t;
            k0_arch::usermode::set_current(&raw mut (*tcb).ctx);
            // ASID를 쓰지 않으므로 주소 공간이 다를 때만 교체(전체 TLB 무효화 동반)
            if (*tcb).ttbr0_pa != k0_mm::current_user_root() {
                k0_mm::install_user_ttbr0((*tcb).ttbr0_pa);
            }
        }
    }

    fn enter_domain(&mut self, _d: Domain) {
        // 앞 도메인이 남긴 변환 캐시를 다음 도메인이 이어받지 않게 함
        k0_mm::flush_user_tlb();
    }

    fn serve(&mut self, d: Domain, now: u64) {
        if let Some(f) = self.serve.as_mut() {
            f(d, now);
        }
    }
}

/// 사건 하나를 처리한 뒤에 타이머 쪽에 알리는 함수입니다.
///
/// 분할 중인지(틱 줄을 낼지)와, 다음 틱에서 도메인이 바뀌어 인터럽트를 보호
/// 구간만큼 일찍 걸어야 하는지를 알립니다. 틱 사건과 표를 바꾸는 사건 뒤에
/// 부릅니다.
fn publish(core: &Core<u64>) {
    k0_arch::irq::set_partitioned(core.partitioned());
    k0_arch::irq::guard_next(core.next_tick_switches());
}

/// 도메인 `d`의 준비 큐 꼬리에 TCB를 붙이는 함수입니다.
///
/// # Safety
/// `tcb`는 유효한 커널 VA의 TCB이고 큐에 없어야 하며 `d`가 그 TCB의
/// 도메인이어야 합니다.
unsafe fn push(q: &mut Queues, d: Domain, tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 tcb는 유효하고 큐 밖에 있음
    unsafe {
        (*tcb).state = TaskState::Ready;
        q.ready[d.index()].push(tcb);
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
unsafe fn disarm(q: &mut Queues, tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 tcb와 그 이웃은 유효한 TCB임
    unsafe {
        if (*tcb).deadline == 0 {
            return;
        }
        let d = domain_of(tcb).index();
        let me = tcb as u64;
        let next = (*tcb).tnext as *mut Tcb;
        let prev = (*tcb).tprev as *mut Tcb;
        let prev_ok = if prev.is_null() { q.timed[d] == me } else { (*prev).tnext == me };
        let next_ok = next.is_null() || (*next).tprev == me;
        if !prev_ok || !next_ok {
            k0_arch::fatal("timeout list corrupt");
        }
        if prev.is_null() {
            q.timed[d] = next as u64;
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

/// 블록된 태스크에 기한을 걸어 그 도메인의 기한 목록에 넣는 함수입니다.
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
        let q = &mut *Q.0.get();
        if (*tcb).deadline != 0 {
            k0_arch::fatal("timeout armed twice");
        }
        let d = domain_of(tcb).index();
        let head = q.timed[d] as *mut Tcb;
        (*tcb).deadline = deadline;
        (*tcb).tprev = 0;
        (*tcb).tnext = head as u64;
        if head.is_null() {
            q.timed_min[d] = deadline;
        } else {
            (*head).tprev = tcb as u64;
            q.timed_min[d] = q.timed_min[d].min(deadline);
        }
        q.timed[d] = tcb as u64;
    }
}

/// 기한을 건 대기자가 어느 도메인에든 하나라도 있는지 알려 주는 함수입니다.
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 합니다.
pub unsafe fn has_timed() -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적임
    unsafe { (*Q.0.get()).timed.iter().any(|&t| t != 0) }
}

/// 준비 큐에 선 태스크가 어느 도메인에든 하나라도 있는지 알려 주는 함수입니다.
///
/// 분할 중에는 도는 도메인에 준비된 태스크가 없어도 다른 도메인의 창이 오면
/// 실행이 이어집니다. 커널이 영영 기다리게 됐는지는 모든 도메인을 봐야 압니다.
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 합니다.
pub unsafe fn has_ready() -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적임
    unsafe { (*Q.0.get()).ready.iter().any(|q| q.head != 0) }
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
    unsafe { disarm(&mut *Q.0.get(), tcb) };
}

/// 도메인 `d`에서 기한이 지난 대기자를 목록에서 빼면서 하나씩 넘겨 주는
/// 함수입니다.
///
/// 넘겨받은 쪽이 그 태스크를 대기 큐나 응답 링크에서 떼어 내고 [enqueue]로
/// 깨워야 합니다. 콜백이 스케줄러 상태를 다시 빌리므로 이 함수는 콜백을
/// 부르는 동안 스케줄러에 대한 참조를 들고 있지 않습니다.
///
/// 다른 도메인의 목록은 보지 않습니다. 그 도메인의 대기자는 그 도메인이 도는
/// 틱에서 깨어납니다.
///
/// # Arguments
/// `d` - 지금 도는 도메인
/// `now` - 지금의 틱 수
/// `f` - 기한이 지난 TCB마다 한 번 불리는 콜백. 넘겨받은 TCB 외의 태스크를
/// 기한 목록에서 빼면 안 됨
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 합니다.
pub unsafe fn expired(d: Domain, now: u64, mut f: impl FnMut(*mut Tcb)) {
    let d = d.index();
    // SAFETY: 함수 계약대로 접근이 배타적이고 목록의 링크는 유효한 TCB임.
    //         다음 링크는 콜백 전에 읽어 두며, 콜백은 넘겨받은 TCB만 다룸
    unsafe {
        let mut t = {
            let q = &*Q.0.get();
            if q.timed[d] == 0 || now < q.timed_min[d] {
                return;
            }
            q.timed[d] as *mut Tcb
        };
        // 남는 대기자들의 가장 이른 기한을 다시 구함
        let mut earliest = u64::MAX;
        while !t.is_null() {
            let next = (*t).tnext as *mut Tcb;
            let deadline = (*t).deadline;
            if deadline <= now {
                disarm(&mut *Q.0.get(), t);
                f(t);
            } else {
                earliest = earliest.min(deadline);
            }
            t = next;
        }
        (*Q.0.get()).timed_min[d] = earliest;
    }
}

/// 루트 태스크로 제어권을 이양하는 함수입니다. 복귀하지 않습니다.
///
/// 루트 태스크를 실행 중으로 등록하고 벡터의 EL0 경로가 저장/복원할
/// 컨텍스트를 지정한 뒤 EL0로 진입합니다. 이후 커널 코드는 예외로만
/// 실행됩니다. 분할 표는 기본 표(루트 도메인 하나)로 시작합니다.
///
/// # Arguments
/// `tcb` - 스폰을 마친 루트 태스크의 TCB
///
/// # Safety
/// TCB의 주소 공간(TTBR0)이 설치된 뒤에만 호출해야 합니다. 커널의 identity
/// 의존이 남아 있으면 안 됩니다(호출 순서는 kernel_main이 강제).
pub unsafe fn handoff(tcb: &'static mut Tcb) -> ! {
    let t = core::ptr::from_mut(tcb);
    // SAFETY: t는 방금 받은 유효한 TCB임
    if unsafe { domain_of(t) } != Domain::ROOT {
        k0_arch::fatal("root task outside the root domain");
    }
    // 틱 번호를 읽는 것과 EL0 진입 사이에 타이머 인터럽트가 끼면 틱 번호만
    // 오르고 스케줄러는 그 틱을 보지 못함(커널 문맥의 틱은 사건을 만들지
    // 않음). 그러면 첫 틱까지 "마지막으로 처리한 틱"이 틱 번호보다 하나
    // 뒤처져, 그 사이에 정한 표가 한 틱 일찍 시작하고 보호 구간도 걸리지 않음.
    // 먼저 막아 두면 그 틱은 EL0에 들어간 직후에 평소의 틱 사건으로 들어옴
    k0_arch::irq::mask_all();
    {
        let mut ev = Event::begin();
        ev.core().start(&mut Kenv::bare(), t as u64);
    }
    // SAFETY: 함수 계약대로 컨텍스트와 주소 공간이 준비된 발산 경로에서 호출됨
    unsafe { k0_arch::usermode::enter_user() }
}

/// TCB를 자기 도메인의 준비 큐에 넣어 실행 대상으로 만드는 함수입니다.
///
/// 구성(TCB_CONFIGURE)을 마친 태스크의 첫 진입과 IPC 블록에서 깨어나는
/// 태스크가 같은 경로를 씁니다. 깨어나는 태스크에 기한이 걸려 있었다면
/// 여기서 취소되므로, 지나간 대기의 기한이 나중의 대기를 끝내는 일은
/// 없습니다. 큐는 TCB에 적힌 도메인으로 정해지고 부르는 쪽이 고를 수
/// 없습니다.
///
/// # Arguments
/// `tcb` - 실행을 기다릴 TCB의 커널 VA 포인터
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고,
/// `tcb`는 유효한 TCB이며 큐에 없고 실행 중(Running)이 아니어야 합니다.
pub unsafe fn enqueue(tcb: *mut Tcb) {
    // SAFETY: 함수 계약대로 접근이 배타적이고 계약을 disarm과 push로 중계함
    unsafe {
        let q = &mut *Q.0.get();
        disarm(q, tcb);
        push(q, domain_of(tcb), tcb);
    }
}

/// 현재 태스크가 CPU를 내놓는 함수입니다(YIELD).
///
/// 같은 도메인에 준비된 다른 태스크가 있으면 현재 태스크를 큐 꼬리로 돌리고
/// 그 태스크를 실행합니다. 없으면 아무 일도 하지 않고 현재 태스크가 계속되며,
/// 그때는 틱 면제도 생기거나 사라지지 않습니다. 넘겨받은 태스크는 주기의
/// 중간에 들어오므로 처음 맞는 틱 하나를 넘깁니다.
///
/// # Safety
/// 시스템 콜 컨텍스트에서 벡터의 컨텍스트 저장이 끝난 뒤에만 호출해야
/// 합니다.
pub unsafe fn rotate() {
    let mut ev = Event::begin();
    ev.core().yield_now(&mut Kenv::bare());
}

/// EL0에서 들어온 타이머 틱을 처리하는 함수입니다. 실행할 태스크가 있으면
/// true를 줍니다.
///
/// 도는 도메인이 그대로면 그 도메인의 일(`serve`)을 한 뒤 현재 태스크를 끊을지
/// 판정합니다. 틱과 틱 사이에 CPU를 받은 태스크는 처음 맞는 틱 하나를 넘기고,
/// 틱의 판정으로 받은 태스크는 다음 틱에서 끊깁니다. 창의 경계면 현재 태스크에
/// 같은 규칙을 적용해 자기 도메인 큐에 세워 두고, 들어오는 도메인의 일을 한 뒤
/// 그 도메인의 큐 머리를 실행합니다.
///
/// # Arguments
/// `serve` - 도메인 하나의 밀린 일(콘솔 깨우기, 그다음 타임아웃)을 하는
/// 콜백. 그 도메인의 태스크만 깨워야 함
///
/// # Errors
/// 들어온 도메인에 준비된 태스크가 없으면 false를 반환합니다. 끊긴 태스크는
/// 이미 자기 큐에 서 있으므로 호출자는 EL0로 돌아가지 말고 인터럽트를
/// 기다렸다가 [poll]을 불러야 합니다
///
/// # Safety
/// EL0에서 진입한 IRQ의 핸들러에서 벡터의 컨텍스트 저장이 끝난 뒤에만
/// 호출해야 합니다(현재 태스크가 실행 중이던 문맥). `serve`는 이 크레이트의
/// 사건 함수를 부르면 안 됩니다([enqueue]와 기한 함수는 됨).
pub unsafe fn on_tick(serve: &mut dyn FnMut(Domain, u64)) -> bool {
    let mut ev = Event::begin();
    let core = ev.core();
    let next = core.tick(&mut Kenv { serve: Some(serve) });
    publish(core);
    matches!(next, Next::Run(_))
}

/// 실행할 태스크 없이 기다리던 커널이 인터럽트를 받은 뒤에 부르는
/// 함수입니다. 실행할 태스크가 생겼으면 true를 줍니다.
///
/// 기다리는 동안 틱이 지나갔으면 틱 사건과 같습니다. 그때 들어오는 태스크는
/// 주기의 처음에 들어오므로 틱 면제가 없습니다. 틱 없이 깨어났으면(콘솔 입력)
/// 도는 도메인의 일을 하고, 준비된 태스크는 주기의 중간에 들어오는 것이라
/// 면제를 받습니다. 틱이 지나갔는지는 틱 번호를 보고 여기서 판단합니다.
///
/// 도메인이 바뀌는 틱은 보호 구간만큼 일찍 처리됩니다. 들어온 도메인이 비어
/// 있다가 그 구간 안에 입력으로 깨어난 태스크는 틱의 예정 시각에 시작하면서도
/// 틱 사이에 들어온 것으로 쳐서 면제를 받습니다. 틱 바로 뒤에 깨어난 태스크와
/// 같은 대우이고, 그 차이는 깨어난 태스크의 도메인 안에만 남습니다.
///
/// 블록된 채로 기다리던 태스크 자신이 준비 큐에 돌아와 있으면 그 태스크로
/// 전환합니다(같은 컨텍스트로 복귀).
///
/// # Arguments
/// `serve` - [on_tick]과 같음
///
/// # Errors
/// 도는 도메인에 준비된 태스크가 없으면 false를 반환합니다. 호출자는 다음
/// 인터럽트를 기다렸다가 다시 불러야 합니다
///
/// # Safety
/// [on_tick]과 같되, 커널이 실행할 태스크 없이 기다리던 문맥이어야 합니다.
pub unsafe fn poll(serve: &mut dyn FnMut(Domain, u64)) -> bool {
    let mut ev = Event::begin();
    let core = ev.core();
    let next = core.poll(&mut Kenv { serve: Some(serve) });
    publish(core);
    matches!(next, Next::Run(_))
}

/// 현재 설치된 태스크의 TCB 포인터를 주는 함수입니다.
///
/// 시스템 콜과 폴트의 문맥에서는 그 진입을 일으킨 태스크입니다. 커널이 실행할
/// 태스크 없이 기다리는 동안에는 마지막으로 설치된 태스크입니다.
///
/// # Safety
/// 이양 후의 시스템 콜/IRQ 컨텍스트에서만 호출해야 합니다(그 전에는 0).
pub unsafe fn current() -> *mut Tcb {
    // SAFETY: 함수 계약대로 접근이 배타적임
    unsafe { (*Q.0.get()).current as *mut Tcb }
}

/// 블록 표시를 마친(또는 종료된) 현재 태스크를 두고 같은 도메인의 다음
/// 태스크로 전환하는 함수입니다.
///
/// 현재 태스크를 준비 큐에 되돌리지 않는 것이 [rotate]와의 차이입니다.
/// 호출 전에 현재 태스크를 깨울 주체(엔드포인트 대기 큐나 수신자의 reply
/// 링크)에 걸어 두어야 유실되지 않습니다.
///
/// 넘겨받은 태스크는 주기의 중간에 들어오므로 처음 맞는 틱 하나를 넘깁니다.
/// 양보뿐 아니라 블록과 종료도 면제를 주어야, 틱 직전에 블록하는 것으로 뒤에
/// 선 태스크의 몫을 줄이는 길이 막힙니다.
///
/// # Errors
/// 같은 도메인에 준비된 태스크가 없으면 false를 반환합니다. 다른 도메인에
/// 준비된 태스크가 있어도 넘어가지 않습니다. 호출자는 인터럽트를 기다렸다가
/// [poll]을 불러야 합니다
///
/// # Safety
/// [rotate]와 동일합니다.
pub unsafe fn block_and_switch() -> bool {
    let mut ev = Event::begin();
    matches!(ev.core().block(&mut Kenv::bare()), Next::Run(_))
}

/// 준비 큐에 있는 TCB를 큐에서 빼내는 함수입니다.
///
/// 준비 상태의 태스크가 종료 처리될 때 씁니다. 상태는 호출자가 바꿉니다.
/// 창의 경계에서 끊겨 다른 도메인의 큐에 서 있는 태스크도 같은 길로 빠집니다.
///
/// # Errors
/// 큐 링크가 어긋나 있으면 false를 반환합니다. 큐 손상이므로 호출자는
/// fail-secure 정지해야 합니다
///
/// # Safety
/// 시스템 콜 컨텍스트여야 하고 `tcb`는 준비 큐에 든 유효한 TCB여야 합니다.
#[must_use]
pub unsafe fn remove_ready(tcb: *mut Tcb) -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적이고 계약을 큐로 중계함
    unsafe {
        let d = domain_of(tcb).index();
        (*Q.0.get()).ready[d].remove(tcb)
    }
}

/// 현재 태스크가 이번 커널 진입 중에 종료됐는지 확인하는 함수입니다.
///
/// # Safety
/// 이양 후의 시스템 콜/IRQ 컨텍스트에서만 호출해야 합니다.
pub unsafe fn current_is_dead() -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적이고 current는 유효한 TCB임. 종료된
    //         TCB의 메모리도 이번 커널 진입이 끝날 때까지는 재사용되지 않음
    unsafe { (*current()).state == TaskState::Dead }
}

/// 새 분할 표를 정하는 함수입니다.
///
/// 표는 지금 틱의 두 틱 뒤에 0번 항목부터 효력이 생깁니다. 다음 틱은 지금의
/// 계획을 따릅니다. 다만 이 호출이 아직 시작하지 않은 보류 표를 갈아 끼우면
/// 다음 틱의 도메인이 달라질 수 있으므로, 다음 틱을 일찍 받을지를 호출 끝에
/// 다시 정합니다. 그때 이미 보호 구간 안이면 그 틱 한 번은 구간이 짧습니다.
///
/// # Arguments
/// `table` - 형식 검사를 마친 표
///
/// # Safety
/// 시스템 콜 컨텍스트에서만 호출해야 합니다.
pub unsafe fn set_table(table: Table) {
    let mut ev = Event::begin();
    let core = ev.core();
    core.set_table(table);
    publish(core);
}

/// 지금 도는 도메인을 주는 함수입니다.
///
/// # Safety
/// 이양 후의 시스템 콜/IRQ 컨텍스트에서만 호출해야 하고, 사건 함수의 콜백
/// 안에서 부르면 안 됩니다.
pub unsafe fn running_domain() -> Domain {
    Event::begin().core().running_domain()
}

/// 분할 표(또는 보류 중인 표)가 기본 표가 아닌지 알려 주는 함수입니다.
///
/// # Safety
/// [running_domain]과 같습니다.
pub unsafe fn partitioned() -> bool {
    Event::begin().core().partitioned()
}

/// 아직 시작하지 않은 태스크의 도메인을 정하는 함수입니다.
///
/// # Arguments
/// `tcb` - Inactive 또는 Stopped 상태의 TCB
/// `d` - 새 도메인
///
/// # Safety
/// 시스템 콜 컨텍스트여야 하고, `tcb`는 유효한 TCB이며 준비 큐에도 대기 큐에도
/// 기한 목록에도 없어야 합니다(목록은 도메인마다 따로라서, 들어 있는 채로
/// 도메인이 바뀌면 빼낼 때 엉뚱한 목록을 보게 됨).
pub unsafe fn assign(tcb: *mut Tcb, d: Domain) {
    // SAFETY: 함수 계약대로 tcb는 유효하고 어느 목록에도 없음
    unsafe { (*tcb).domain = d.id() };
}
