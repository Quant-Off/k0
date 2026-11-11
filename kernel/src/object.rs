//! 커널 오브젝트의 종료 처리 모듈입니다.
//!
//! # Features
//! 케이퍼빌리티가 슬롯에서 제거될 때(`CAP_DELETE`, `CAP_REVOKE`, 그리고
//! 슬롯을 담은 오브젝트의 연쇄 종료) k0-cap이 부르는 콜백을 구현합니다.
//! 여기서 하는 일은 하나입니다. 제거되는 케이퍼빌리티나 종료되는 오브젝트를
//! 가리키는 커널 쪽 상태를 전부 끊어, 그 메모리가 다시 재분류된 뒤에 커널과
//! 하드웨어가 옛 내용을 따라가는 일이 없게 합니다.
//!
//! - Frame: 이 케이퍼빌리티가 만든 매핑을 주소 공간에서 지웁니다
//! - PageTable: 주소 공간에서 연결을 끊습니다
//! - Endpoint 원본: 줄 서 있던 태스크를 전부 깨웁니다
//! - Tcb 원본, 그리고 TCB에 묶인 케이퍼빌리티: 그 태스크를 종료시킵니다
//! - AddrSpace 원본: 등록을 해제하고 TTBR0가 가리키고 있으면 빈 루트로
//!   옮깁니다
//!
//! 매핑 해제는 케이퍼빌리티의 매핑 기록 (asid, 세대)가 지금도 살아 있는
//! 주소 공간과 일치할 때만 테이블을 걷습니다. 주소 공간이 먼저 사라졌다면
//! 그 테이블은 이미 도달 불가능하고 메모리가 다른 용도로 바뀌었을 수 있으니
//! 절대 걷지 않습니다.
//!
//! # Errors
//! 걷는 도중 테이블 손상을 만나면 `k0_arch::fatal`로 정지합니다.

use k0_cap::{Cap, Mapping, Removed, Slot};
use k0_task::{Endpoint, Tcb};

/// 오브젝트의 PA를 커널 별칭 주소로 바꾸는 함수입니다.
fn kva(pa: u64) -> u64 {
    pa + k0_mm::KERNEL_VA_OFFSET
}

/// Frame 케이퍼빌리티의 매핑 기록대로 매핑을 지우는 함수입니다.
///
/// 기록이 낡았거나(주소 공간이 사라짐) 그 자리에 더는 이 프레임이 없으면
/// 아무것도 지우지 않습니다.
///
/// # Arguments
/// `base` - 프레임의 PA
/// `m` - 케이퍼빌리티에 있던 매핑 기록
///
/// # Safety
/// 시스템 콜 컨텍스트(단일 코어, DAIF 마스크)에서만 호출해야 합니다.
pub unsafe fn unmap_frame(base: u64, m: &Mapping) {
    // SAFETY: 함수 계약대로 등록부 접근이 배타적임
    let Some(root) = (unsafe { k0_mm::addr_spaces() }).resolve(m.asid, m.generation) else {
        return;
    };
    // SAFETY: root는 등록부가 살아 있다고 확인한 주소 공간의 루트이고, 살아
    //         있는 주소 공간의 테이블은 전부 별칭 매핑된 PageTable 오브젝트임
    match unsafe { k0_mm::user_unmap_frame(root, m.va, base) } {
        Ok(true) => k0_mm::flush_user_tlb(),
        Ok(false) => {}
        Err(_) => k0_arch::fatal("user page table corrupt (unmap)"),
    }
}

/// PageTable 케이퍼빌리티의 매핑 기록대로 테이블의 연결을 끊는 함수입니다.
///
/// 테이블의 내용은 건드리지 않습니다. 소거 여부는 호출자가 정합니다.
///
/// # Arguments
/// `base` - 테이블 프레임의 PA
/// `m` - 케이퍼빌리티에 있던 매핑 기록
///
/// # Safety
/// [unmap_frame]과 동일합니다.
pub unsafe fn uninstall_table(base: u64, m: &Mapping) {
    // SAFETY: 함수 계약대로 등록부 접근이 배타적임
    let Some(root) = (unsafe { k0_mm::addr_spaces() }).resolve(m.asid, m.generation) else {
        return;
    };
    // SAFETY: unmap_frame과 동일
    match unsafe { k0_mm::user_uninstall_table(root, m.va, u32::from(m.level), base) } {
        Ok(true) => k0_mm::flush_user_tlb(),
        Ok(false) => {}
        Err(_) => k0_arch::fatal("user page table corrupt (uninstall)"),
    }
}

/// k0-cap의 제거 콜백을 구현하는 구조체입니다. 상태가 없습니다.
pub struct Reaper;

impl k0_cap::Env for Reaper {
    fn removed(&mut self, r: &Removed) {
        // SAFETY: 케이퍼빌리티 삭제는 시스템 콜 컨텍스트에서만 일어나고,
        //         케이퍼빌리티가 가리키는 오브젝트는 재분류가 별칭 매핑한
        //         전용 프레임이며 이번 커널 진입이 끝날 때까지 재사용되지 않음
        unsafe {
            match r.cap {
                Cap::Frame {
                    base, map: Some(m), ..
                } => unmap_frame(base, &m),
                Cap::PageTable { base, map: Some(m) } => uninstall_table(base, &m),
                Cap::Endpoint { base, .. } if r.original => {
                    k0_ipc::cancel_endpoint(kva(base) as *mut Endpoint)
                }
                Cap::Tcb { base } if r.original => k0_ipc::teardown(kva(base) as *mut Tcb),
                Cap::AddrSpace { root_pa, asid } if r.original => {
                    if !k0_mm::addr_spaces().release(asid, root_pa) {
                        k0_arch::fatal("address space registry corrupt");
                    }
                    k0_mm::detach_user_root(root_pa);
                    k0_mm::flush_user_tlb();
                }
                _ => {}
            }
            // 태스크에 묶인 케이퍼빌리티(케이퍼빌리티 공간이나 주소 공간)가
            // 사라지면 그 태스크는 더 실행될 수 없음. 내장 슬롯은 TCB 프레임
            // 안에 있으므로 슬롯 주소를 그래뉼로 내리면 TCB임
            if r.bound {
                let tcb = (r.slot as u64 & !(k0_mm::GRANULE as u64 - 1)) as *mut Tcb;
                k0_ipc::teardown(tcb);
            }
        }
    }

    fn container(&mut self, cap: &Cap) -> Option<(*mut Slot, usize)> {
        match *cap {
            Cap::CNode { base, slots } => Some((kva(base) as *mut Slot, usize::try_from(slots).ok()?)),
            Cap::Tcb { base } => {
                // SAFETY: base는 TCB 케이퍼빌리티가 가리키던 별칭 매핑된 프레임임
                let slots = unsafe { Tcb::slot(kva(base) as *mut Tcb, 0) };
                Some((slots, k0_task::TCB_SLOTS))
            }
            _ => None,
        }
    }
}
