//! 콘솔 입력을 읽는 시스템 콜(CONSOLE_READ)과 그 대기자를 다루는 모듈입니다.
//!
//! # Features
//! 커널을 `console-input`으로 빌드한 이미지에만 있습니다. 바이트를 받는 일은
//! `k0_arch::conin`(수신 로직과 UART)이 하고, 이 모듈은 누가 읽을 수 있는지와
//! 기다리는 태스크를 다룹니다.
//!
//! 읽으려면 읽기 권한(R)이 있는 Console 케이퍼빌리티를 제시해야 합니다. 받아
//! 둔 바이트가 없으면 태스크는 `BlockedConsole` 상태로 잠들고, 대기자 자리는
//! 하나뿐이라 그동안 다른 태스크의 읽기는 BUSY로 끝납니다. 기한은 IPC와 같은
//! 기한 목록을 씁니다.
//!
//! 수신 인터럽트의 핸들러는 링에 바이트를 옮기기만 하고 스케줄러를 건드리지
//! 않습니다. 대기자를 깨우는 일은 [wake]가 하고, 커널이 스케줄러에 배타적으로
//! 접근할 수 있는 문맥(EL0에서 들어온 인터럽트의 끝, 대기 루프, 틱)에서
//! 부릅니다. 깨울 때는 대기자가 그 케이퍼빌리티를 아직 쥐고 있는지 다시
//! 확인하므로, 기다리는 동안 권한을 잃은 태스크는 바이트를 받지 못합니다.
//! 깨어난 태스크는 준비 큐의 끝에 섭니다(선점하지 않음).

use core::cell::UnsafeCell;
use core::fmt::Write;

use k0_abi::{console as flag, err};
use k0_arch::conin;
use k0_arch::earlycon::EarlyCon;
use k0_arch::usermode::Context;
use k0_cap::{rights, Cap};
use k0_task::{TaskState, Tcb};

/// 콘솔 입력을 기다리는 태스크(커널 VA, 0 = 없음)를 담는 자리입니다.
struct Waiter(UnsafeCell<u64>);

// SAFETY: 단일 코어이고 이 모듈의 함수는 전부 인터럽트가 마스크된 커널
//         문맥에서만 불리므로 동시에 접근하지 않음
unsafe impl Sync for Waiter {}

static WAITER: Waiter = Waiter(UnsafeCell::new(0));

fn waiter() -> *mut Tcb {
    // SAFETY: 위의 Sync 근거대로 이 접근은 배타적임
    unsafe { *WAITER.0.get() as *mut Tcb }
}

fn set_waiter(t: *mut Tcb) {
    // SAFETY: 위의 Sync 근거대로 이 접근은 배타적임
    unsafe { *WAITER.0.get() = t as u64 };
}

/// 태스크의 CNode에서 슬롯을 찾아 읽기 권한이 있는 Console인지 확인하는
/// 함수입니다.
///
/// # Errors
/// 범위 밖이거나 빈 슬롯은 BAD_SLOT, Console이 아니면 BAD_CAP, 읽기 권한이
/// 없으면 NO_RIGHTS
///
/// # Safety
/// `t`는 유효한 TCB여야 하고 접근이 배타적이어야 합니다.
unsafe fn readable(t: *mut Tcb, slot: u64) -> Result<(), i64> {
    // SAFETY: 함수 계약대로 접근이 배타적이고 t는 유효한 TCB임
    let cnode = match unsafe { Tcb::cspace(t) } {
        Ok(Some(c)) => c,
        Ok(None) => return Err(err::BAD_SLOT),
        Err(_) => k0_arch::fatal("cap slot corrupt (cspace)"),
    };
    let Some(s) = cnode.slot(slot) else {
        return Err(err::BAD_SLOT);
    };
    // SAFETY: s는 살아 있는 CNode의 범위 안 슬롯임
    match unsafe { k0_cap::read(s) } {
        Ok(Some(k0_cap::Entry {
            cap: Cap::Console { rights },
            ..
        })) => {
            if rights & rights::R == 0 {
                return Err(err::NO_RIGHTS);
            }
            Ok(())
        }
        Ok(Some(_)) => Err(err::BAD_CAP),
        Ok(None) => Err(err::BAD_SLOT),
        Err(_) => k0_arch::fatal("cap slot corrupt (console)"),
    }
}

fn deliver(ctx: &mut Context, byte: u8, lost: bool) {
    ctx.x[0] = 0;
    ctx.x[1] = u64::from(byte);
    ctx.x[2] = if lost { flag::LOST } else { 0 };
}

