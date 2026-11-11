//! 케이퍼빌리티 시스템 크레이트입니다.
//!
//! # Features
//! seL4 방식의 자원 모델을 따릅니다. 커널은 heap 없이 동작하고, 커널/DTB/부트
//! 윈도우/펌웨어 예약 구간을 제외한 물리 메모리 전부를 untyped 케이퍼빌리티로
//! 만들어 루트 태스크에 넘깁니다. 이후의 모든 커널 오브젝트는 재분류(retype)가
//! untyped에서 워터마크(범프) 방식으로 잘라낸 메모리에 자리 잡습니다.
//!
//! 케이퍼빌리티는 64바이트 슬롯에 원시 워드로 저장되고 사용할 때마다 검증하며
//! 해석합니다. 슬롯은 CNode 오브젝트와 TCB 오브젝트 안에만 있고 태스크마다
//! 자기 CNode를 가지므로, 슬롯 번호는 호출한 태스크의 CNode 안에서만 의미를
//! 갖습니다. 슬롯들은 파생 리스트(전위 순서의 이중 연결 리스트 + 깊이)로
//! 엮입니다. 복사본은 원본의 자식이고, 재분류로 생긴 케이퍼빌리티는 그
//! untyped의 자식입니다. 삭제와 회수는 자손을 남기지 않으므로 자식이 부모보다
//! 오래 살 수 없고, 오브젝트의 원본(ORIGINAL) 케이퍼빌리티가 사라진 채로
//! 복사본만 남는 일이 없습니다. 원본이 지워지는 순간이 오브젝트의 종료
//! 시점입니다. 다만 한 번의 호출 안에서의 순서는 보장하지 않습니다. 조상을
//! 회수할 때는 리스트 순서대로 지우므로 원본의 콜백이 그 복사본들의
//! 콜백보다 먼저 불릴 수 있고, 복사본들은 같은 호출이 반환하기 전에 모두
//! 사라집니다.
//!
//! 이 크레이트는 장부(bookkeeping)만 다룹니다. 메모리 준비(별칭 매핑과
//! 소거)와 오브젝트 종료 처리(매핑 해제, 큐 정리 등)는 호출자가 콜백으로
//! 수행합니다. 재귀는 쓰지 않습니다. CNode와 TCB처럼 슬롯을 담는 오브젝트가
//! 종료되면 보류 목록에 걸어 두었다가 반복문으로 비우므로 커널 스택 사용량이
//! 파생 깊이나 CNode 중첩과 무관하게 일정합니다.
//!
//! # Errors
//! 실패한 연산은 상태를 바꾸지 않고 `CapError`를 반환합니다. `Corrupt`는
//! 커널 전용 메모리의 슬롯이 해석 불가능하거나 리스트 링크가 어긋났다는
//! 뜻이므로 호출자는 fail-secure 정지해야 합니다.

#![cfg_attr(not(test), no_std)]

use core::ptr;

/// 슬롯 하나의 크기(바이트)
pub const SLOT_SIZE: usize = 64;

/// 루트 CNode의 슬롯 수
pub const ROOT_CNODE_SLOTS: usize = 512;

/// 파생 깊이 상한, 초과하는 복사는 거부됩니다
pub const MAX_DEPTH: u64 = 1 << 32;

/// 케이퍼빌리티 권한 비트 모듈입니다.
///
/// Frame은 R/W/X를 매핑 권한의 상한으로 쓰고, Endpoint는 R을 수신, W를
/// 송신으로 씁니다. 다른 종류는 권한 비트를 갖지 않습니다.
pub mod rights {
    pub const R: u8 = 1;
    pub const W: u8 = 2;
    pub const X: u8 = 4;
    pub const ALL: u8 = R | W | X;
}

const KIND_MASK: u64 = 0xF;
const RIGHTS_SHIFT: u32 = 4;
const RIGHTS_MASK: u64 = 0x7 << RIGHTS_SHIFT;
const FLAG_ORIGINAL: u64 = 1 << 8;
const FLAG_BOUND: u64 = 1 << 9;
const FLAG_MAPPED: u64 = 1 << 10;
const BASE_MASK: u64 = 0x0000_FFFF_FFFF_F000;
const LEVEL_MASK: u64 = 0x3;

const KIND_UNTYPED: u64 = 1;
const KIND_FRAME: u64 = 2;
const KIND_PAGE_TABLE: u64 = 3;
const KIND_TCB: u64 = 4;
const KIND_ENDPOINT: u64 = 5;
const KIND_CNODE: u64 = 6;
const KIND_ADDR_SPACE: u64 = 7;
const KIND_CONSOLE: u64 = 8;

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

/// Frame / PageTable 케이퍼빌리티가 기억하는 매핑 위치 구조체입니다.
///
/// 주소 공간은 포인터가 아니라 (asid, generation) 쌍으로 가리킵니다. 주소
/// 공간이 먼저 사라지면 세대가 달라져 이 기록은 낡은 것으로 판정되고, 해제
/// 경로는 사라진 테이블을 절대 걷지 않습니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub asid: u16,
    pub generation: u64,
    pub va: u64,
    pub level: u8,
}

/// 케이퍼빌리티 하나를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    /// 소유자가 재분류할 수 있는 미분류 물리 메모리
    ///
    /// `used`는 재분류 워터마크로 base로부터의 소비량입니다
    Untyped { base: u64, size: u64, used: u64 },
    /// 사용자 매핑 가능 프레임, `rights`가 매핑 권한의 상한
    Frame {
        base: u64,
        rights: u8,
        map: Option<Mapping>,
    },
    /// 사용자 주소 공간의 중간 페이지 테이블
    PageTable { base: u64, map: Option<Mapping> },
    /// 태스크 제어 블록, 상태 머신은 오브젝트 안에 있음
    Tcb { base: u64 },
    /// 동기 IPC 엔드포인트, `badge`는 수신자에게 전달되는 송신자 식별값
    Endpoint { base: u64, rights: u8, badge: u64 },
    /// 케이퍼빌리티 슬롯 배열
    CNode { base: u64, slots: u64 },
    /// 사용자 주소 공간(TTBR0 루트 테이블)
    AddrSpace { root_pa: u64, asid: u16 },
    /// 커널 디버그 콘솔 출력 권한(오브젝트 없음)
    Console,
}

