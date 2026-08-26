//! EL0 동기 랑데부 IPC(SEND / RECV / CALL / REPLY_RECV) 크레이트입니다.
//!
//! # Features
//! 커널 메시지 버퍼가 없는 seL4식 랑데부입니다. 메시지는 레지스터
//! (MR0-MR3, x2-x5)로만 오가고 송신자와 수신자의 저장된 컨텍스트 사이를
//! 커널이 직접 복사하므로 커널 자원 고갈 표면과 사용자 메모리 이중 읽기
//! (TOCTOU) 표면이 없습니다. 블록된 태스크는 준비 큐가 아니라 엔드포인트의
//! 대기 큐(TCB intrusive 링크)에 있고, 타임아웃은 없으며 NONBLOCK 플래그가
//! 유일한 비대기 수단입니다. CALL의 응답 자격은 수신자 TCB의 reply_to
//! 1회성 링크로만 존재해 위조와 재사용이 불가능하고, 자격을 잃는 호출자는
//! NO_REPLY로 깨워 영구 블록을 막습니다.
//!
//! # Errors
//! 전 태스크가 블록되면 깨울 주체가 없으므로(알림 오브젝트는 다음
//! 슬라이스) 교착입니다. 처리 함수들이 false를 반환해 보고하고, 파킹
//! 정책(콘솔 출력 포함)은 커널의 몫입니다.

#![no_std]

use k0_arch::usermode::Context;
use k0_task::{Endpoint, TaskState, Tcb};

/// 슬롯에서 엔드포인트 오브젝트의 커널 별칭 포인터를 얻는 함수입니다.
///
/// # Errors
/// 엔드포인트가 아닌 케이퍼빌리티는 BAD_CAP, 범위 밖 슬롯은 BAD_SLOT
fn ep_of(slot: u64) -> Result<*mut Endpoint, i64> {
    let Ok(slot) = usize::try_from(slot) else {
        return Err(k0_abi::err::BAD_SLOT);
    };
    // SAFETY: 단일 코어의 시스템 콜 컨텍스트(DAIF 마스크)라 접근이 배타적임
    let cnode = unsafe { k0_cap::root_mut() };
    match cnode.cap(slot) {
        Some(k0_cap::Cap::Endpoint { base }) => {
            Ok((base + k0_mm::KERNEL_VA_OFFSET) as *mut Endpoint)
        }
        Some(_) => Err(k0_abi::err::BAD_CAP),
        None => Err(k0_abi::err::BAD_SLOT),
    }
}

/// 메시지 레지스터를 상대의 저장된 컨텍스트로 옮기는 함수입니다.
///
/// MR0-MR3(x2-x5)만 복사하고 다른 레지스터는 전달하지 않습니다(최소 노출).
/// x0은 성공(0), x1은 배지 자리입니다(파생 배지는 다음 슬라이스, 현재 0).
fn deliver(src: &Context, dst: &mut Context) {
    dst.x[2..=5].copy_from_slice(&src.x[2..=5]);
    dst.x[0] = 0;
    dst.x[1] = 0;
}

/// 보류된 응답 자격을 정리하는 함수입니다.
///
/// `holder`가 응답 없이 종료하거나 새 CALL을 받아 자격을 덮을 때, 응답을
/// 기다리던 호출자를 NO_REPLY 에러로 깨워 영구 블록을 막습니다. 보류된
/// 응답이 없으면 아무 일도 하지 않습니다.
///
/// # Safety
/// 시스템 콜/IRQ 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 하고
/// `holder`는 유효한 TCB여야 합니다.
pub unsafe fn abort_reply(holder: &mut Tcb) {
    let caller = holder.reply_to as *mut Tcb;
    holder.reply_to = 0;
    if caller.is_null() {
        return;
    }
    // SAFETY: reply_to는 이 크레이트가 BlockedReply 상태의 유효한 TCB로만 기록함
    unsafe {
        (*caller).ctx.x[0] = k0_abi::err::NO_REPLY as u64;
        k0_sched::enqueue(caller);
    }
}

