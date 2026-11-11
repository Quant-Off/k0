//! 시스템 콜과 EL0 폴트의 커널 정책 모듈입니다.
//!
//! # Features
//! k0-arch의 EL0 벡터가 링크 계약으로 부르는 `k0_syscall` / `k0_fault` /
//! `k0_preempt`를 정의합니다. 격리의 핵심 규칙은 두 가지입니다.
//!
//! - 모든 슬롯 번호는 호출한 태스크 자신의 CNode(현재 TCB의 내장 슬롯이
//!   가리키는 것)에서만 해석합니다. 전역 CNode는 없으므로 다른 태스크의
//!   케이퍼빌리티를 번호로 가리킬 방법이 없습니다.
//! - 메모리나 다른 태스크를 바꾸는 시스템 콜은 반드시 그 대상의
//!   케이퍼빌리티를 받습니다. "현재 주소 공간" 같은 암묵 권한은 없습니다.
//!
//! 핸들러가 현재 태스크 자신을 종료시킬 수 있습니다(EXIT, 자기 CNode나
//! 주소 공간의 삭제, 자기가 든 untyped의 회수). 그래서 반환값 기록과 다음
//! 태스크 선택은 핸들러가 끝난 뒤(케이퍼빌리티 정리가 전부 끝난 뒤)
//! [finish]가 한곳에서 합니다. 종료된 태스크의 컨텍스트에는 반환값을 쓰지
//! 않고 소거만 합니다.
//!
//! # Errors
//! 미지의 시스템 콜 번호와 EL0 폴트는 호출한 태스크만 종료시킵니다. 실행할
//! 태스크가 남지 않으면 fail-secure 파킹합니다. 케이퍼빌리티 슬롯이나
//! 사용자 페이지 테이블의 손상은 `k0_arch::fatal`로 정지합니다.

use core::fmt::Write;

use k0_abi::err;
use k0_arch::earlycon::EarlyCon;
use k0_arch::usermode::Context;
use k0_cap::{Cap, CapError, CNodeRef, Entry, Mapping, Slot};
use k0_task::{TaskState, Tcb};

use crate::object::{self, Reaper};
use crate::panic::park;

// ABI의 권한 비트와 k0-cap의 권한 비트는 같은 값이어야 함
const _: () = {
    assert!(k0_abi::rights::R == k0_cap::rights::R as u64);
    assert!(k0_abi::rights::W == k0_cap::rights::W as u64);
    assert!(k0_abi::rights::X == k0_cap::rights::X as u64);
    assert!(k0_abi::rights::ALL == k0_cap::rights::ALL as u64);
    assert!(k0_abi::bootinfo::slot::TCB == k0_cap::boot_slot::TCB);
    assert!(k0_abi::bootinfo::slot::ADDR_SPACE == k0_cap::boot_slot::ADDR_SPACE);
    assert!(k0_abi::bootinfo::slot::CONSOLE == k0_cap::boot_slot::CONSOLE);
    assert!(k0_abi::bootinfo::slot::CNODE == k0_cap::boot_slot::CNODE);
};