/// 재분류로 만들 수 있는 커널 오브젝트 종류를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjKind {
    Frame,
    PageTable,
    Tcb,
    Endpoint,
    CNode,
    AddrSpace,
}

/// 케이퍼빌리티 연산이 거부된 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    /// 원본 슬롯이 비어 있음
    Empty,
    /// 목적지 슬롯이 비어 있지 않음
    Occupied,
    /// 해당 슬롯이 untyped가 아님
    NotUntyped,
    /// untyped의 남은 공간 부족
    Exhausted,
    /// 이 종류의 케이퍼빌리티는 복사하거나 옮길 수 없음
    NotCopyable,
    /// 권한 마스크를 적용하면 남는 권한이 없음
    NoRights,
    /// 이미 배지가 있는 케이퍼빌리티에 다른 배지를 요구했거나 배지를 가질 수
    /// 없는 종류임
    BadBadge,
    /// 파생 깊이 상한 초과
    TooDeep,
    /// 호출자의 메모리 준비(별칭 매핑/소거) 실패
    PrepFailed,
    /// 부트스트랩 입력 구간이 잘못됨
    BadRegion,
    /// 부트스트랩이 CNode 용량을 넘김
    OutOfSlots,
    /// 슬롯 내용이나 리스트 링크가 손상됨(커널 불변식 위반)
    Corrupt,
}

/// 케이퍼빌리티 슬롯 구조체입니다.
///
/// `w`는 케이퍼빌리티 원시 워드, `prev` / `next`는 파생 리스트의 이웃 슬롯
/// 주소(0 = 없음), `depth`는 파생 깊이입니다. 전부 0이면 빈 슬롯이라서
/// 소거된 메모리가 그대로 빈 CNode입니다. `aux`는 컨테이너의 0번 슬롯에서만
/// 보류 목록용으로 쓰입니다.
#[repr(C)]
pub struct Slot {
    w: [u64; 4],
    prev: u64,
    next: u64,
    depth: u64,
    aux: u64,
}

const _: () = assert!(size_of::<Slot>() == SLOT_SIZE);

impl Slot {
    /// 빈 슬롯 값
    pub const EMPTY: Slot = Slot {
        w: [0; 4],
        prev: 0,
        next: 0,
        depth: 0,
        aux: 0,
    };
}

/// 슬롯에서 읽어 낸 케이퍼빌리티와 그 플래그를 담는 구조체입니다.
///
/// `original`은 재분류나 부트스트랩이 만든 오브젝트당 하나뿐인 케이퍼빌리티,
/// `bound`는 TCB 내장 슬롯에 든(태스크에 묶인) 케이퍼빌리티를 뜻합니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub cap: Cap,
    pub original: bool,
    pub bound: bool,
}

/// 슬롯에서 제거된 케이퍼빌리티를 종료 처리 콜백에 알리는 구조체입니다.
#[derive(Clone, Copy, Debug)]
pub struct Removed {
    pub cap: Cap,
    pub original: bool,
    pub bound: bool,
    /// 케이퍼빌리티가 들어 있던 슬롯의 주소(이미 비워진 상태)
    pub slot: *mut Slot,
}

/// 케이퍼빌리티 제거에 따른 오브젝트 처리를 맡는 호출자 측 트레이트입니다.
pub trait Env {
    /// 케이퍼빌리티 하나가 슬롯에서 제거된 직후 불리는 함수입니다.
    ///
    /// 슬롯은 이미 리스트에서 빠지고 비워진 상태입니다. 구현은 어떤 슬롯도
    /// 수정하면 안 됩니다(슬롯을 담은 오브젝트의 정리는 [Env::container]가
    /// 알려 준 배열을 이 크레이트가 직접 비웁니다). 원본의 콜백이 불리는
    /// 시점에 같은 오브젝트의 복사본이 아직 리스트에 남아 있을 수 있으므로,
    /// 구현은 "복사본이 먼저 다 사라졌다"고 가정하면 안 되고 복사본의 콜백이
    /// 이미 종료된 오브젝트에 대해 불려도 안전해야 합니다.
    fn removed(&mut self, r: &Removed);

    /// 원본 케이퍼빌리티가 가리키던 오브젝트가 슬롯을 담고 있으면 그 슬롯
    /// 배열(0번 슬롯의 주소, 슬롯 수)을 주는 함수입니다.
    ///
    /// 0번 슬롯은 케이퍼빌리티를 담지 않는 자리여야 합니다.
    fn container(&mut self, cap: &Cap) -> Option<(*mut Slot, usize)>;
}

fn encode(cap: &Cap) -> [u64; 4] {
    let map_words = |m: &Option<Mapping>| match m {
        Some(m) => (FLAG_MAPPED, m.va | u64::from(m.level), u64::from(m.asid), m.generation),
        None => (0, 0, 0, 0),
    };
    match cap {
        // untyped의 base는 임의의 물리 주소일 수 있어 별도 워드에 둠
        Cap::Untyped { base, size, used } => [KIND_UNTYPED, *size, *used, *base],
        Cap::Frame { base, rights, map } => {
            let (f, w1, w2, w3) = map_words(map);
            [
                base | KIND_FRAME | (u64::from(*rights) << RIGHTS_SHIFT) | f,
                w1,
                w2,
                w3,
            ]
        }
        Cap::PageTable { base, map } => {
            let (f, w1, w2, w3) = map_words(map);
            [base | KIND_PAGE_TABLE | f, w1, w2, w3]
        }
        Cap::Tcb { base } => [base | KIND_TCB, 0, 0, 0],
        Cap::Endpoint {
            base,
            rights,
            badge,
        } => [
            base | KIND_ENDPOINT | (u64::from(*rights) << RIGHTS_SHIFT),
            *badge,
            0,
            0,
        ],
        Cap::CNode { base, slots } => [base | KIND_CNODE, *slots, 0, 0],
        Cap::AddrSpace { root_pa, asid } => [root_pa | KIND_ADDR_SPACE, u64::from(*asid), 0, 0],
        Cap::Console => [KIND_CONSOLE, 0, 0, 0],
    }
}

