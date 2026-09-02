//! 케이퍼빌리티 시스템 크레이트입니다.
//!
//! # Features
//! seL4 방식의 자원 모델을 따릅니다. 커널은 heap 없이 정적 루트 CNode 하나로
//! 시작하고, 커널/DTB/부트 윈도우/펌웨어 예약 구간을 제외한 나머지 물리
//! 메모리 전부를 untyped 케이퍼빌리티로 만들어 루트 태스크 소유로
//! 기록합니다. 이양 후에는 재분류(retype)가 untyped에서 워터마크(범프)
//! 방식으로 커널 오브젝트를 잘라냅니다. 오브젝트는 untyped의 물리 메모리
//! 안에 직접 자리 잡으므로 커널 할당은 발생하지 않고, 워터마크가 단조
//! 증가라 오브젝트끼리 절대 겹치지 않습니다. 실제 메모리 준비(별칭 매핑과
//! 소거)는 호출자가 prep 콜백으로 수행하고, prep이 성공한 경우에만 상태가
//! 변합니다. 이 크레이트 자체는 메모리를 일절 건드리지 않는 순수
//! 장부(bookkeeping)입니다.
//!
//! # Errors
//! 부트스트랩의 슬롯 고갈과 잘못된 예약 구간은 `CapError`로 반환하며 호출자는
//! 부팅을 중단해야 합니다(fail-secure). 재분류 실패는 `RetypeError`로
//! 반환하며 상태 변화 없이 사용자에게 전달됩니다.

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
/// 케이퍼빌리티 전송(IPC)과 함께 추가될 확장 지점입니다.
#[derive(Clone, Copy, Debug)]
pub enum Cap {
    Empty,
    /// 루트 태스크의 TCB(부트 시점 정적 오브젝트, 재구성 불가)
    RootTcb,
    /// 루트 태스크의 주소 공간(TTBR0 루트 테이블)
    AddrSpace { root_pa: u64 },
    /// 커널 디버그 콘솔 출력 권한(부트 시점 정적 오브젝트)
    Console,
    /// 재분류로 만든 태스크 제어 블록, 상태 머신은 오브젝트 안에 있음
    Tcb { base: u64 },
    /// 소유자가 재분류할 수 있는 미분류 물리 메모리
    ///
    /// `used`는 재분류 워터마크로 base로부터의 소비량이며 단조 증가합니다
    Untyped { base: u64, size: u64, used: u64 },
    /// 재분류로 만든 사용자 매핑 가능 프레임
    Frame { base: u64, mapped: bool },
    /// 재분류로 만든 사용자 주소 공간의 중간 페이지 테이블
    PageTable { base: u64, installed: bool },
    /// 재분류로 만든 동기 IPC 엔드포인트, 대기 큐는 오브젝트 안에 있음
    Endpoint { base: u64 },
}

/// 재분류로 만들 수 있는 커널 오브젝트 종류를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjKind {
    Frame,
    PageTable,
    Tcb,
    Endpoint,
}

/// 케이퍼빌리티 부트스트랩이 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    OutOfSlots,
    BadRegion,
}