/// EL0 svc 트랩의 시스템 콜 정책 함수입니다. (k0-arch가 링크 계약으로 호출)
///
/// 미지의 번호는 제로 트러스트 원칙대로 호출한 태스크를 종료시키고, 그게
/// 마지막 태스크였다면 fail-secure 파킹합니다.
///
/// # Arguments
/// `ctx` - 사용자 컨텍스트, x8 = 번호, x0-x5 = 인자, x0(수신 계열은 x1-x5도) = 반환값
#[unsafe(no_mangle)]
extern "C" fn k0_syscall(ctx: &mut Context) {
    use k0_abi::syscall as nr;

    let mut con = EarlyCon;
    let a = [ctx.x[0], ctx.x[1], ctx.x[2], ctx.x[3], ctx.x[4], ctx.x[5]];
    // Some(v)는 x0에 쓸 반환값, None은 핸들러가 컨텍스트 기록을 이미 끝낸 경우
    let ret: Option<i64> = match ctx.x[8] {
        nr::DEBUG_PUTC => Some(sys_putc(a[0], a[1])),
        nr::YIELD => {
            ctx.x[0] = 0;
            // SAFETY: 벡터가 컨텍스트 저장을 마친 시스템 콜 컨텍스트임
            unsafe { k0_sched::rotate() };
            None
        }
        nr::EXIT => {
            let _ = writeln!(con, "k0: task exited (code {})", a[0]);
            kill_current();
            None
        }
        nr::RETYPE => Some(sys_retype(a[0], a[1], a[2])),
        nr::MAP => Some(sys_map(a[0], a[1], a[2], a[3])),
        nr::UNMAP => Some(sys_unmap(a[0])),
        nr::TCB_CONFIGURE => Some(sys_tcb_configure(a[0], a[1], a[2], a[3], a[4], a[5])),
        nr::TCB_RESUME => Some(sys_tcb_resume(a[0])),
        nr::CAP_COPY => Some(sys_cap_copy(a[0], a[1], a[2], a[3], a[4])),
        nr::CAP_MOVE => Some(sys_cap_move(a[0], a[1], a[2])),
        nr::CAP_DELETE => Some(sys_cap_delete(a[0])),
        nr::CAP_REVOKE => Some(sys_cap_revoke(a[0])),
        // IPC는 블록 경로에서 반환 레지스터를 미래의 상대가 기록하므로
        // 핸들러가 ctx 기록까지 직접 책임지고, false(교착)만 커널이 파킹
        nr::SEND => ipc_or_park(&mut con, k0_ipc::sys_send(ctx)),
        nr::RECV => ipc_or_park(&mut con, k0_ipc::sys_recv(ctx)),
        nr::CALL => ipc_or_park(&mut con, k0_ipc::sys_call(ctx)),
        nr::REPLY_RECV => ipc_or_park(&mut con, k0_ipc::sys_reply_recv(ctx)),
        other => {
            let _ = writeln!(con, "k0: unknown syscall {other}, killing task");
            kill_current();
            None
        }
    };
    finish(ctx, ret);
}

/// EL0 동기 폴트의 격리 정책 함수입니다. (k0-arch가 링크 계약으로 호출)
///
/// 폴트는 그 태스크의 결함이므로 시스템을 세우지 않고 해당 태스크만
/// 종료합니다(격리). 폴트를 핸들러 태스크에 IPC로 전달하는 구조는 설계된
/// 확장 지점입니다.
///
/// # Arguments
/// `ctx` - 폴트 시점의 사용자 컨텍스트(ELR = 폴트 명령)
/// `esr` - ESR_EL1 신드롬
/// `far` - FAR_EL1(어보트 계열에서만 의미 있음)
#[unsafe(no_mangle)]
extern "C" fn k0_fault(ctx: &mut Context, esr: u64, far: u64) {
    let mut con = EarlyCon;
    let ec = k0_arch::vectors::ec_name((esr >> 26) & 0x3F);
    let _ = writeln!(
        con,
        "k0: task fault {ec} (esr {esr:#x} elr {:#x} far {far:#x}), killing task",
        ctx.elr
    );
    kill_current();
    finish(ctx, None);
}

/// 타이머 틱의 선점 정책 함수입니다. (k0-arch가 EL0 IRQ 경로에서 링크
/// 계약으로 호출)
#[unsafe(no_mangle)]
extern "C" fn k0_preempt() {
    // SAFETY: 벡터가 사용자 컨텍스트 저장을 마친 EL0 IRQ/FIQ 컨텍스트임
    unsafe { k0_sched::rotate() };
}

/// 핸들러가 끝난 뒤의 공통 종료 경로 함수입니다.
///
/// 이 시점에는 케이퍼빌리티 삭제에 따른 정리가 전부 끝나 있습니다(k0-cap이
/// 보류 목록을 비운 뒤에 반환함). 현재 태스크가 이번 진입 중에 종료됐으면
/// 그 컨텍스트를 소거하고 다음 태스크로 넘어갑니다. 준비 큐에는 살아 있는
/// 태스크만 있으므로(종료 처리가 큐에서 빼냄) 넘어간 태스크가 다시 종료
/// 상태일 수는 없습니다.
///
/// # Arguments
/// `ctx` - 이번 진입을 일으킨 태스크의 컨텍스트
/// `ret` - 그 태스크의 x0에 쓸 반환값(살아 있을 때만 기록)
fn finish(ctx: &mut Context, ret: Option<i64>) {
    // SAFETY: 벡터가 컨텍스트 저장을 마친 시스템 콜/폴트 컨텍스트임
    unsafe {
        if k0_sched::current_is_dead() {
            // 종료된 태스크의 레지스터를 남기지 않음. ctx는 현재(종료된)
            // 태스크의 컨텍스트이고 그 TCB 프레임은 아직 재사용 전임
            *ctx = Context::zeroed();
            if !k0_sched::block_and_switch() {
                let mut con = EarlyCon;
                let _ = writeln!(con, "k0: all tasks exited");
                park()
            }
            return;
        }
    }
    if let Some(v) = ret {
        ctx.x[0] = v as u64;
    }
}