fn decode(w: &[u64; 4]) -> Result<Option<Entry>, CapError> {
    let w0 = w[0];
    let kind = w0 & KIND_MASK;
    if kind == 0 {
        return if w.iter().all(|&x| x == 0) {
            Ok(None)
        } else {
            Err(CapError::Corrupt)
        };
    }
    let known = KIND_MASK | RIGHTS_MASK | FLAG_ORIGINAL | FLAG_BOUND | FLAG_MAPPED | BASE_MASK;
    if w0 & !known != 0 {
        return Err(CapError::Corrupt);
    }
    let base = w0 & BASE_MASK;
    let rights = ((w0 & RIGHTS_MASK) >> RIGHTS_SHIFT) as u8;
    let mapped = w0 & FLAG_MAPPED != 0;
    let map = || -> Result<Option<Mapping>, CapError> {
        if !mapped {
            return if w[1] == 0 && w[2] == 0 && w[3] == 0 {
                Ok(None)
            } else {
                Err(CapError::Corrupt)
            };
        }
        let asid = u16::try_from(w[2]).map_err(|_| CapError::Corrupt)?;
        if w[1] & !(BASE_MASK | LEVEL_MASK) != 0 {
            return Err(CapError::Corrupt);
        }
        Ok(Some(Mapping {
            asid,
            generation: w[3],
            va: w[1] & BASE_MASK,
            level: (w[1] & LEVEL_MASK) as u8,
        }))
    };
    let plain = |tail_zero: bool| -> Result<(), CapError> {
        if rights != 0 || mapped || !tail_zero {
            Err(CapError::Corrupt)
        } else {
            Ok(())
        }
    };
    let cap = match kind {
        KIND_UNTYPED => {
            plain(base == 0)?;
            let (size, used, base) = (w[1], w[2], w[3]);
            if size == 0 || base.checked_add(size).is_none() || used > size {
                return Err(CapError::Corrupt);
            }
            Cap::Untyped { base, size, used }
        }
        KIND_FRAME => {
            if rights == 0 {
                return Err(CapError::Corrupt);
            }
            Cap::Frame {
                base,
                rights,
                map: map()?,
            }
        }
        KIND_PAGE_TABLE => {
            if rights != 0 {
                return Err(CapError::Corrupt);
            }
            Cap::PageTable { base, map: map()? }
        }
        KIND_TCB => {
            plain(w[1] == 0 && w[2] == 0 && w[3] == 0)?;
            Cap::Tcb { base }
        }
        KIND_ENDPOINT => {
            if rights == 0 || rights & rights::X != 0 || mapped || w[2] != 0 || w[3] != 0 {
                return Err(CapError::Corrupt);
            }
            Cap::Endpoint {
                base,
                rights,
                badge: w[1],
            }
        }
        KIND_CNODE => {
            plain(w[2] == 0 && w[3] == 0)?;
            if w[1] < 2 {
                return Err(CapError::Corrupt);
            }
            Cap::CNode { base, slots: w[1] }
        }
        KIND_ADDR_SPACE => {
            plain(w[2] == 0 && w[3] == 0)?;
            let asid = u16::try_from(w[1]).map_err(|_| CapError::Corrupt)?;
            Cap::AddrSpace {
                root_pa: base,
                asid,
            }
        }
        KIND_CONSOLE => {
            plain(w[1] == 0 && w[2] == 0 && w[3] == 0)?;
            if base != 0 {
                return Err(CapError::Corrupt);
            }
            Cap::Console
        }
        _ => return Err(CapError::Corrupt),
    };
    Ok(Some(Entry {
        cap,
        original: w0 & FLAG_ORIGINAL != 0,
        bound: w0 & FLAG_BOUND != 0,
    }))
}

/// 슬롯 내용을 검증하며 읽는 함수입니다. 빈 슬롯이면 `None`
///
/// # Errors
/// 해석 불가능한 내용이면 `Corrupt`
///
/// # Safety
/// `slot`은 커널 전용 메모리의 유효한 슬롯이고 접근이 배타적이어야 합니다.
pub unsafe fn read(slot: *const Slot) -> Result<Option<Entry>, CapError> {
    // SAFETY: 함수 계약대로 slot은 유효함
    let w = unsafe { (*slot).w };
    let e = decode(&w)?;
    if e.is_none() {
        // 빈 슬롯은 리스트에 걸려 있으면 안 됨
        // SAFETY: 위와 동일
        let (p, n, d) = unsafe { ((*slot).prev, (*slot).next, (*slot).depth) };
        if p != 0 || n != 0 || d != 0 {
            return Err(CapError::Corrupt);
        }
    }
    Ok(e)
}

/// 슬롯에 든 케이퍼빌리티의 종류 고유 상태(워터마크, 매핑 기록)를 갱신하는
/// 함수입니다. 종류, 오브젝트, 권한, 플래그, 리스트 위치는 그대로 둡니다.
///
/// 허용되는 갱신은 Untyped의 워터마크와 Frame / PageTable의 매핑 기록
/// 뿐입니다.
///
/// # Errors
/// 빈 슬롯이면 `Empty`, 그 외의 필드가 달라지는 갱신이면 `Corrupt`
///
/// # Safety
/// [read]와 동일합니다.
pub unsafe fn update(slot: *mut Slot, cap: Cap) -> Result<(), CapError> {
    // SAFETY: 함수 계약을 중계함
    let old = unsafe { read(slot) }?.ok_or(CapError::Empty)?;
    let same = match (old.cap, cap) {
        (
            Cap::Untyped { base: a, size: b, .. },
            Cap::Untyped {
                base: c,
                size: d,
                used,
            },
        ) => a == c && b == d && used <= d,
        (
            Cap::Frame {
                base: a, rights: b, ..
            },
            Cap::Frame {
                base: c, rights: d, ..
            },
        ) => a == c && b == d,
        (Cap::PageTable { base: a, .. }, Cap::PageTable { base: c, .. }) => a == c,
        _ => false,
    };
    if !same {
        return Err(CapError::Corrupt);
    }
    let new = encode(&cap);
    let flags = (if old.original { FLAG_ORIGINAL } else { 0 }) | (if old.bound { FLAG_BOUND } else { 0 });
    // SAFETY: 함수 계약대로 slot은 유효함
    unsafe {
        (*slot).w = [new[0] | flags, new[1], new[2], new[3]];
    }
    Ok(())
}