/// SEND 시스템 콜 처리 함수입니다.
///
/// 수신자가 대기 중이면 즉시 전달하고, 아니면 송신자가 엔드포인트 대기
/// 큐에서 블록됩니다(NONBLOCK이면 WOULD_BLOCK). 블록 경로의 반환
/// 레지스터는 미래의 수신자가 기록하므로 여기서 덮지 않습니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그, x2-x5 = MR)
///
/// # Errors
/// false는 전 태스크 블록(교착)이며 호출자가 fail-secure 파킹해야 합니다
#[must_use]
pub fn sys_send(ctx: &mut Context) -> bool {
    let ep = match ep_of(ctx.x[0]) {
        Ok(ep) => ep,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    let nonblock = ctx.x[1] & k0_abi::ipc::NONBLOCK != 0;
    // SAFETY: ep는 재분류가 소거하고 별칭 매핑한 전용 프레임이며, 단일
    //         코어의 예외 컨텍스트라 접근이 배타적이고 큐 링크는 유효함
    unsafe {
        let head = (*ep).head as *mut Tcb;
        if !head.is_null() && (*head).state == TaskState::BlockedRecv {
            let rcv = (*ep).pop() as *mut Tcb;
            deliver(ctx, &mut (*rcv).ctx);
            k0_sched::enqueue(rcv);
            ctx.x[0] = 0;
            return true;
        }
        if nonblock {
            ctx.x[0] = k0_abi::err::WOULD_BLOCK as u64;
            return true;
        }
        let cur = k0_sched::current();
        (*cur).state = TaskState::BlockedSend;
        (*ep).push(cur);
        k0_sched::block_and_switch()
    }
}

/// RECV 시스템 콜 처리 함수입니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그)
///
/// # Errors
/// false는 전 태스크 블록(교착)이며 호출자가 fail-secure 파킹해야 합니다
#[must_use]
pub fn sys_recv(ctx: &mut Context) -> bool {
    let ep = match ep_of(ctx.x[0]) {
        Ok(ep) => ep,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    let nonblock = ctx.x[1] & k0_abi::ipc::NONBLOCK != 0;
    // SAFETY: sys_send와 동일
    unsafe { recv_inner(ctx, ep, nonblock) }
}

/// 수신 공통 경로(RECV와 REPLY_RECV의 수신 단계) 함수입니다.
///
/// 대기 중인 송신자가 있으면 즉시 받습니다. CALL 송신자는 응답 대기
/// (BlockedReply)로 전환되고 수신자의 reply_to에 1회성 응답 자격이
/// 기록됩니다. 이미 보류된 응답이 있었다면 그 호출자는 NO_REPLY로
/// 깨웁니다(자격은 항상 최대 하나).
///
/// # Errors
/// false는 전 태스크 블록(교착)입니다
///
/// # Safety
/// 시스템 콜 컨텍스트여야 하고 `ep`는 유효한 엔드포인트 별칭이어야 합니다.
unsafe fn recv_inner(ctx: &mut Context, ep: *mut Endpoint, nonblock: bool) -> bool {
    // SAFETY: 함수 계약대로 접근이 배타적이고 큐 링크는 유효함
    unsafe {
        let head = (*ep).head as *mut Tcb;
        if !head.is_null() && (*head).state != TaskState::BlockedRecv {
            let snd = (*ep).pop() as *mut Tcb;
            deliver(&(*snd).ctx, ctx);
            if (*snd).state == TaskState::BlockedCall {
                let cur = k0_sched::current();
                abort_reply(&mut *cur);
                (*snd).state = TaskState::BlockedReply;
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
        (*cur).state = TaskState::BlockedRecv;
        (*ep).push(cur);
        k0_sched::block_and_switch()
    }
}

/// CALL 시스템 콜 처리 함수입니다.
///
/// 전송과 응답 대기가 한 번의 트랩으로 원자적이라 응답 창을 놓치는 경합이
/// 없습니다. 수신자에게 전달되는 순간 호출자는 BlockedReply가 되고 응답
/// 자격은 수신자의 reply_to에만 존재합니다. 수신자가 응답 없이 종료하면
/// NO_REPLY로 깨어납니다.
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그, x2-x5 = MR)
///
/// # Errors
/// false는 전 태스크 블록(교착)이며 호출자가 fail-secure 파킹해야 합니다
#[must_use]
pub fn sys_call(ctx: &mut Context) -> bool {
    let ep = match ep_of(ctx.x[0]) {
        Ok(ep) => ep,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    let nonblock = ctx.x[1] & k0_abi::ipc::NONBLOCK != 0;
    // SAFETY: sys_send와 동일
    unsafe {
        let cur = k0_sched::current();
        let head = (*ep).head as *mut Tcb;
        if !head.is_null() && (*head).state == TaskState::BlockedRecv {
            let rcv = (*ep).pop() as *mut Tcb;
            deliver(ctx, &mut (*rcv).ctx);
            abort_reply(&mut *rcv);
            (*rcv).reply_to = cur as u64;
            (*cur).state = TaskState::BlockedReply;
            k0_sched::enqueue(rcv);
        } else {
            if nonblock {
                ctx.x[0] = k0_abi::err::WOULD_BLOCK as u64;
                return true;
            }
            (*cur).state = TaskState::BlockedCall;
            (*ep).push(cur);
        }
        k0_sched::block_and_switch()
    }
}

/// REPLY_RECV 시스템 콜 처리 함수입니다.
///
/// 보류된 호출자에게 x2-x5를 응답으로 전달한 뒤(1회성, 자격은 즉시 소멸,
/// 없으면 건너뜀) 이어서 수신 단계로 들어갑니다. 엔드포인트가 무효하면
/// 응답을 소비하지 않고 에러만 반환합니다(원자적 실패).
///
/// # Arguments
/// `ctx` - 호출 태스크의 저장된 컨텍스트(x0 = 슬롯, x1 = 플래그, x2-x5 = 응답 MR)
///
/// # Errors
/// false는 전 태스크 블록(교착)이며 호출자가 fail-secure 파킹해야 합니다
#[must_use]
pub fn sys_reply_recv(ctx: &mut Context) -> bool {
    let ep = match ep_of(ctx.x[0]) {
        Ok(ep) => ep,
        Err(e) => {
            ctx.x[0] = e as u64;
            return true;
        }
    };
    let nonblock = ctx.x[1] & k0_abi::ipc::NONBLOCK != 0;
    // SAFETY: sys_send와 동일하고 reply_to는 BlockedReply의 유효한 TCB임
    unsafe {
        let cur = k0_sched::current();
        let caller = (*cur).reply_to as *mut Tcb;
        if !caller.is_null() {
            (*cur).reply_to = 0;
            deliver(ctx, &mut (*caller).ctx);
            k0_sched::enqueue(caller);
        }
        recv_inner(ctx, ep, nonblock)
    }
}