/// 현재 태스크를 종료 상태로 만드는 함수입니다.
///
/// 죽는 태스크가 보류한 응답 자격을 정리해 호출자가 NO_REPLY로 깨어나게
/// 합니다(영구 블록 방지). 다음 태스크로의 전환은 [finish]가 합니다.
/// 태스크에 묶인 케이퍼빌리티(CNode, 주소 공간)는 TCB 오브젝트가 삭제될
/// 때까지 남습니다.
fn kill_current() {
    // SAFETY: 벡터가 컨텍스트 저장을 마친 시스템 콜 컨텍스트이고 current는
    //         유효한 TCB임
    unsafe { k0_ipc::teardown(k0_sched::current()) };
}

/// IPC가 교착(전 태스크 블록)을 보고하면 fail-secure 파킹하는 함수입니다.
///
/// 알림 오브젝트가 생기기 전까지는 블록된 태스크를 깨울 외부 주체가
/// 없으므로 전 태스크 블록은 회복 불가능한 교착입니다.
fn ipc_or_park(con: &mut EarlyCon, ok: bool) -> Option<i64> {
    if !ok {
        let _ = writeln!(con, "k0: all tasks blocked");
        park()
    }
    None
}

/// 오브젝트의 PA를 커널 별칭 주소로 바꾸는 함수입니다.
fn kva(pa: u64) -> u64 {
    pa + k0_mm::KERNEL_VA_OFFSET
}

/// 호출한 태스크의 CNode를 주는 함수입니다.
///
/// # Errors
/// 태스크에 CNode가 묶여 있지 않으면 BAD_SLOT(어떤 슬롯도 가리킬 수 없음)
fn cspace() -> Result<CNodeRef, i64> {
    // SAFETY: 단일 코어의 시스템 콜 컨텍스트(DAIF 마스크)라 접근이 배타적이고
    //         current는 유효한 TCB임
    match unsafe { Tcb::cspace(k0_sched::current()) } {
        Ok(Some(c)) => Ok(c),
        Ok(None) => Err(err::BAD_SLOT),
        Err(_) => k0_arch::fatal("cap slot corrupt (cspace)"),
    }
}

/// 슬롯 내용을 읽는 함수입니다. 손상은 정지로 이어집니다.
fn read(slot: *mut Slot) -> Option<Entry> {
    // SAFETY: slot은 살아 있는 CNode나 TCB의 범위 안 슬롯이고 접근이 배타적임
    match unsafe { k0_cap::read(slot) } {
        Ok(e) => e,
        Err(_) => k0_arch::fatal("cap slot corrupt"),
    }
}

/// CNode에서 채워진 슬롯을 찾는 함수입니다.
///
/// # Errors
/// 0번, 범위 밖, 빈 슬롯은 전부 BAD_SLOT
fn lookup(cnode: &CNodeRef, index: u64) -> Result<(*mut Slot, Entry), i64> {
    let slot = cnode.slot(index).ok_or(err::BAD_SLOT)?;
    let entry = read(slot).ok_or(err::BAD_SLOT)?;
    Ok((slot, entry))
}

/// k0-cap의 에러를 ABI 에러 코드로 바꾸는 함수입니다.
///
/// # Errors
/// `Corrupt`는 커널 불변식 위반이므로 정지합니다
fn cap_err(e: CapError) -> i64 {
    match e {
        CapError::Empty => err::BAD_SLOT,
        CapError::Occupied => err::OCCUPIED,
        CapError::NotUntyped => err::NOT_UNTYPED,
        CapError::Exhausted => err::EXHAUSTED,
        CapError::NotCopyable => err::BAD_CAP,
        CapError::NoRights => err::NO_RIGHTS,
        CapError::BadBadge => err::BAD_BADGE,
        CapError::TooDeep => err::TOO_DEEP,
        CapError::PrepFailed | CapError::OutOfSlots | CapError::BadRegion => err::KERNEL_RESOURCE,
        CapError::Corrupt => k0_arch::fatal("cap derivation list corrupt"),
    }
}