/// `pos` 바로 뒤에 `new`를 끼워 넣는 함수입니다.
///
/// # Safety
/// 두 슬롯 모두 유효하고 `new`는 리스트 밖에 있어야 합니다.
unsafe fn link_after(pos: *mut Slot, new: *mut Slot, depth: u64) {
    // SAFETY: 함수 계약대로 두 슬롯과 pos의 이웃은 유효함
    unsafe {
        let next = (*pos).next;
        (*new).prev = pos as u64;
        (*new).next = next;
        (*new).depth = depth;
        if next != 0 {
            (*(next as *mut Slot)).prev = new as u64;
        }
        (*pos).next = new as u64;
    }
}

/// 슬롯을 리스트에서 빼는 함수입니다. 이웃의 역링크가 이 슬롯을 가리키지
/// 않으면 아무것도 바꾸지 않고 `Corrupt`를 반환합니다.
///
/// # Safety
/// `slot`과 그 이웃이 유효한 슬롯이어야 합니다.
unsafe fn unlink(slot: *mut Slot) -> Result<(), CapError> {
    // SAFETY: 함수 계약대로 slot과 이웃은 유효함
    unsafe {
        let (p, n) = ((*slot).prev as *mut Slot, (*slot).next as *mut Slot);
        if (!p.is_null() && (*p).next != slot as u64) || (!n.is_null() && (*n).prev != slot as u64)
        {
            return Err(CapError::Corrupt);
        }
        if !p.is_null() {
            (*p).next = n as u64;
        }
        if !n.is_null() {
            (*n).prev = p as u64;
        }
    }
    Ok(())
}

/// 슬롯의 첫 자손(리스트에서 바로 다음이면서 더 깊은 슬롯)을 주는 함수입니다.
/// 없으면 null
///
/// # Safety
/// `slot`과 그 다음 슬롯이 유효해야 합니다.
unsafe fn descendant(slot: *const Slot) -> *mut Slot {
    // SAFETY: 함수 계약대로 slot과 next는 유효함
    unsafe {
        let n = (*slot).next as *mut Slot;
        if n.is_null() || (*n).depth <= (*slot).depth {
            ptr::null_mut()
        } else {
            n
        }
    }
}

/// 슬롯에 자손이 있는지 확인하는 함수입니다.
///
/// # Safety
/// [read]와 동일합니다.
pub unsafe fn has_descendants(slot: *const Slot) -> bool {
    // SAFETY: 함수 계약을 중계함
    !unsafe { descendant(slot) }.is_null()
}

/// 빈 슬롯에 새 리스트의 뿌리가 되는 원본 케이퍼빌리티를 넣는 함수입니다.
///
/// 부트스트랩 전용입니다. 부트 시점의 오브젝트는 untyped 밑에 있지 않으므로
/// 각자 독립된 리스트의 뿌리(깊이 0)가 됩니다.
///
/// # Errors
/// 슬롯이 비어 있지 않으면 `Occupied`
///
/// # Safety
/// [read]와 동일합니다.
pub unsafe fn insert_root(slot: *mut Slot, cap: Cap) -> Result<(), CapError> {
    // SAFETY: 함수 계약을 중계함
    if unsafe { read(slot) }?.is_some() {
        return Err(CapError::Occupied);
    }
    let w = encode(&cap);
    // SAFETY: 함수 계약대로 slot은 유효함
    unsafe {
        (*slot).w = [w[0] | FLAG_ORIGINAL, w[1], w[2], w[3]];
    }
    Ok(())
}

