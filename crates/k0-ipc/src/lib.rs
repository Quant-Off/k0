//! EL0 동기 랑데부 IPC(SEND / RECV / CALL / REPLY_RECV) 크레이트입니다.
//!
//! # Features
//! 커널 메시지 버퍼가 없는 seL4식 랑데부입니다. 메시지는 레지스터
//! (MR0-MR3, x2-x5)로만 오가고 송신자와 수신자의 저장된 컨텍스트 사이를
//! 커널이 직접 복사하므로 커널 자원 고갈 표면과 사용자 메모리 이중 읽기
//! (TOCTOU) 표면이 없습니다. 엔드포인트는 호출한 태스크 자신의 CNode에서만
//! 찾고, 케이퍼빌리티의 권한 비트가 송신(W)과 수신(R)을 따로 허가합니다.
//! 수신자는 송신자가 쓴 케이퍼빌리티의 배지를 x1로 받아 상대를 구별합니다.
//! 배지는 커널이 케이퍼빌리티에서 읽어 전달하므로 송신자가 위조할 수
//! 없습니다. 블록된 태스크는 준비 큐가 아니라 엔드포인트의 대기 큐(TCB
//! intrusive 링크)에 있습니다. 기다리지 않으려면 NONBLOCK 플래그를, 기다림에
//! 끝을 두려면 타임아웃(플래그 레지스터의 상위 32비트, 단위는 틱)을 줍니다.
//! 기한이 지난 대기자는 [expire_timeouts]가 대기 큐나 응답 링크에서 떼어
//! TIMEOUT 에러로 깨웁니다. CALL의 기한은 수신자를 기다리는 동안과 응답을
//! 기다리는 동안에 걸쳐 하나입니다. CALL의 응답 자격은 수신자 TCB의
//! reply_to 1회성 링크로만 존재해 위조와 재사용이 불가능하고, 자격을 잃는
//! 호출자는 NO_REPLY로 깨워 영구 블록을 막습니다.
//!
//! 태스크와 엔드포인트의 종료 처리([teardown], [cancel_endpoint])도 여기에
//! 있습니다. 종료되는 오브젝트를 가리키는 큐 링크와 응답 링크를 전부 끊어,
//! 그 메모리가 재분류된 뒤에 커널이 옛 포인터를 따라가는 일이 없게 합니다.
//!
//! # Errors
//! 호출한 태스크가 블록됐는데 준비된 태스크가 없으면 처리 함수들이 false를
//! 반환합니다. 그때 무엇을 할지(다음 인터럽트까지 대기)는 커널의 몫입니다.
//! 커널 자료구조의 손상은 `k0_arch::fatal`로 즉시 정지합니다.

#![no_std]

#[cfg(feature = "console-input")]
pub mod console;

use k0_arch::usermode::Context;
use k0_cap::{rights, Cap};
use k0_task::{Endpoint, TaskState, Tcb};

/// 호출 태스크의 CNode에서 엔드포인트를 찾고 권한을 확인하는 함수입니다.
///
/// 성공하면 엔드포인트 오브젝트의 커널 별칭 포인터와 그 케이퍼빌리티의
/// 배지를 줍니다.
///
/// # Arguments
/// `slot` - 호출 태스크의 CNode 안의 슬롯 번호
/// `need` - 필요한 권한 비트(송신 W, 수신 R)
///
/// # Errors
/// 범위 밖이거나 빈 슬롯은 BAD_SLOT, 엔드포인트가 아니면 BAD_CAP, 권한이
/// 없으면 NO_RIGHTS
///
/// # Safety
/// 이양 후의 시스템 콜 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야
/// 합니다.
unsafe fn ep_of(slot: u64, need: u8) -> Result<(*mut Endpoint, u64), i64> {
    // SAFETY: 함수 계약대로 접근이 배타적이고 current는 유효한 TCB임
    let cnode = match unsafe { Tcb::cspace(k0_sched::current()) } {
        Ok(Some(c)) => c,
        Ok(None) => return Err(k0_abi::err::BAD_SLOT),
        Err(_) => k0_arch::fatal("cap slot corrupt (cspace)"),
    };
    let Some(s) = cnode.slot(slot) else {
        return Err(k0_abi::err::BAD_SLOT);
    };
    // SAFETY: s는 살아 있는 CNode의 범위 안 슬롯임
    match unsafe { k0_cap::read(s) } {
        Ok(Some(k0_cap::Entry {
            cap:
                Cap::Endpoint {
                    base,
                    rights,
                    badge,
                },
            ..
        })) => {
            if rights & need != need {
                return Err(k0_abi::err::NO_RIGHTS);
            }
            Ok(((base + k0_mm::KERNEL_VA_OFFSET) as *mut Endpoint, badge))
        }
        Ok(Some(_)) => Err(k0_abi::err::BAD_CAP),
        Ok(None) => Err(k0_abi::err::BAD_SLOT),
        Err(_) => k0_arch::fatal("cap slot corrupt (endpoint)"),
    }
}