/// Result를 시스템 콜 반환값으로 접는 함수입니다.
fn done(r: Result<(), i64>) -> i64 {
    match r {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// DEBUG_PUTC 시스템 콜 처리 함수입니다.
///
/// Console 케이퍼빌리티 제시가 필요합니다(암묵 권한 없음). 출력 가능한
/// ASCII와 개행만 그대로 내보내고 나머지 바이트는 `?`로 바꿔 터미널 제어
/// 문자 주입을 막습니다.
///
/// # Arguments
/// `slot` - Console 슬롯(x0)
/// `byte` - 출력할 바이트(x1, 하위 8비트)
fn sys_putc(slot: u64, byte: u64) -> i64 {
    done((|| {
        let cn = cspace()?;
        let (_, e) = lookup(&cn, slot)?;
        if e.cap != Cap::Console {
            return Err(err::BAD_CAP);
        }
        let b = byte as u8;
        let safe = if b == b'\n' || (0x20..=0x7E).contains(&b) { b } else { b'?' };
        EarlyCon.put_byte(safe);
        Ok(())
    })())
}

/// RETYPE 시스템 콜 처리 함수입니다.
///
/// untyped에서 오브젝트를 잘라내기 전에 해당 프레임을 TTBR1 별칭으로
/// 매핑하고 소거합니다(이전 내용 누설 차단, 회수된 메모리의 재사용 때도
/// 같음). 준비가 실패하면 케이퍼빌리티 상태는 변하지 않습니다. 새
/// 케이퍼빌리티는 호출자가 지정한 빈 슬롯에 들어가므로 커널은 빈 자리를
/// 찾지 않습니다.
///
/// # Arguments
/// `ut` - untyped 슬롯(x0)
/// `kind` - 오브젝트 타입(x1)
/// `dest` - 새 케이퍼빌리티를 받을 빈 슬롯(x2)
fn sys_retype(ut: u64, kind: u64, dest: u64) -> i64 {
    let kind = match kind {
        k0_abi::obj::FRAME => k0_cap::ObjKind::Frame,
        k0_abi::obj::PAGE_TABLE => k0_cap::ObjKind::PageTable,
        k0_abi::obj::TCB => k0_cap::ObjKind::Tcb,
        k0_abi::obj::ENDPOINT => k0_cap::ObjKind::Endpoint,
        k0_abi::obj::CNODE => k0_cap::ObjKind::CNode,
        k0_abi::obj::ADDR_SPACE => k0_cap::ObjKind::AddrSpace,
        _ => return err::BAD_TYPE,
    };
    done((|| {
        let cn = cspace()?;
        let ut = cn.slot(ut).ok_or(err::BAD_SLOT)?;
        let dest = cn.slot(dest).ok_or(err::BAD_SLOT)?;
        let prep = |pa: u64| -> Option<u16> {
            k0_mm::alias_object(pa).ok()?;
            // SAFETY: 방금 별칭 매핑을 확인한 커널 전용 구간이며, 자손이 없는
            //         untyped에서만 같은 자리가 다시 나오므로 살아 있는
            //         오브젝트와 겹치지 않음. 소거는 이전 내용의 누설을 차단함
            unsafe {
                core::ptr::write_bytes(kva(pa) as *mut u8, 0, k0_mm::GRANULE);
            }
            if kind == k0_cap::ObjKind::AddrSpace {
                // SAFETY: 시스템 콜 컨텍스트라 등록부 접근이 배타적임
                unsafe { k0_mm::addr_spaces() }.register(pa)
            } else {
                Some(0)
            }
        };
        // SAFETY: 두 슬롯은 살아 있는 CNode의 범위 안 슬롯이고 접근이 배타적임
        unsafe { k0_cap::retype(ut, dest, kind, k0_mm::GRANULE as u64, prep) }.map_err(cap_err)
    })())
}

/// 슬롯에서 AddrSpace 케이퍼빌리티를 읽고 등록부로 살아 있음을 확인하는
/// 함수입니다. (루트 PA, asid, 세대)를 줍니다.
///
/// # Errors
/// AddrSpace가 아니면 BAD_CAP. 케이퍼빌리티는 있는데 등록부에 없으면 커널
/// 불변식 위반이므로 정지합니다
fn aspace_of(cn: &CNodeRef, index: u64) -> Result<(*mut Slot, u64, u16, u64), i64> {
    let (slot, e) = lookup(cn, index)?;
    let Cap::AddrSpace { root_pa, asid } = e.cap else {
        return Err(err::BAD_CAP);
    };
    // SAFETY: 시스템 콜 컨텍스트라 등록부 접근이 배타적임
    match unsafe { k0_mm::addr_spaces() }.get(asid) {
        Some((root, generation)) if root == root_pa => Ok((slot, root_pa, asid, generation)),
        _ => k0_arch::fatal("address space registry corrupt"),
    }
}

/// MAP 시스템 콜 처리 함수입니다.
///
/// 케이퍼빌리티 종류가 동작을 결정합니다. Frame은 리프 매핑, PageTable은
/// 경로의 첫 빈 레벨에 설치입니다. 대상 주소 공간은 제시한 AddrSpace
/// 케이퍼빌리티로만 정해지며 현재 설치된 TTBR0는 참조하지 않습니다(암묵
/// 권한 배제). Frame의 매핑 권한은 케이퍼빌리티의 권한 비트를 넘을 수
/// 없습니다. 실행 매핑(RX)은 X 권한이 있어야 하는데, X는 부트 시점의
/// 텍스트 프레임에만 있고 그 프레임에는 W가 없습니다. 재분류로 만든
/// 프레임은 W를 갖는 대신 X를 영영 가질 수 없습니다. 그래서 어떤 물리
/// 프레임도 한곳에서 쓰기 가능하면서 다른 곳에서 실행 가능할 수 없습니다.
/// 성공하면 케이퍼빌리티에 매핑 위치를 기록해 두었다가 해제와 삭제 때
/// 씁니다. 무효 항목을 유효하게 만드는 것뿐이라 TLB 무효화는 필요 없습니다.
///
/// # Arguments
/// `slot` - 케이퍼빌리티 슬롯(x0)
/// `va` - 사용자 VA(x1)
/// `perm` - Frame 권한(x2, PageTable은 무시)
/// `aspace` - 대상 AddrSpace 슬롯(x3)
fn sys_map(slot: u64, va: u64, perm: u64, aspace: u64) -> i64 {
    use k0_cap::rights::{R, W, X};

    done((|| {
        let cn = cspace()?;
        let (_, root, asid, generation) = aspace_of(&cn, aspace)?;
        let (s, e) = lookup(&cn, slot)?;
        let g = k0_mm::GRANULE as u64;
        match e.cap {
            Cap::Frame { base, rights, map } => {
                if map.is_some() {
                    return Err(err::ALREADY_MAPPED);
                }
                let (uperm, need) = match perm {
                    k0_abi::perm::RO => (k0_mm::UserPerm::RoUser, R),
                    k0_abi::perm::RW => (k0_mm::UserPerm::RwUser, R | W),
                    k0_abi::perm::RX => (k0_mm::UserPerm::TextUser, R | X),
                    _ => return Err(err::BAD_PERM),
                };
                if rights & need != need {
                    return Err(err::NO_RIGHTS);
                }
                // SAFETY: root는 등록부가 살아 있다고 확인한 주소 공간의 루트고,
                //         걷게 되는 테이블은 전부 부트 윈도우 또는 재분류된
                //         (별칭 매핑된) 프레임임
                unsafe { k0_mm::user_map_frame(root, va, base, uperm) }.map_err(mmu_err)?;
                let map = Some(Mapping {
                    asid,
                    generation,
                    va,
                    level: 3,
                });
                // SAFETY: s는 방금 읽은 살아 있는 슬롯이고 매핑 기록만 바꿈
                unsafe { k0_cap::update(s, Cap::Frame { base, rights, map }) }.map_err(cap_err)
            }
            Cap::PageTable { base, map } => {
                if map.is_some() {
                    return Err(err::ALREADY_MAPPED);
                }
                // SAFETY: 위와 동일하고 base는 기록이 비어 있는(따라서 소거된)
                //         전용 테이블 프레임임
                let level =
                    unsafe { k0_mm::user_install_table(root, va, base) }.map_err(mmu_err)?;
                let map = Some(Mapping {
                    asid,
                    generation,
                    va: va & !(g - 1),
                    level: level as u8,
                });
                // SAFETY: s는 방금 읽은 살아 있는 슬롯이고 매핑 기록만 바꿈
                unsafe { k0_cap::update(s, Cap::PageTable { base, map }) }.map_err(cap_err)
            }
            _ => Err(err::BAD_CAP),
        }
    })())
}

/// UNMAP 시스템 콜 처리 함수입니다.
///
/// Frame은 자기 매핑을 지우고, PageTable은 주소 공간에서 연결을 끊은 뒤
/// 소거합니다. 소거는 기록이 낡았더라도(주소 공간이 먼저 사라졌더라도)
/// 항상 하므로, 매핑 기록이 비어 있는 PageTable은 언제나 전부 0입니다.
/// 그래서 떼어 낸 테이블을 다른 자리에 설치해도 옛 매핑이 따라가지
/// 않습니다.
///
/// # Arguments
/// `slot` - Frame 또는 PageTable 슬롯(x0)
fn sys_unmap(slot: u64) -> i64 {
    done((|| {
        let cn = cspace()?;
        let (s, e) = lookup(&cn, slot)?;
        match e.cap {
            Cap::Frame { base, rights, map } => {
                let m = map.ok_or(err::BAD_STATE)?;
                // SAFETY: 시스템 콜 컨텍스트이고 s는 살아 있는 슬롯임
                unsafe {
                    object::unmap_frame(base, &m);
                    k0_cap::update(
                        s,
                        Cap::Frame {
                            base,
                            rights,
                            map: None,
                        },
                    )
                }
                .map_err(cap_err)
            }
            Cap::PageTable { base, map } => {
                let m = map.ok_or(err::BAD_STATE)?;
                // SAFETY: 시스템 콜 컨텍스트이고, 연결을 끊은 뒤(또는 주소
                //         공간이 사라져 도달 불가능해진 뒤)의 테이블은 이
                //         케이퍼빌리티만이 가리키는 전용 프레임임
                unsafe {
                    object::uninstall_table(base, &m);
                    k0_mm::zero_table(base);
                    k0_mm::flush_user_tlb();
                    k0_cap::update(s, Cap::PageTable { base, map: None })
                }
                .map_err(cap_err)
            }
            _ => Err(err::BAD_CAP),
        }
    })())
}

/// 런타임 매핑 경로의 MmuError를 ABI 에러 코드로 바꾸는 함수입니다.
///
/// # Errors
/// `BadTable`은 페이지 테이블 손상을 의미하므로 fail-secure 정지합니다
fn mmu_err(e: k0_mm::MmuError) -> i64 {
    match e {
        k0_mm::MmuError::Misaligned | k0_mm::MmuError::NullPage => err::BAD_VA,
        k0_mm::MmuError::Overlap => err::OVERLAP,
        k0_mm::MmuError::MissingTable => err::MISSING_TABLE,
        k0_mm::MmuError::BadTable => k0_arch::fatal("user page table corrupt"),
        _ => err::KERNEL_RESOURCE,
    }
}

/// 목적지 CNode를 가리키는 케이퍼빌리티에서 목적지 슬롯을 얻는 함수입니다.
///
/// 목적지는 언제나 CNode 케이퍼빌리티로 지목하고, 그 케이퍼빌리티가 말하는
/// 슬롯 수 안에서만(0번 제외) 고릅니다. 슬롯이 비어 있는지는 k0-cap이
/// 확인합니다.
///
/// # Errors
/// CNode가 아니면 BAD_CAP, 0번이거나 범위 밖이면 BAD_SLOT
fn dest_slot(cn: &CNodeRef, cnode_slot: u64, index: u64) -> Result<*mut Slot, i64> {
    let (_, e) = lookup(cn, cnode_slot)?;
    let Cap::CNode { base, slots } = e.cap else {
        return Err(err::BAD_CAP);
    };
    let n = usize::try_from(slots).map_err(|_| err::BAD_CAP)?;
    // SAFETY: 살아 있는 CNode 케이퍼빌리티는 별칭 매핑된 슬롯 배열을 가리킴
    let target = unsafe { CNodeRef::new(kva(base) as *mut Slot, n) };
    target.slot(index).ok_or(err::BAD_SLOT)
}

/// CAP_COPY 시스템 콜 처리 함수입니다.
///
/// # Arguments
/// `src` - 원본 슬롯(x0)
/// `cnode` - 목적지 CNode를 가리키는 슬롯(x1)
/// `index` - 목적지 CNode 안의 빈 슬롯(x2)
/// `mask` - 남길 권한 비트(x3)
/// `badge` - 붙일 배지(x4, 0 = 없음)
fn sys_cap_copy(src: u64, cnode: u64, index: u64, mask: u64, badge: u64) -> i64 {
    done((|| {
        if mask & !k0_abi::rights::ALL != 0 {
            return Err(err::BAD_PERM);
        }
        let cn = cspace()?;
        let (s, _) = lookup(&cn, src)?;
        let d = dest_slot(&cn, cnode, index)?;
        // SAFETY: 두 슬롯은 살아 있는 CNode의 범위 안 슬롯이고 접근이 배타적임
        unsafe { k0_cap::copy(s, d, mask as u8, badge, false) }.map_err(cap_err)
    })())
}

/// CAP_MOVE 시스템 콜 처리 함수입니다.
///
/// # Arguments
/// `src` - 원본 슬롯(x0)
/// `cnode` - 목적지 CNode를 가리키는 슬롯(x1)
/// `index` - 목적지 CNode 안의 빈 슬롯(x2)
fn sys_cap_move(src: u64, cnode: u64, index: u64) -> i64 {
    done((|| {
        let cn = cspace()?;
        let (s, _) = lookup(&cn, src)?;
        let d = dest_slot(&cn, cnode, index)?;
        // SAFETY: 두 슬롯은 살아 있는 CNode의 범위 안 슬롯이고 접근이 배타적임
        unsafe { k0_cap::move_cap(s, d) }.map_err(cap_err)
    })())
}

/// CAP_DELETE 시스템 콜 처리 함수입니다.
///
/// 자손을 전부 지운 뒤 자신을 지웁니다. 원본을 지우면 오브젝트가 종료되고,
/// 그 오브젝트가 슬롯을 담고 있었으면(CNode, TCB) 안의 케이퍼빌리티까지
/// 연쇄적으로 정리됩니다. 호출자 자신이 종료되는 경우도 있으며 그 처리는
/// [finish]가 합니다.
///
/// # Arguments
/// `slot` - 삭제할 슬롯(x0)
fn sys_cap_delete(slot: u64) -> i64 {
    done((|| {
        let cn = cspace()?;
        let (s, _) = lookup(&cn, slot)?;
        // SAFETY: s는 살아 있는 슬롯이고, 파생 리스트로 이어진 슬롯은 전부
        //         살아 있는 CNode / TCB 안에 있으며 접근이 배타적임
        unsafe { k0_cap::delete(s, &mut Reaper) }.map_err(cap_err)
    })())
}

/// CAP_REVOKE 시스템 콜 처리 함수입니다.
///
/// 슬롯에서 파생된 케이퍼빌리티를 전부 지우고 슬롯 자신은 남깁니다.
/// untyped를 회수하면 거기서 잘라낸 오브젝트가 전부 종료되어 다음 RETYPE이
/// 그 메모리를 처음부터 다시 씁니다.
///
/// # Arguments
/// `slot` - 회수할 슬롯(x0)
fn sys_cap_revoke(slot: u64) -> i64 {
    done((|| {
        let cn = cspace()?;
        let (s, _) = lookup(&cn, slot)?;
        // SAFETY: sys_cap_delete와 동일
        unsafe { k0_cap::revoke(s, &mut Reaper) }.map_err(cap_err)
    })())
}

/// TCB_CONFIGURE 시스템 콜 처리 함수입니다.
///
/// 진입 컨텍스트는 커널이 강제합니다. SPSR은 항상 EL0t + DAIF 언마스크로
/// 기록되어 사용자에게서 PSTATE를 절대 받지 않습니다(권한 상승 차단). 새
/// 태스크의 주소 공간과 케이퍼빌리티 공간은 AddrSpace / CNode 케이퍼빌리티
/// 제시로만 지정할 수 있고, 두 케이퍼빌리티의 복사본이 TCB의 내장 슬롯에
/// 들어갑니다. 모든 검사를 통과한 뒤에 둘을 한꺼번에 기록하므로 한쪽만
/// 묶인 TCB는 생기지 않습니다. 구성은 Inactive 상태에서 한 번만
/// 허용됩니다(실행 중 재구성 금지).
///
/// # Arguments
/// `slot` - 재분류된 TCB의 슬롯(x0)
/// `entry` - 진입점 VA(x1, 4바이트 정렬)
/// `stack` - 스택 최상단 VA(x2, 16바이트 정렬)
/// `aspace` - AddrSpace 슬롯(x3)
/// `cnode` - CNode 슬롯(x4)
/// `arg` - 새 태스크가 x0으로 받을 값(x5)
fn sys_tcb_configure(slot: u64, entry: u64, stack: u64, aspace: u64, cnode: u64, arg: u64) -> i64 {
    let g = k0_mm::GRANULE as u64;
    // 비정렬 진입점은 첫 명령에서 PC 정렬 폴트가 되므로 구성 시점에 거부
    if entry % 4 != 0 || entry < g || entry >= 1u64 << 48 {
        return err::BAD_VA;
    }
    if stack % 16 != 0 || stack < g || stack > 1u64 << 48 {
        return err::BAD_VA;
    }
    done((|| {
        let cn = cspace()?;
        let (as_slot, root_pa, _, _) = aspace_of(&cn, aspace)?;
        let (cn_slot, cn_entry) = lookup(&cn, cnode)?;
        if !matches!(cn_entry.cap, Cap::CNode { .. }) {
            return Err(err::BAD_CAP);
        }
        let (_, t_entry) = lookup(&cn, slot)?;
        let Cap::Tcb { base } = t_entry.cap else {
            return Err(err::BAD_CAP);
        };
        let t = kva(base) as *mut Tcb;
        // SAFETY: base는 TCB 케이퍼빌리티가 가리키는 별칭 매핑된 전용 프레임이고
        //         소거 상태가 곧 유효한 Inactive TCB임. 상태는 raw 포인터로
        //         먼저 확인해, 실행 중인 TCB(그 컨텍스트의 가변 참조가 살아
        //         있음)와 겹치는 가변 참조를 만들지 않음. Inactive TCB는 어느
        //         큐에도 없고 현재 태스크도 아니므로 그 뒤의 가변 참조는
        //         유일함
        unsafe {
            if (*t).state != TaskState::Inactive {
                return Err(err::BAD_STATE);
            }
            // 검증과 기록이 분리돼 있어 실패하면 두 내장 슬롯 모두 빈 채로 남음
            k0_cap::copy_pair_bound(
                cn_slot,
                Tcb::slot(t, k0_task::TCB_SLOT_CSPACE),
                as_slot,
                Tcb::slot(t, k0_task::TCB_SLOT_VSPACE),
            )
            .map_err(cap_err)?;
            let t = &mut *t;
            t.ctx = Context::zeroed();
            t.ctx.elr = entry;
            t.ctx.sp = stack;
            t.ctx.spsr = 0; // EL0t, DAIF 언마스크, 커널이 강제
            t.ctx.x[0] = arg;
            t.ttbr0_pa = root_pa;
            t.state = TaskState::Stopped;
        }
        Ok(())
    })())
}

/// TCB_RESUME 시스템 콜 처리 함수입니다.
///
/// 구성(Stopped)을 마친 TCB만 준비 큐에 넣습니다. 이미 큐에 있거나 실행
/// 중이거나 종료된 TCB는 거부되므로 같은 TCB가 큐에 두 번 들어갈 수
/// 없습니다.
///
/// # Arguments
/// `slot` - 재분류된 TCB의 슬롯(x0)
fn sys_tcb_resume(slot: u64) -> i64 {
    done((|| {
        let cn = cspace()?;
        let (_, e) = lookup(&cn, slot)?;
        let Cap::Tcb { base } = e.cap else {
            return Err(err::BAD_CAP);
        };
        let t = kva(base) as *mut Tcb;
        // SAFETY: t는 TCB 케이퍼빌리티가 가리키는 전용 프레임의 별칭이고,
        //         Stopped 검사로 큐 중복 진입이 차단됨. Stopped는 구성이 끝나
        //         두 내장 슬롯이 묶인 상태에서만 나옴
        unsafe {
            if (*t).state != TaskState::Stopped {
                return Err(err::BAD_STATE);
            }
            k0_sched::enqueue(t);
        }
        Ok(())
    })())
}