/// untyped에서 커널 오브젝트 하나를 잘라내는(retype) 함수입니다.
///
/// 워터마크를 그래뉼로 올림 정렬한 위치가 잘라낼 자리입니다. untyped에
/// 자손이 하나도 없으면 그 구간에 살아 있는 오브젝트가 없다는 뜻이므로
/// 워터마크를 처음으로 되돌려 메모리를 재사용합니다. 모든 검사를 통과한 뒤
/// `prep`으로 호출자에게 물리 메모리 준비(별칭 매핑과 소거)를 맡기고,
/// `prep`이 성공한 경우에만 워터마크 전진과 새 케이퍼빌리티 기록을
/// 확정합니다. 새 케이퍼빌리티는 그 오브젝트의 원본이며 untyped의 자식으로
/// 리스트에 들어갑니다. 실패 경로에서는 상태가 일절 변하지 않습니다.
///
/// # Arguments
/// `ut` - 원본 untyped가 든 슬롯
/// `dest` - 새 케이퍼빌리티를 받을 빈 슬롯
/// `kind` - 만들 오브젝트 종류
/// `granule` - 오브젝트 크기이자 정렬(플랫폼 그래뉼, 2의 거듭제곱)
/// `prep` - 잘라낸 PA를 받아 메모리를 준비하는 콜백, AddrSpace는 등록한
/// asid를 돌려주고 그 외에는 0을 돌려줌
///
/// # Errors
/// untyped 아님은 `NotUntyped`, 목적지 점유는 `Occupied`, 공간 부족은
/// `Exhausted`, 깊이 초과는 `TooDeep`, 콜백 실패는 `PrepFailed`
///
/// # Safety
/// 두 슬롯 모두 [read]의 조건을 만족해야 합니다.
pub unsafe fn retype(
    ut: *mut Slot,
    dest: *mut Slot,
    kind: ObjKind,
    granule: u64,
    prep: impl FnOnce(u64) -> Option<u16>,
) -> Result<(), CapError> {
    // SAFETY: 함수 계약을 중계함
    let src = unsafe { read(ut) }?.ok_or(CapError::NotUntyped)?;
    let Cap::Untyped { base, size, used } = src.cap else {
        return Err(CapError::NotUntyped);
    };
    // SAFETY: 함수 계약을 중계함
    if unsafe { read(dest) }?.is_some() {
        return Err(CapError::Occupied);
    }
    if granule < SLOT_SIZE as u64 || !granule.is_power_of_two() {
        return Err(CapError::Exhausted);
    }
    // 자손이 없으면 이 구간에 살아 있는 오브젝트가 없으므로 처음부터 재사용
    // SAFETY: 함수 계약을 중계함
    let used = if unsafe { has_descendants(ut) } { used } else { 0 };

    // 잘라낼 자리: 워터마크를 그래뉼로 올림 정렬
    let mark = base.checked_add(used).ok_or(CapError::Exhausted)?;
    let carve = mark.checked_add(granule - 1).ok_or(CapError::Exhausted)? & !(granule - 1);
    let carve_end = carve.checked_add(granule).ok_or(CapError::Exhausted)?;
    let end = base.checked_add(size).ok_or(CapError::Corrupt)?;
    if carve_end > end {
        return Err(CapError::Exhausted);
    }
    // 오브젝트의 PA는 슬롯 인코딩(4KiB 정렬, 48비트)에 들어가야 함. 그 밖의
    // 주소는 오브젝트가 될 수 없음
    if carve & !BASE_MASK != 0 {
        return Err(CapError::Exhausted);
    }
    // SAFETY: 함수 계약대로 ut는 유효함
    let depth = unsafe { (*ut).depth }
        .checked_add(1)
        .filter(|d| *d <= MAX_DEPTH)
        .ok_or(CapError::TooDeep)?;

    let asid = prep(carve).ok_or(CapError::PrepFailed)?;

    let cap = match kind {
        ObjKind::Frame => Cap::Frame {
            base: carve,
            rights: rights::R | rights::W,
            map: None,
        },
        ObjKind::PageTable => Cap::PageTable {
            base: carve,
            map: None,
        },
        // 소거된 프레임이 곧 유효한 Inactive TCB (상태 0)
        ObjKind::Tcb => Cap::Tcb { base: carve },
        // 소거된 프레임이 곧 유효한 빈 엔드포인트 (빈 대기 큐 = 0)
        ObjKind::Endpoint => Cap::Endpoint {
            base: carve,
            rights: rights::R | rights::W,
            badge: 0,
        },
        // 소거된 프레임이 곧 빈 슬롯으로 가득 찬 CNode
        ObjKind::CNode => Cap::CNode {
            base: carve,
            slots: granule / SLOT_SIZE as u64,
        },
        // 소거된 프레임이 곧 매핑이 하나도 없는 루트 테이블
        ObjKind::AddrSpace => Cap::AddrSpace {
            root_pa: carve,
            asid,
        },
    };

    // 확정: 워터마크 전진 후 새 케이퍼빌리티 기록
    // SAFETY: 함수 계약을 중계함, 종류와 base가 같은 갱신임
    unsafe {
        update(
            ut,
            Cap::Untyped {
                base,
                size,
                used: carve_end - base,
            },
        )?;
        let w = encode(&cap);
        (*dest).w = [w[0] | FLAG_ORIGINAL, w[1], w[2], w[3]];
        link_after(ut, dest, depth);
    }
    Ok(())
}

/// 케이퍼빌리티를 다른 슬롯으로 복사(파생)하는 함수입니다.
///
/// 복사본은 원본 슬롯의 자식으로 리스트에 들어가므로 원본을 회수하거나
/// 삭제하면 함께 사라집니다. Frame과 Endpoint는 `mask`로 권한을 줄일 수 있고
/// (늘릴 수는 없음), 배지가 없는 Endpoint에는 배지를 붙일 수 있습니다.
/// 복사본의 매핑 기록은 비어 있습니다. Untyped와 PageTable은 복사할 수
/// 없습니다(같은 메모리를 두 번 재분류하거나 같은 테이블을 두 주소 공간에
/// 설치하는 길을 원천 차단).
///
/// # Arguments
/// `src` - 원본 슬롯
/// `dest` - 복사본을 받을 빈 슬롯
/// `mask` - 남길 권한 비트(Frame / Endpoint에만 적용)
/// `badge` - 붙일 배지(0 = 붙이지 않음)
/// `bound` - TCB 내장 슬롯으로의 복사인지 여부
///
/// # Errors
/// 빈 원본은 `Empty`, 목적지 점유는 `Occupied`, 복사 불가 종류는
/// `NotCopyable`, 권한이 남지 않으면 `NoRights`, 배지 규칙 위반은
/// `BadBadge`, 깊이 초과는 `TooDeep`
///
/// # Safety
/// 두 슬롯 모두 [read]의 조건을 만족해야 합니다.
pub unsafe fn copy(
    src: *mut Slot,
    dest: *mut Slot,
    mask: u8,
    badge: u64,
    bound: bool,
) -> Result<(), CapError> {
    // SAFETY: 함수 계약을 중계함
    unsafe {
        let plan = plan_copy(src, dest, mask, badge)?;
        commit_copy(src, dest, plan, bound);
    }
    Ok(())
}

/// 서로 다른 두 케이퍼빌리티를 TCB의 내장 슬롯 두 개로 한꺼번에 복사하는
/// 함수입니다.
///
/// 두 복사를 모두 검증한 뒤에만 기록하므로 한쪽만 들어간 상태가 생기지
/// 않습니다(태스크 구성의 원자성). 복사본은 둘 다 TCB에 묶인 것으로
/// 표시됩니다.
///
/// # Errors
/// [copy]와 같고, 실패하면 두 목적지 모두 비어 있는 그대로입니다. 네 슬롯이
/// 서로 달라야 하며 겹치면 `Occupied`
///
/// # Safety
/// 네 슬롯 모두 [read]의 조건을 만족해야 합니다.
pub unsafe fn copy_pair_bound(
    src_a: *mut Slot,
    dest_a: *mut Slot,
    src_b: *mut Slot,
    dest_b: *mut Slot,
) -> Result<(), CapError> {
    if dest_a == dest_b || src_a == dest_b || src_b == dest_a {
        return Err(CapError::Occupied);
    }
    // SAFETY: 함수 계약을 중계함. 검증 두 번은 상태를 바꾸지 않고, 첫 기록은
    //         dest_a와 src_a 주변 링크만 건드리므로 둘째 계획(src_b의 내용과
    //         깊이, dest_b의 비어 있음)은 그대로 유효함
    unsafe {
        let plan_a = plan_copy(src_a, dest_a, rights::ALL, 0)?;
        let plan_b = plan_copy(src_b, dest_b, rights::ALL, 0)?;
        commit_copy(src_a, dest_a, plan_a, true);
        commit_copy(src_b, dest_b, plan_b, true);
    }
    Ok(())
}