/// 플래그 레지스터의 타임아웃을 기한(틱 수)으로 바꾸는 함수입니다. 타임아웃이
/// 0이면 기한 없음(0)입니다.
///
/// n틱을 달라고 하면 기한은 지금부터 n+1번째 틱입니다. 다음 틱까지 남은
/// 시간은 호출 시점에 따라 0에 가까울 수 있어서, 하나를 더해야 대기가 틱
/// 주기의 n배보다 짧아지지 않습니다.
///
/// # Arguments
/// `flags` - 호출 태스크가 x1로 준 값
fn deadline_of(flags: u64) -> u64 {
    match flags >> k0_abi::ipc::TIMEOUT_SHIFT {
        0 => 0,
        n => k0_arch::irq::ticks().saturating_add(n).saturating_add(1),
    }
}

/// IPC 시스템 콜의 공통 인자(플래그와 엔드포인트)를 확인하는 함수입니다.
///
/// 플래그를 먼저 봅니다. 뜻이 정해지지 않은 비트가 서 있으면 엔드포인트를
/// 찾기 전에 거부합니다. 성공하면 (플래그, 엔드포인트, 배지)를 줍니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그와 타임아웃)
/// `need` - 필요한 권한 비트(송신 W, 수신 R)
///
/// # Errors
/// 예약 비트가 서 있으면 BAD_FLAGS, 그 밖에는 [ep_of]와 같음
///
/// # Safety
/// [ep_of]와 동일합니다.
unsafe fn args_of(ctx: &Context, need: u8) -> Result<(u64, *mut Endpoint, u64), i64> {
    let flags = ctx.x[1];
    if flags & k0_abi::ipc::RESERVED != 0 {
        return Err(k0_abi::err::BAD_FLAGS);
    }
    // SAFETY: 함수 계약을 중계함
    let (ep, badge) = unsafe { ep_of(ctx.x[0], need) }?;
    Ok((flags, ep, badge))
}

/// 메시지 레지스터를 상대의 저장된 컨텍스트로 옮기는 함수입니다.
///
/// MR0-MR3(x2-x5)만 복사하고 다른 레지스터는 전달하지 않습니다(최소 노출).
/// x0은 성공(0), x1은 호출자가 명시한 배지입니다. 배지는 수신 방향에서만
/// 케이퍼빌리티의 값이고 응답 방향은 항상 0입니다(응답자의 다른 상태가
/// 새지 않게 인자로만 받음).
fn deliver(src: &Context, dst: &mut Context, badge: u64) {
    dst.x[2..=5].copy_from_slice(&src.x[2..=5]);
    dst.x[0] = 0;
    dst.x[1] = badge;
}

/// 보류된 응답 자격을 정리하는 함수입니다.
///
/// `holder`가 응답 없이 종료하거나 새 CALL을 받아 자격을 덮을 때, 응답을
/// 기다리던 호출자를 NO_REPLY 에러로 깨워 영구 블록을 막습니다. 보류된
/// 응답이 없으면 아무 일도 하지 않습니다.
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고
/// `holder`는 유효한 TCB여야 합니다. 호출자가 `holder`의 컨텍스트에 대한
/// 가변 참조를 쥔 채 부를 수 있도록 raw 포인터를 받고 `reply_to` 필드만
/// 건드립니다(컨텍스트와 겹치는 참조를 만들지 않음).
pub unsafe fn abort_reply(holder: *mut Tcb) {
    // SAFETY: 함수 계약대로 holder는 유효한 TCB이고 reply_to는 이 크레이트가
    //         BlockedReply 상태의 유효한 TCB로만 기록함
    unsafe {
        let caller = (*holder).reply_to as *mut Tcb;
        (*holder).reply_to = 0;
        if caller.is_null() {
            return;
        }
        (*caller).reply_from = 0;
        (*caller).ctx.x[0] = k0_abi::err::NO_REPLY as u64;
        k0_sched::enqueue(caller);
    }
}