/// CONSOLE_READ 시스템 콜 처리 함수입니다.
///
/// 반환 레지스터는 이 함수가 직접 씁니다(블록되는 경우에는 깨우는 쪽이 씀).
/// 호출한 태스크가 블록됐는데 준비된 태스크가 없으면 false를 돌려줍니다.
///
/// 권한 확인을 통과하고 입력 경로가 있는 첫 호출에서 주기적인 틱 줄을 멈추고
/// 수신 인터럽트를 켭니다. 그 전에 장치에 쌓인 바이트는 버려집니다.
///
/// # Arguments
/// `ctx` - 호출한 태스크의 컨텍스트(x0 = Console 슬롯, x1 = 플래그)
#[must_use]
pub fn sys_console_read(ctx: &mut Context) -> bool {
    let (slot, flags) = (ctx.x[0], ctx.x[1]);
    let fail = |ctx: &mut Context, e: i64| {
        ctx.x[0] = e as u64;
        true
    };
    // SAFETY: 이양 후의 시스템 콜 컨텍스트(단일 코어, DAIF 마스크)라 접근이
    //         배타적이고 current는 유효한 TCB임
    let cur = unsafe { k0_sched::current() };
    // SAFETY: 위와 같음
    if let Err(e) = unsafe { readable(cur, slot) } {
        return fail(ctx, e);
    }
    if flags & flag::RESERVED != 0 {
        return fail(ctx, err::BAD_FLAGS);
    }
    if !conin::available() {
        return fail(ctx, err::NO_DEVICE);
    }
    if k0_arch::irq::quiet_ticks() {
        let _ = writeln!(EarlyCon, "k0: tick lines off (console input in use)");
    }
    conin::open();
    if !waiter().is_null() {
        return fail(ctx, err::BUSY);
    }
    if flags & flag::FLUSH != 0 {
        conin::flush();
    }
    if let Some((b, lost)) = conin::pop() {
        deliver(ctx, b, lost);
        return true;
    }
    if flags & flag::NONBLOCK != 0 {
        return fail(ctx, err::WOULD_BLOCK);
    }
    // 잠들기 전에 장치를 한 번 들여다봄. 인터럽트가 알려 주지 않은 바이트가
    // FIFO에 남아 있으면 여기서 가져옴
    conin::poll();
    if let Some((b, lost)) = conin::pop() {
        deliver(ctx, b, lost);
        return true;
    }
    // SAFETY: 접근이 배타적이고 cur는 실행 중인 유효한 TCB라 어떤 큐에도 없음
    unsafe {
        (*cur).state = TaskState::BlockedConsole;
        // 깨울 때 권한을 다시 확인할 슬롯 번호
        (*cur).blocked_on = slot;
        set_waiter(cur);
        k0_sched::arm_timeout(cur, crate::deadline_of(flags));
        k0_sched::block_and_switch()
    }
}

/// 콘솔 입력을 기다리는 태스크가 있는지 알려 주는 함수입니다.
pub fn waiting() -> bool {
    !waiter().is_null()
}

/// 기다리는 태스크를 깨울 수 있으면 깨우는 함수입니다.
///
/// 바이트가 도착했으면 그것을 전달하고, 입력 경로가 꺼졌으면 NO_DEVICE로,
/// 대기자가 그사이 케이퍼빌리티를 잃었으면 바이트를 쓰지 않고 REVOKED로
/// 깨웁니다. 깨울 까닭이 없으면 아무 일도 하지 않습니다.
///
/// # Arguments
/// `current_ctx` - 현재 태스크의 컨텍스트가 살아 있는 참조. 대기 루프에서는
///                 블록된 현재 태스크가 대기자일 수 있고, 그때는 반환값을 이
///                 참조로 써야 함. 실행 중이던 태스크를 끊고 들어온 문맥에서는
///                 `None`
///
/// # Safety
/// 스케줄러에 배타적으로 접근할 수 있는 커널 문맥(시스템 콜, EL0에서 들어온
/// 인터럽트의 끝, 대기 루프)에서만 불러야 합니다.
pub unsafe fn wake(mut current_ctx: Option<&mut Context>) {
    let t = waiter();
    if t.is_null() {
        return;
    }
    // SAFETY: 함수 계약대로 접근이 배타적이고, 대기자 자리에 있는 TCB는
    //         teardown이 빼기 전까지 살아 있는 BlockedConsole 태스크임
    unsafe {
        if (*t).state != TaskState::BlockedConsole {
            k0_arch::fatal("console waiter is not waiting");
        }
        let held = readable(t, (*t).blocked_on).is_ok();
        let off = !conin::available();
        let result = if !held {
            Some((err::REVOKED as u64, 0, 0))
        } else if off {
            Some((err::NO_DEVICE as u64, 0, 0))
        } else {
            conin::pop().map(|(b, lost)| (0, u64::from(b), if lost { flag::LOST } else { 0 }))
        };
        let Some((x0, x1, x2)) = result else {
            return;
        };
        let write = |c: &mut Context| {
            c.x[0] = x0;
            if x0 == 0 {
                c.x[1] = x1;
                c.x[2] = x2;
            }
        };
        if t == k0_sched::current() {
            match current_ctx.as_deref_mut() {
                Some(c) => write(c),
                None => k0_arch::fatal("running task is the console waiter"),
            }
        } else {
            write(&mut (*t).ctx);
        }
        (*t).blocked_on = 0;
        set_waiter(core::ptr::null_mut());
        k0_sched::enqueue(t);
    }
}

/// 기한이 지난 대기자를 대기자 자리에서 빼는 함수입니다. 반환값을 쓰고 준비
/// 큐에 넣는 일은 부른 쪽([crate::expire_timeouts])이 합니다.
///
/// # Safety
/// [wake]와 같고, `t`는 `BlockedConsole` 상태여야 합니다.
pub(crate) unsafe fn drop_waiter(t: *mut Tcb) {
    if waiter() != t {
        k0_arch::fatal("console waiter slot corrupt");
    }
    // SAFETY: 함수 계약대로 t는 유효한 TCB임
    unsafe { (*t).blocked_on = 0 };
    set_waiter(core::ptr::null_mut());
}