/// 복사를 검증만 하고 기록할 내용(케이퍼빌리티 워드, 깊이)을 주는 함수입니다.
///
/// # Safety
/// 두 슬롯 모두 [read]의 조건을 만족해야 합니다.
unsafe fn plan_copy(
    src: *mut Slot,
    dest: *mut Slot,
    mask: u8,
    badge: u64,
) -> Result<([u64; 4], u64), CapError> {
    // SAFETY: 함수 계약을 중계함
    let e = unsafe { read(src) }?.ok_or(CapError::Empty)?;
    // SAFETY: 함수 계약을 중계함
    if unsafe { read(dest) }?.is_some() {
        return Err(CapError::Occupied);
    }
    let cap = match e.cap {
        Cap::Untyped { .. } | Cap::PageTable { .. } => return Err(CapError::NotCopyable),
        Cap::Frame { base, rights, .. } => {
            if badge != 0 {
                return Err(CapError::BadBadge);
            }
            let rights = rights & mask;
            if rights == 0 {
                return Err(CapError::NoRights);
            }
            Cap::Frame {
                base,
                rights,
                map: None,
            }
        }
        Cap::Endpoint {
            base,
            rights,
            badge: old,
        } => {
            let rights = rights & mask;
            if rights == 0 {
                return Err(CapError::NoRights);
            }
            let badge = match (old, badge) {
                (0, b) => b,
                (o, 0) => o,
                (o, b) if o == b => o,
                _ => return Err(CapError::BadBadge),
            };
            Cap::Endpoint {
                base,
                rights,
                badge,
            }
        }
        other => {
            // 배지는 엔드포인트 전용. 다른 종류의 워드에 사용자 값이 섞이면
            // asid나 매핑 기록을 위조하는 길이 됨
            if badge != 0 {
                return Err(CapError::BadBadge);
            }
            other
        }
    };
    // SAFETY: 함수 계약대로 src는 유효함
    let depth = unsafe { (*src).depth }
        .checked_add(1)
        .filter(|d| *d <= MAX_DEPTH)
        .ok_or(CapError::TooDeep)?;
    Ok((encode(&cap), depth))
}

/// 검증을 마친 복사를 기록하는 함수입니다. 실패하지 않습니다.
///
/// # Safety
/// `plan`은 같은 `src` / `dest`에 대해 [plan_copy]가 방금 준 것이어야 하고 그
/// 사이에 두 슬롯이 바뀌지 않았어야 합니다.
unsafe fn commit_copy(src: *mut Slot, dest: *mut Slot, plan: ([u64; 4], u64), bound: bool) {
    let (w, depth) = plan;
    let flags = if bound { FLAG_BOUND } else { 0 };
    // SAFETY: 함수 계약대로 두 슬롯은 유효하고 dest는 리스트 밖의 빈 슬롯임
    unsafe {
        (*dest).w = [w[0] | flags, w[1], w[2], w[3]];
        link_after(src, dest, depth);
    }
}

/// 케이퍼빌리티를 다른 슬롯으로 옮기는 함수입니다.
///
/// 리스트에서의 위치(부모와 자손), 플래그, 매핑 기록이 그대로 따라갑니다.
/// TCB에 묶인 케이퍼빌리티는 옮길 수 없습니다.
///
/// # Errors
/// 빈 원본은 `Empty`, 목적지 점유는 `Occupied`, 묶인 케이퍼빌리티는
/// `NotCopyable`
///
/// # Safety
/// 두 슬롯 모두 [read]의 조건을 만족해야 합니다.
pub unsafe fn move_cap(src: *mut Slot, dest: *mut Slot) -> Result<(), CapError> {
    // SAFETY: 함수 계약을 중계함
    let e = unsafe { read(src) }?.ok_or(CapError::Empty)?;
    // SAFETY: 함수 계약을 중계함
    if unsafe { read(dest) }?.is_some() {
        return Err(CapError::Occupied);
    }
    if e.bound {
        return Err(CapError::NotCopyable);
    }
    // SAFETY: 함수 계약대로 두 슬롯과 src의 이웃은 유효함
    unsafe {
        let (p, n) = ((*src).prev as *mut Slot, (*src).next as *mut Slot);
        if (!p.is_null() && (*p).next != src as u64) || (!n.is_null() && (*n).prev != src as u64)
        {
            return Err(CapError::Corrupt);
        }
        (*dest).w = (*src).w;
        (*dest).prev = p as u64;
        (*dest).next = n as u64;
        (*dest).depth = (*src).depth;
        if !p.is_null() {
            (*p).next = dest as u64;
        }
        if !n.is_null() {
            (*n).prev = dest as u64;
        }
        src.write(Slot::EMPTY);
    }
    Ok(())
}

/// 슬롯 하나를 리스트에서 빼고 비운 뒤 종료 처리 콜백을 부르는 함수입니다.
///
/// 원본 케이퍼빌리티였고 그 오브젝트가 슬롯을 담고 있으면 보류 목록에
/// 걸어 둡니다.
///
/// # Safety
/// `slot`은 비어 있지 않은 유효한 슬롯이어야 합니다.
unsafe fn remove(slot: *mut Slot, env: &mut impl Env, pending: &mut u64) -> Result<(), CapError> {
    // SAFETY: 함수 계약을 중계함
    let e = unsafe { read(slot) }?.ok_or(CapError::Corrupt)?;
    // SAFETY: 함수 계약대로 slot과 이웃은 유효함
    unsafe {
        unlink(slot)?;
        slot.write(Slot::EMPTY);
    }
    env.removed(&Removed {
        cap: e.cap,
        original: e.original,
        bound: e.bound,
        slot,
    });
    if e.original
        && let Some((base, n)) = env.container(&e.cap)
    {
        if base.is_null() || n < 2 {
            return Err(CapError::Corrupt);
        }
        // 0번 슬롯은 케이퍼빌리티를 담지 않으므로 보류 목록의 링크로 씀
        // SAFETY: container 계약대로 base는 유효한 슬롯 배열의 선두임
        unsafe {
            if (*base).w != [0; 4] || (*base).next != 0 || (*base).aux != 0 {
                return Err(CapError::Corrupt);
            }
            (*base).next = *pending;
            (*base).aux = n as u64;
        }
        *pending = base as u64;
    }
    Ok(())
}