/// 현재 태스크를 엔드포인트 대기 큐에 세우는 함수입니다.
///
/// 어느 엔드포인트에 줄 서 있는지와 보낼 메시지의 배지를 TCB에 함께 기록해,
/// 태스크가 큐 중간에서 제거될 때(종료/회수) 큐를 O(1)로 고칠 수 있게
/// 합니다.
///
/// # Safety
/// 시스템 콜 컨텍스트여야 하고 `ep`는 유효한 엔드포인트, `cur`는 어느
/// 큐에도 없는 현재 태스크여야 합니다.
unsafe fn wait_on(ep: *mut Endpoint, cur: *mut Tcb, state: TaskState, badge: u64) {
    // SAFETY: 함수 계약대로 접근이 배타적이고 큐 링크는 유효함
    unsafe {
        (*cur).state = state;
        (*cur).blocked_on = ep as u64;
        (*cur).ipc_badge = badge;
        (*ep).push(cur);
    }
}

/// 엔드포인트 대기 큐의 머리를 꺼내는 함수입니다. 줄 서 있던 동안의 기록
/// (엔드포인트, 배지)을 지우고 배지를 함께 돌려줍니다.
///
/// # Safety
/// 시스템 콜 컨텍스트여야 하고 `ep`는 비어 있지 않은 유효한 엔드포인트여야
/// 합니다.
unsafe fn take(ep: *mut Endpoint) -> (*mut Tcb, u64) {
    // SAFETY: 함수 계약대로 접근이 배타적이고 큐 링크는 유효함
    unsafe {
        let t = (*ep).pop();
        let badge = (*t).ipc_badge;
        (*t).blocked_on = 0;
        (*t).ipc_badge = 0;
        (t, badge)
    }
}