/// 재분류가 거부된 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetypeError {
    /// 슬롯 번호가 범위 밖
    BadSlot,
    /// 해당 슬롯이 untyped가 아님
    NotUntyped,
    /// untyped의 남은 공간 부족
    Exhausted,
    /// CNode 슬롯 가득 참
    OutOfSlots,
    /// 호출자의 메모리 준비(별칭 매핑/소거) 실패
    PrepFailed,
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

    /// 슬롯의 케이퍼빌리티 사본을 주는 함수입니다. 범위 밖이면 None
    ///
    /// # Arguments
    /// `slot` - 슬롯 번호
    pub fn cap(&self, slot: usize) -> Option<Cap> {
        self.slots().get(slot).copied()
    }

    /// 슬롯의 케이퍼빌리티 가변 참조를 주는 함수입니다. 범위 밖이면 None
    ///
    /// 매핑/설치 상태 플래그의 갱신에 쓰입니다.
    ///
    /// # Arguments
    /// `slot` - 슬롯 번호
    pub fn cap_mut(&mut self, slot: usize) -> Option<&mut Cap> {
        if slot >= self.used {
            return None;
        }
        Some(&mut self.slots[slot])
    }

    fn push(&mut self, cap: Cap) -> Result<(), CapError> {
        if self.used == CNODE_SLOTS {
            return Err(CapError::OutOfSlots);
        }
        self.slots[self.used] = cap;
        self.used += 1;
        Ok(())
    }

    /// untyped에서 커널 오브젝트 하나를 잘라내는(retype) 함수입니다.
    ///
    /// 워터마크를 그래뉼로 올림 정렬한 위치가 잘라낼 자리입니다. 모든 검사를
    /// 통과한 뒤 `prep`으로 호출자에게 물리 메모리 준비(별칭 매핑과 소거)를
    /// 맡기고, `prep`이 true를 준 경우에만 워터마크 전진과 새 케이퍼빌리티
    /// 기록을 확정합니다. 실패 경로에서는 상태가 일절 변하지 않습니다.
    ///
    /// # Arguments
    /// `slot` - 원본 untyped의 슬롯 번호
    /// `kind` - 만들 오브젝트 종류
    /// `granule` - 오브젝트 크기이자 정렬(플랫폼 그래뉼, 2의 거듭제곱)
    /// `prep` - 잘라낸 PA를 받아 메모리를 준비하는 콜백
    ///
    /// # Errors
    /// 슬롯 범위 밖은 `BadSlot`, untyped 아님은 `NotUntyped`, 공간 부족은
    /// `Exhausted`, CNode 가득 참은 `OutOfSlots`, 콜백 실패는 `PrepFailed`
    pub fn retype(
        &mut self,
        slot: usize,
        kind: ObjKind,
        granule: u64,
        prep: impl FnOnce(u64) -> bool,
    ) -> Result<usize, RetypeError> {
        if slot >= self.used {
            return Err(RetypeError::BadSlot);
        }
        let Cap::Untyped { base, size, used } = self.slots[slot] else {
            return Err(RetypeError::NotUntyped);
        };
        if self.used == CNODE_SLOTS {
            return Err(RetypeError::OutOfSlots);
        }

        // 잘라낼 자리: 워터마크를 그래뉼로 올림 정렬
        let mark = base.checked_add(used).ok_or(RetypeError::Exhausted)?;
        let carve = mark
            .checked_add(granule - 1)
            .ok_or(RetypeError::Exhausted)?
            & !(granule - 1);
        let carve_end = carve.checked_add(granule).ok_or(RetypeError::Exhausted)?;
        if carve_end > base + size {
            return Err(RetypeError::Exhausted);
        }

        if !prep(carve) {
            return Err(RetypeError::PrepFailed);
        }

        // 확정: 워터마크 전진 후 새 케이퍼빌리티 기록
        self.slots[slot] = Cap::Untyped {
            base,
            size,
            used: carve_end - base,
        };
        let new_slot = self.used;
        let cap = match kind {
            ObjKind::Frame => Cap::Frame {
                base: carve,
                mapped: false,
            },
            ObjKind::PageTable => Cap::PageTable {
                base: carve,
                installed: false,
            },
            // 소거된 프레임이 곧 유효한 Inactive TCB (상태 0)
            ObjKind::Tcb => Cap::Tcb { base: carve },
            // 소거된 프레임이 곧 유효한 빈 엔드포인트 (빈 대기 큐 = 0)
            ObjKind::Endpoint => Cap::Endpoint { base: carve },
        };
        self.push(cap).map_err(|_| RetypeError::OutOfSlots)?;
        Ok(new_slot)
    }
}

struct SyncCell(UnsafeCell<CNode>);

/// # Safety
/// 부트 시퀀스에서는 단일 부트 코어가 bootstrap을 통해 한 번만 쓰고, 이양
/// 후에는 시스템 콜 컨텍스트(단일 코어, 예외 진입 시 DAIF 마스크)가
/// root_mut로 배타적으로 접근하기 때문에 동시 접근이 없습니다.
unsafe impl Sync for SyncCell {}

static ROOT_CNODE: SyncCell = SyncCell(UnsafeCell::new(CNode {
    slots: [Cap::Empty; CNODE_SLOTS],
    used: 0,
}));

/// 이양 후 시스템 콜 처리부가 루트 CNode를 변이하기 위한 함수입니다.
///
/// # Safety
/// 단일 코어에서 예외 진입으로 DAIF가 마스크된 시스템 콜 컨텍스트에서만
/// 호출해야 하고, bootstrap이 준 부트 시점의 공유 참조가 더 이상 살아 있지
/// 않아야 합니다(이양의 스택 되감기로 소멸됨). 반환된 가변 참조를 시스템 콜
/// 처리 밖으로 유출하면 안 됩니다.
pub unsafe fn root_mut() -> &'static mut CNode {
    // SAFETY: 함수 계약대로 이 접근은 배타적임
    unsafe { &mut *ROOT_CNODE.0.get() }
}

/// 루트 CNode를 구성하는 부트스트랩 함수입니다.
///
/// DTB의 물리 메모리 맵에서 예약 구간을 뺀 나머지를 untyped 케이퍼빌리티로
/// 만듭니다. 예약 구간에는 커널 이미지/DTB/부트 윈도우와 함께 부트로더 및
/// 펌웨어 예약 구간(FDT memreserve, /reserved-memory)이 전부 들어와야
/// 합니다. 예약 구간끼리 겹쳐도 동작합니다(커서가 단조 전진).
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
    cnode.push(Cap::RootTcb)?;
    cnode.push(Cap::AddrSpace {
        root_pa: addr_space_root,
    })?;
    cnode.push(Cap::Console)?;

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
                        used: 0,
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
                        used: 0,
                    })?;
                }
                return Ok(());
            }
        }
    }
}