/// 슬롯의 자손을 전부 제거하는 함수입니다. (보류 목록은 비우지 않음)
///
/// # Safety
/// `slot`은 유효한 슬롯이어야 합니다.
unsafe fn revoke_one(
    slot: *mut Slot,
    env: &mut impl Env,
    pending: &mut u64,
) -> Result<(), CapError> {
    loop {
        // SAFETY: 함수 계약을 중계함
        let n = unsafe { descendant(slot) };
        if n.is_null() {
            return Ok(());
        }
        // SAFETY: 리스트에 걸린 슬롯은 유효하고 비어 있지 않음
        unsafe { remove(n, env, pending) }?;
    }
}

/// 슬롯의 자손을 전부 제거한 뒤 슬롯 자신도 제거하는 함수입니다. (보류
/// 목록은 비우지 않음)
///
/// # Safety
/// `slot`은 유효한 슬롯이어야 합니다.
unsafe fn delete_one(
    slot: *mut Slot,
    env: &mut impl Env,
    pending: &mut u64,
) -> Result<(), CapError> {
    // SAFETY: 함수 계약을 중계함
    if unsafe { read(slot) }?.is_none() {
        return Ok(());
    }
    // SAFETY: 함수 계약을 중계함
    unsafe {
        revoke_one(slot, env, pending)?;
        remove(slot, env, pending)
    }
}

/// 보류 목록에 걸린 슬롯 배열들을 전부 비우는 함수입니다.
///
/// 비우는 중에 또 다른 컨테이너가 종료되면 목록에 추가되고 같은 반복문이
/// 이어서 처리합니다.
///
/// # Safety
/// 보류 목록의 항목은 [Env::container]가 준 유효한 슬롯 배열이어야 합니다.
unsafe fn drain(env: &mut impl Env, pending: &mut u64) -> Result<(), CapError> {
    while *pending != 0 {
        let base = *pending as *mut Slot;
        // SAFETY: 보류 목록의 항목은 유효한 슬롯 배열의 0번 슬롯임
        let n = unsafe {
            *pending = (*base).next;
            let n = (*base).aux;
            (*base).next = 0;
            (*base).aux = 0;
            n
        };
        let n = usize::try_from(n).map_err(|_| CapError::Corrupt)?;
        for i in 1..n {
            // SAFETY: i는 배열 범위 안임
            unsafe { delete_one(base.add(i), env, pending) }?;
        }
    }
    Ok(())
}

/// 슬롯에서 파생된 케이퍼빌리티를 전부 회수(revoke)하는 함수입니다.
///
/// 슬롯 자신은 남습니다. 자손이 가리키던 오브젝트 중 원본이 제거된 것은
/// 종료 처리되고, 그 오브젝트가 담고 있던 케이퍼빌리티까지 연쇄적으로
/// 정리된 뒤에 반환합니다. 빈 슬롯이면 아무 일도 하지 않습니다.
///
/// # Errors
/// 슬롯이나 리스트가 손상됐으면 `Corrupt`
///
/// # Safety
/// `slot`은 [read]의 조건을 만족해야 하고, 리스트로 이어진 모든 슬롯과
/// `env`가 주는 슬롯 배열이 유효해야 합니다.
pub unsafe fn revoke(slot: *mut Slot, env: &mut impl Env) -> Result<(), CapError> {
    let mut pending = 0u64;
    // SAFETY: 함수 계약을 중계함
    unsafe {
        if read(slot)?.is_none() {
            return Ok(());
        }
        revoke_one(slot, env, &mut pending)?;
        drain(env, &mut pending)
    }
}

/// 슬롯의 케이퍼빌리티를 그 자손과 함께 삭제하는 함수입니다.
///
/// 원본 케이퍼빌리티를 삭제하면 그 오브젝트가 종료됩니다. 빈 슬롯이면
/// 아무 일도 하지 않습니다.
///
/// # Errors
/// 슬롯이나 리스트가 손상됐으면 `Corrupt`
///
/// # Safety
/// [revoke]와 동일합니다.
pub unsafe fn delete(slot: *mut Slot, env: &mut impl Env) -> Result<(), CapError> {
    let mut pending = 0u64;
    // SAFETY: 함수 계약을 중계함
    unsafe {
        delete_one(slot, env, &mut pending)?;
        drain(env, &mut pending)
    }
}

/// CNode 오브젝트의 슬롯 배열을 가리키는 구조체입니다.
#[derive(Clone, Copy)]
pub struct CNodeRef {
    base: *mut Slot,
    slots: usize,
}

impl CNodeRef {
    /// 슬롯 배열 위에 참조를 만드는 함수입니다.
    ///
    /// # Safety
    /// `base`는 `slots`개의 슬롯이 연속으로 놓인 커널 전용 메모리의 선두여야
    /// 하고, 참조가 쓰이는 동안 그 메모리가 유효해야 합니다.
    pub unsafe fn new(base: *mut Slot, slots: usize) -> Self {
        Self { base, slots }
    }

    /// 슬롯 수를 주는 함수입니다. (0번 포함)
    pub fn len(&self) -> usize {
        self.slots
    }

    /// 슬롯이 하나도 없는지 확인하는 함수입니다.
    pub fn is_empty(&self) -> bool {
        self.slots == 0
    }

    /// 슬롯 번호를 슬롯 주소로 바꾸는 함수입니다.
    ///
    /// 0번은 null 슬롯이라 어떤 케이퍼빌리티도 가리키지 않고, 범위 밖 번호와
    /// 함께 `None`을 줍니다.
    ///
    /// # Arguments
    /// `index` - 사용자가 준 슬롯 번호
    pub fn slot(&self, index: u64) -> Option<*mut Slot> {
        let i = usize::try_from(index).ok()?;
        if i == 0 || i >= self.slots {
            return None;
        }
        // SAFETY: new의 계약대로 base부터 slots개가 유효하고 i는 범위 안임
        Some(unsafe { self.base.add(i) })
    }
}