/// SEND 시스템 콜 처리 함수입니다.
///
/// 수신자가 대기 중이면 즉시 전달하고, 아니면 송신자가 엔드포인트 대기
/// 큐에서 블록됩니다(NONBLOCK이면 WOULD_BLOCK). 블록 경로의 반환
/// 레지스터는 미래의 수신자가 기록하므로 여기서 덮지 않습니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그와 타임아웃,
/// x2-x5 = MR)
///
/// # Errors
/// false는 호출 태스크가 블록됐고 준비된 태스크가 없다는 뜻입니다
#[must_use]
pub fn sys_send(ctx: &mut Context) -> bool {
    // SAFETY: 시스템 콜 컨텍스트에서만 불림
    let (flags, ep, badge) = match unsafe { args_of(ctx, rights::W) } {
        Ok(v) => v,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    let nonblock = flags & k0_abi::ipc::NONBLOCK != 0;
    // SAFETY: ep는 재분류가 소거하고 별칭 매핑한 전용 프레임이며, 단일
    //         코어의 예외 컨텍스트라 접근이 배타적이고 큐 링크는 유효함
    unsafe {
        let head = (*ep).head as *mut Tcb;
        if !head.is_null() && (*head).state == TaskState::BlockedRecv {
            let (rcv, _) = take(ep);
            deliver(ctx, &mut (*rcv).ctx, badge);
            k0_sched::enqueue(rcv);
            ctx.x[0] = 0;
            return true;
        }
        if nonblock {
            ctx.x[0] = k0_abi::err::WOULD_BLOCK as u64;
            return true;
        }
        let cur = k0_sched::current();
        wait_on(ep, cur, TaskState::BlockedSend, badge);
        k0_sched::arm_timeout(cur, deadline_of(flags));
        k0_sched::block_and_switch()
    }
}

/// RECV 시스템 콜 처리 함수입니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그와 타임아웃)
///
/// # Errors
/// false는 호출 태스크가 블록됐고 준비된 태스크가 없다는 뜻입니다
#[must_use]
pub fn sys_recv(ctx: &mut Context) -> bool {
    // SAFETY: 시스템 콜 컨텍스트에서만 불림
    let (flags, ep, _) = match unsafe { args_of(ctx, rights::R) } {
        Ok(v) => v,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    // SAFETY: sys_send와 동일
    unsafe { recv_inner(ctx, ep, flags) }
}

/// 수신 공통 경로(RECV와 REPLY_RECV의 수신 단계) 함수입니다.
///
/// 대기 중인 송신자가 있으면 즉시 받습니다. CALL 송신자는 응답 대기
/// (BlockedReply)로 전환되고 수신자의 reply_to에 1회성 응답 자격이
/// 기록됩니다. 이미 보류된 응답이 있었다면 그 호출자는 NO_REPLY로
/// 깨웁니다(자격은 항상 최대 하나). 응답 대기로 넘어가는 CALL 송신자의
/// 기한은 그대로 이어집니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트
/// `ep` - 수신할 엔드포인트
/// `flags` - 호출 태스크가 x1로 준 플래그와 타임아웃. 전달이 `ctx`의 x1을
/// 덮어쓰므로 호출자가 미리 읽어 넘김
///
/// # Errors
/// false는 호출 태스크가 블록됐고 준비된 태스크가 없다는 뜻입니다
///
/// # Safety
/// 시스템 콜 컨텍스트여야 하고 `ep`는 유효한 엔드포인트 별칭이어야 합니다.
unsafe fn recv_inner(ctx: &mut Context, ep: *mut Endpoint, flags: u64) -> bool {
    let nonblock = flags & k0_abi::ipc::NONBLOCK != 0;
    // SAFETY: 함수 계약대로 접근이 배타적이고 큐 링크는 유효함
    unsafe {
        let head = (*ep).head as *mut Tcb;
        if !head.is_null() && (*head).state != TaskState::BlockedRecv {
            let (snd, badge) = take(ep);
            deliver(&(*snd).ctx, ctx, badge);
            if (*snd).state == TaskState::BlockedCall {
                let cur = k0_sched::current();
                abort_reply(cur);
                (*snd).state = TaskState::BlockedReply;
                (*snd).reply_from = cur as u64;
                (*cur).reply_to = snd as u64;
            } else {
                (*snd).ctx.x[0] = 0;
                k0_sched::enqueue(snd);
            }
            return true;
        }
        if nonblock {
            ctx.x[0] = k0_abi::err::WOULD_BLOCK as u64;
            return true;
        }
        let cur = k0_sched::current();
        wait_on(ep, cur, TaskState::BlockedRecv, 0);
        k0_sched::arm_timeout(cur, deadline_of(flags));
        k0_sched::block_and_switch()
    }
}

/// CALL 시스템 콜 처리 함수입니다.
///
/// 전송과 응답 대기가 한 번의 트랩으로 원자적이라 응답 창을 놓치는 경합이
/// 없습니다. 수신자에게 전달되는 순간 호출자는 BlockedReply가 되고 응답
/// 자격은 수신자의 reply_to에만 존재합니다. 수신자가 응답 없이 종료하면
/// NO_REPLY로 깨어납니다. 타임아웃은 수신자를 기다리는 동안과 응답을
/// 기다리는 동안 전체에 걸친 기한 하나입니다. 응답을 기다리다 기한이 지나면
/// 수신자가 쥔 응답 자격이 사라지고, 수신자의 뒤늦은 응답은 전달되지
/// 않습니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그와 타임아웃,
/// x2-x5 = MR)
///
/// # Errors
/// false는 호출 태스크가 블록됐고 준비된 태스크가 없다는 뜻입니다
#[must_use]
pub fn sys_call(ctx: &mut Context) -> bool {
    // SAFETY: 시스템 콜 컨텍스트에서만 불림
    let (flags, ep, badge) = match unsafe { args_of(ctx, rights::W) } {
        Ok(v) => v,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    let nonblock = flags & k0_abi::ipc::NONBLOCK != 0;
    // SAFETY: sys_send와 동일
    unsafe {
        let cur = k0_sched::current();
        let head = (*ep).head as *mut Tcb;
        if !head.is_null() && (*head).state == TaskState::BlockedRecv {
            let (rcv, _) = take(ep);
            deliver(ctx, &mut (*rcv).ctx, badge);
            abort_reply(rcv);
            (*rcv).reply_to = cur as u64;
            (*cur).reply_from = rcv as u64;
            (*cur).state = TaskState::BlockedReply;
            k0_sched::enqueue(rcv);
        } else {
            if nonblock {
                ctx.x[0] = k0_abi::err::WOULD_BLOCK as u64;
                return true;
            }
            wait_on(ep, cur, TaskState::BlockedCall, badge);
        }
        k0_sched::arm_timeout(cur, deadline_of(flags));
        k0_sched::block_and_switch()
    }
}

/// REPLY_RECV 시스템 콜 처리 함수입니다.
///
/// 보류된 호출자에게 x2-x5를 응답으로 전달한 뒤(1회성, 자격은 즉시 소멸,
/// 없으면 건너뜀) 이어서 수신 단계로 들어갑니다. 엔드포인트나 플래그가
/// 무효하면 응답을 소비하지 않고 에러만 반환합니다(원자적 실패).
///
/// 타임아웃은 수신 단계에만 적용됩니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그와 타임아웃,
/// x2-x5 = 응답 MR)
///
/// # Errors
/// false는 호출 태스크가 블록됐고 준비된 태스크가 없다는 뜻입니다
#[must_use]
pub fn sys_reply_recv(ctx: &mut Context) -> bool {
    // SAFETY: 시스템 콜 컨텍스트에서만 불림
    let (flags, ep, _) = match unsafe { args_of(ctx, rights::R) } {
        Ok(v) => v,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    // SAFETY: sys_send와 동일하고 reply_to는 BlockedReply의 유효한 TCB임
    unsafe {
        let cur = k0_sched::current();
        let caller = (*cur).reply_to as *mut Tcb;
        if !caller.is_null() {
            (*cur).reply_to = 0;
            (*caller).reply_from = 0;
            deliver(ctx, &mut (*caller).ctx, 0);
            k0_sched::enqueue(caller);
        }
        recv_inner(ctx, ep, flags)
    }
}

/// 기한이 지난 IPC 대기자를 전부 에러로 깨우는 함수입니다.
///
/// 대기자를 그 상태가 말하는 자리에서 떼어 냅니다. 엔드포인트에 줄 서
/// 있었으면 대기 큐에서 빼고 TIMEOUT으로 깨웁니다(메시지는 전달되지 않음).
/// 응답을 기다리고 있었으면 응답을 쥔 태스크의 자격을 지우고 REPLY_TIMEOUT
/// 으로 깨웁니다. 그 태스크의 뒤늦은 응답은 받을 상대가 없어 건너뜁니다.
/// 링크가 상태와 맞지 않으면 종료 처리와 똑같이 손상으로 보고 정지합니다.
/// 틱마다 한 번 부릅니다.
///
/// # Arguments
/// `now` - 지금의 틱 수
/// `current_ctx` - 현재 태스크의 컨텍스트에 대한 참조를 호출자가 쥐고 있으면
/// 그 참조. 실행할 태스크가 없어 대기 중일 때는 현재 태스크 자신이 기한을
/// 맞을 수 있고, 그때 반환값은 이 참조로 기록함(같은 메모리에 대한 두 번째
/// 경로를 만들지 않음)
///
/// # Safety
/// EL0에서 진입한 예외의 핸들러(단일 코어, DAIF 마스크)에서만 호출해야 하고,
/// `current_ctx`가 `Some`이면 현재 태스크의 컨텍스트여야 합니다.
pub unsafe fn expire_timeouts(now: u64, mut current_ctx: Option<&mut Context>) {
    // SAFETY: 함수 계약대로 접근이 배타적이고, 기한 목록에는 블록된 태스크만
    //         있으며 그 상태가 말하는 큐와 링크는 이 크레이트가 유지함
    unsafe {
        let cur = k0_sched::current();
        k0_sched::expired(now, |t| {
            let status = match (*t).state {
                TaskState::BlockedSend | TaskState::BlockedRecv | TaskState::BlockedCall => {
                    let ep = (*t).blocked_on as *mut Endpoint;
                    if ep.is_null() || !(*ep).remove(t) {
                        k0_arch::fatal("endpoint queue corrupt");
                    }
                    (*t).blocked_on = 0;
                    (*t).ipc_badge = 0;
                    k0_abi::err::TIMEOUT as u64
                }
                TaskState::BlockedReply => {
                    let holder = (*t).reply_from as *mut Tcb;
                    if holder.is_null() || (*holder).reply_to != t as u64 {
                        k0_arch::fatal("reply link corrupt");
                    }
                    (*holder).reply_to = 0;
                    (*t).reply_from = 0;
                    k0_abi::err::REPLY_TIMEOUT as u64
                }
                #[cfg(feature = "console-input")]
                TaskState::BlockedConsole => {
                    console::drop_waiter(t);
                    k0_abi::err::TIMEOUT as u64
                }
                TaskState::Inactive
                | TaskState::Stopped
                | TaskState::Ready
                | TaskState::Running
                | TaskState::Dead => k0_arch::fatal("timeout on a task that is not blocked"),
            };
            if t == cur {
                match current_ctx.as_deref_mut() {
                    Some(c) => c.x[0] = status,
                    None => k0_arch::fatal("running task in timeout list"),
                }
            } else {
                (*t).ctx.x[0] = status;
            }
            k0_sched::enqueue(t);
        });
    }
}

/// 태스크 하나를 종료 상태로 만드는 함수입니다. 여러 번 불러도 됩니다.
///
/// 태스크를 가리키는 커널 포인터를 전부 끊습니다. 준비 큐나 엔드포인트
/// 대기 큐에 있으면 빼내고, 응답을 기다리던 중이면 응답을 쥔 태스크의
/// 자격을 지우고, 자신이 응답 자격을 쥐고 있으면 그 호출자를 NO_REPLY로
/// 깨웁니다. 걸려 있던 기한도 취소합니다. 그 뒤 상태를 Dead로 바꾸고 저장된
/// 레지스터를 소거합니다.
/// 분기는 상태만 보고 정하며 포인터가 남아 있는지를 근거로 삼지 않습니다.
///
/// 현재 실행 중인 태스크(Running)는 여기서 다른 태스크로 전환하지 않고
/// 컨텍스트도 건드리지 않습니다(호출자가 그 컨텍스트의 가변 참조를 쥐고
/// 있음). 시스템 콜 종료 경로가 상태를 보고 소거와 전환을 수행합니다.
///
/// # Safety
/// 시스템 콜 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고 `t`는
/// 유효한 TCB 프레임이어야 합니다.
pub unsafe fn teardown(t: *mut Tcb) {
    // SAFETY: 함수 계약대로 접근이 배타적이고, 상태가 말하는 큐와 링크는
    //         이 크레이트와 k0-sched가 유지하는 유효한 TCB / 엔드포인트임
    unsafe {
        let state = (*t).state;
        // 블록된 채로 종료되는 태스크는 깨어나는 경로를 거치지 않으므로 기한을
        // 여기서 취소함
        k0_sched::disarm_timeout(t);
        match state {
            TaskState::Dead => return,
            TaskState::Inactive | TaskState::Stopped | TaskState::Running => {}
            TaskState::Ready => {
                if !k0_sched::remove_ready(t) {
                    k0_arch::fatal("ready queue corrupt");
                }
            }
            TaskState::BlockedSend | TaskState::BlockedRecv | TaskState::BlockedCall => {
                let ep = (*t).blocked_on as *mut Endpoint;
                if ep.is_null() || !(*ep).remove(t) {
                    k0_arch::fatal("endpoint queue corrupt");
                }
            }
            TaskState::BlockedReply => {
                let holder = (*t).reply_from as *mut Tcb;
                if holder.is_null() || (*holder).reply_to != t as u64 {
                    k0_arch::fatal("reply link corrupt");
                }
                (*holder).reply_to = 0;
            }
            // 콘솔 입력을 기다리던 태스크는 대기자 자리를 비워, 그 TCB의
            // 메모리가 재분류된 뒤에 커널이 옛 포인터로 깨우는 일이 없게 함
            #[cfg(feature = "console-input")]
            TaskState::BlockedConsole => console::drop_waiter(t),
        }
        abort_reply(t);
        (*t).state = TaskState::Dead;
        (*t).next = 0;
        (*t).prev = 0;
        (*t).blocked_on = 0;
        (*t).reply_from = 0;
        (*t).ipc_badge = 0;
        (*t).ttbr0_pa = 0;
        if state != TaskState::Running {
            (&raw mut (*t).ctx).write(Context::zeroed());
        }
    }
}

/// 종료되는 엔드포인트의 대기 큐를 비우는 함수입니다.
///
/// 줄 서 있던 태스크를 전부 REVOKED 에러로 깨웁니다. 반환 뒤에는 어떤
/// TCB도 이 엔드포인트를 가리키지 않습니다.
///
/// # Safety
/// 시스템 콜 컨텍스트에서만 호출해야 하고 `ep`는 유효한 엔드포인트
/// 프레임이어야 합니다.
pub unsafe fn cancel_endpoint(ep: *mut Endpoint) {
    // SAFETY: 함수 계약대로 접근이 배타적이고 큐 링크는 유효함. 큐에 있는
    //         태스크는 블록 상태라 현재 태스크가 아니므로 컨텍스트 기록이
    //         다른 참조와 겹치지 않음
    unsafe {
        while (*ep).head != 0 {
            let (t, _) = take(ep);
            (*t).ctx.x[0] = k0_abi::err::REVOKED as u64;
            k0_sched::enqueue(t);
        }
    }
}
