//! 진입 페이즈 2의 케이퍼빌리티 시스템 크레이트입니다.
//!
//! # Features
//! seL4 방식의 자원 모델을 따릅니다. 커널은 heap 없이 정적 루트 CNode 하나로
//! 시작하고, 커널/DTB/부트 윈도우를 제외한 나머지 물리 메모리 전부를
//! untyped 케이퍼빌리티로 만들어 루트 태스크 소유로 기록합니다. 지금은
//! 부트스트랩(생성과 목록화)까지만 구현되어 있고, 사용자 공간이 untyped를
//! 재분류(retype)하는 시스템 콜은 다음 슬라이스입니다.
//!
//! # Errors
//! 슬롯 고갈과 잘못된 예약 구간은 `CapError`로 반환합니다. 호출자는 실패 시
//! 부팅을 중단해야 합니다(fail-secure).

#![no_std]

use core::cell::UnsafeCell;

/// 루트 CNode의 슬롯 수
pub const CNODE_SLOTS: usize = 32;

/// 물리 메모리 구간 하나를 나타내는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysRegion {
    pub base: u64,
    pub size: u64,
}

impl PhysRegion {
    fn end(&self) -> u64 {
        self.base + self.size
    }
}

/// 케이퍼빌리티 하나를 나타내는 열거형입니다.
///
/// 슬롯 0은 비워 둡니다(null 케이퍼빌리티). 파생(derive)/회수(revoke) 계보는
/// 재분류 시스템 콜과 함께 추가될 확장 지점입니다.
#[derive(Clone, Copy, Debug)]
pub enum Cap {
    Empty,
    /// 루트 태스크의 TCB
    Tcb,
    /// 루트 태스크의 주소 공간(TTBR0 루트 테이블)
    AddrSpace { root_pa: u64 },
    /// 소유자가 재분류할 수 있는 미분류 물리 메모리
    Untyped { base: u64, size: u64 },
}

/// 케이퍼빌리티 부트스트랩이 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    OutOfSlots,
    BadRegion,
}

/// 루트 태스크에 넘길 케이퍼빌리티를 담는 루트 CNode 구조체입니다.
pub struct CNode {
    slots: [Cap; CNODE_SLOTS],
    used: usize,
}

impl CNode {
    /// 채워진 슬롯들을 주는 함수입니다. (슬롯 0의 null 포함)
    pub fn slots(&self) -> &[Cap] {
        &self.slots[..self.used]
    }

    fn push(&mut self, cap: Cap) -> Result<(), CapError> {
        if self.used == CNODE_SLOTS {
            return Err(CapError::OutOfSlots);
        }
        self.slots[self.used] = cap;
        self.used += 1;
        Ok(())
    }
}

struct SyncCell(UnsafeCell<CNode>);

/// # Safety
/// 단일 부트 코어가 bootstrap을 통해 한 번만 쓰고, 이후에는 공유 참조로만
/// 읽기 때문에 동시 접근이 없습니다.
unsafe impl Sync for SyncCell {}

static ROOT_CNODE: SyncCell = SyncCell(UnsafeCell::new(CNode {
    slots: [Cap::Empty; CNODE_SLOTS],
    used: 0,
}));

/// 루트 CNode를 구성하는 부트스트랩 함수입니다.
///
/// DTB의 물리 메모리 맵에서 예약 구간(커널 이미지, DTB, 부트 윈도우)을 뺀
/// 나머지를 untyped 케이퍼빌리티로 만듭니다. 예약 구간끼리는 겹치지 않아야
/// 합니다. 부트로더/펌웨어 예약 구간(FDT memreserve, /reserved-memory)의
/// 반영은 재분류 시스템 콜 전에 추가해야 하는 확장 지점입니다.
///
/// # Arguments
/// `memory` - DTB가 보고한 물리 메모리 구간들
/// `reserved` - untyped에서 제외할 구간들
/// `addr_space_root` - 루트 태스크 TTBR0 루트 테이블의 PA
///
/// # Errors
/// 구간 산술 위반은 `BadRegion`, 슬롯 부족은 `OutOfSlots`
pub fn bootstrap(
    memory: &[PhysRegion],
    reserved: &[PhysRegion],
    addr_space_root: u64,
) -> Result<&'static CNode, CapError> {
    // SAFETY: 단일 부트 코어의 초기화 시퀀스에서 한 번만 도달함(순서는
    //         kernel_main이 강제), 반환 후에는 공유 참조만 존재함
    let cnode = unsafe { &mut *ROOT_CNODE.0.get() };

    cnode.push(Cap::Empty)?; // 슬롯 0은 null
    cnode.push(Cap::Tcb)?;
    cnode.push(Cap::AddrSpace {
        root_pa: addr_space_root,
    })?;

    for m in memory {
        if m.size == 0 || m.base.checked_add(m.size).is_none() {
            return Err(CapError::BadRegion);
        }
        push_untypeds(cnode, *m, reserved)?;
    }
    Ok(cnode)
}

/// 메모리 구간 하나에서 예약 구간들을 뺀 나머지를 untyped로 넣는 함수입니다.
fn push_untypeds(cnode: &mut CNode, m: PhysRegion, reserved: &[PhysRegion]) -> Result<(), CapError> {
    // 예약 구간을 오름차순으로 훑기 위한 선택 정렬(구간 수가 작아 충분함)
    let mut cursor = m.base;
    loop {
        // cursor 이후에서 가장 낮은 예약 구간을 찾음
        let mut next: Option<PhysRegion> = None;
        for r in reserved {
            if r.size == 0 || r.base.checked_add(r.size).is_none() {
                return Err(CapError::BadRegion);
            }
            if r.end() > cursor && r.base < m.end() {
                match next {
                    Some(n) if n.base <= r.base => {}
                    _ => next = Some(*r),
                }
            }
        }
        match next {
            Some(r) => {
                if r.base > cursor {
                    cnode.push(Cap::Untyped {
                        base: cursor,
                        size: r.base.min(m.end()) - cursor,
                    })?;
                }
                if r.end() >= m.end() {
                    return Ok(());
                }
                cursor = r.end().max(cursor);
            }
            None => {
                if m.end() > cursor {
                    cnode.push(Cap::Untyped {
                        base: cursor,
                        size: m.end() - cursor,
                    })?;
                }
                return Ok(());
            }
        }
    }
}