/// 루트 CNode에서 고정된 부트 케이퍼빌리티의 슬롯 번호 모듈입니다.
pub mod boot_slot {
    pub const TCB: u64 = 1;
    pub const ADDR_SPACE: u64 = 2;
    pub const CONSOLE: u64 = 3;
    pub const CNODE: u64 = 4;
    /// untyped가 시작되는 슬롯
    pub const FIRST_DYNAMIC: u64 = 5;
}

/// 부트스트랩이 루트 CNode에 넣을 고정 오브젝트들을 담는 구조체입니다.
#[derive(Clone, Copy)]
pub struct BootObjects {
    /// 루트 태스크 TCB 프레임의 PA
    pub tcb: u64,
    /// 루트 태스크 TTBR0 루트 테이블의 PA
    pub addr_space_root: u64,
    /// 루트 주소 공간에 등록된 asid
    pub addr_space_asid: u16,
    /// 루트 CNode 자신의 PA
    pub cnode: u64,
}

/// 루트 CNode를 구성하는 부트스트랩 함수입니다.
///
/// 고정 슬롯(TCB, AddrSpace, Console, CNode)을 채운 뒤 DTB의 물리 메모리
/// 맵에서 예약 구간을 뺀 나머지를 untyped 케이퍼빌리티로 만듭니다. 예약
/// 구간에는 커널 이미지/DTB/부트 윈도우와 함께 부트로더 및 펌웨어 예약
/// 구간(FDT memreserve, /reserved-memory)이 전부 들어와야 합니다. 예약
/// 구간끼리 겹쳐도 동작합니다(커서가 단조 전진). 메모리 구간끼리 겹치면
/// 같은 물리 프레임이 두 untyped에 들어가 두 번 재분류될 수 있으므로
/// 거부합니다. 부트 케이퍼빌리티는 전부 원본이고 각자 독립된 리스트의
/// 뿌리입니다.
///
/// # Arguments
/// `cnode` - 소거된 루트 CNode
/// `objs` - 고정 슬롯에 넣을 오브젝트들
/// `memory` - DTB가 보고한 물리 메모리 구간들(서로 겹치지 않아야 함)
/// `reserved` - untyped에서 제외할 구간들
///
/// # Errors
/// 구간 산술 위반과 메모리 구간 간 겹침은 `BadRegion`, 슬롯 부족은
/// `OutOfSlots`. 성공 시 다음 빈 슬롯 번호를 반환합니다
///
/// # Safety
/// `cnode`의 슬롯 전부가 비어 있고 접근이 배타적이어야 합니다.
pub unsafe fn bootstrap(
    cnode: &CNodeRef,
    objs: &BootObjects,
    memory: &[PhysRegion],
    reserved: &[PhysRegion],
) -> Result<u64, CapError> {
    for (i, m) in memory.iter().enumerate() {
        if m.size == 0 || m.base.checked_add(m.size).is_none() {
            return Err(CapError::BadRegion);
        }
        if memory[..i]
            .iter()
            .any(|o| o.base < m.end() && m.base < o.end())
        {
            return Err(CapError::BadRegion);
        }
    }
    for r in reserved {
        if r.size == 0 || r.base.checked_add(r.size).is_none() {
            return Err(CapError::BadRegion);
        }
    }

    let fixed = [
        (boot_slot::TCB, Cap::Tcb { base: objs.tcb }),
        (
            boot_slot::ADDR_SPACE,
            Cap::AddrSpace {
                root_pa: objs.addr_space_root,
                asid: objs.addr_space_asid,
            },
        ),
        (boot_slot::CONSOLE, Cap::Console),
        (
            boot_slot::CNODE,
            Cap::CNode {
                base: objs.cnode,
                slots: cnode.len() as u64,
            },
        ),
    ];
    for (index, cap) in fixed {
        let slot = cnode.slot(index).ok_or(CapError::OutOfSlots)?;
        // SAFETY: 함수 계약대로 cnode의 슬롯은 유효함
        unsafe { insert_root(slot, cap) }?;
    }

    let mut next = boot_slot::FIRST_DYNAMIC;
    for m in memory {
        // SAFETY: 함수 계약을 중계함
        unsafe { push_untypeds(cnode, &mut next, *m, reserved) }?;
    }
    Ok(next)
}

/// 다음 빈 슬롯에 부트 원본 케이퍼빌리티를 넣고 번호를 전진시키는 함수입니다.
///
/// # Errors
/// CNode가 가득 차면 `OutOfSlots`
///
/// # Safety
/// [bootstrap]과 동일합니다.
pub unsafe fn push_root(cnode: &CNodeRef, next: &mut u64, cap: Cap) -> Result<u64, CapError> {
    let index = *next;
    let slot = cnode.slot(index).ok_or(CapError::OutOfSlots)?;
    // SAFETY: 함수 계약대로 cnode의 슬롯은 유효함
    unsafe { insert_root(slot, cap) }?;
    *next = index + 1;
    Ok(index)
}

/// 메모리 구간 하나에서 예약 구간들을 뺀 나머지를 untyped로 넣는 함수입니다.
///
/// # Safety
/// [bootstrap]과 동일합니다.
unsafe fn push_untypeds(
    cnode: &CNodeRef,
    next: &mut u64,
    m: PhysRegion,
    reserved: &[PhysRegion],
) -> Result<(), CapError> {
    // 예약 구간을 오름차순으로 훑기 위한 선택 정렬(구간 수가 작아 충분함)
    let mut cursor = m.base;
    loop {
        // cursor 이후에서 가장 낮은 예약 구간을 찾음
        let mut lowest: Option<PhysRegion> = None;
        for r in reserved {
            if r.end() > cursor && r.base < m.end() {
                match lowest {
                    Some(n) if n.base <= r.base => {}
                    _ => lowest = Some(*r),
                }
            }
        }
        let gap_end = match lowest {
            Some(r) => r.base.min(m.end()),
            None => m.end(),
        };
        if gap_end > cursor {
            let cap = Cap::Untyped {
                base: cursor,
                size: gap_end - cursor,
                used: 0,
            };
            // SAFETY: 함수 계약을 중계함
            unsafe { push_root(cnode, next, cap) }?;
        }
        match lowest {
            Some(r) if r.end() < m.end() => cursor = r.end().max(cursor),
            _ => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
