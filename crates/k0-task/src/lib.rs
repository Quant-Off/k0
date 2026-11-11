//! 진입 페이즈 3의 태스크(TCB) 관리 크레이트입니다.
//!
//! # Features
//! 태스크 제어 블록과 엔드포인트 대기 큐의 배치를 정의하고, 검증된 flat
//! 이미지를 프레임에 적재해 루트 태스크의 사용자 주소 공간을 구성하는 스폰
//! 로직을 담습니다. 커널 heap은 없습니다. 루트 태스크의 TCB도 다른 TCB와
//! 똑같이 그래뉼 프레임 하나(부트 윈도우)에 자리 잡습니다. TCB는 자기
//! 케이퍼빌리티 공간(CNode)과 주소 공간을 포인터가 아니라 내장 슬롯에 든
//! 케이퍼빌리티로 쥐고 있어서, 그 케이퍼빌리티가 회수되면 파생 리스트를
//! 통해 커널이 곧바로 알게 됩니다.
//! 세그먼트 권한은 빌드 시점에 W^X가 검사된 메타데이터를 따르고, 코드
//! 적재 후에는 I-캐시를 동기화합니다. 스택 아래는 매핑하지 않아 사용자
//! 스택 오버플로가 즉시 폴트가 됩니다.
//!
//! # Errors
//! 배치 위반과 매핑 실패는 `SpawnError`로 반환합니다. 호출자는 실패 시
//! 부팅을 중단해야 합니다(fail-secure).

#![no_std]

use k0_arch::usermode::Context;
use k0_cap::{Cap, CNodeRef, Slot};
use k0_mm::{FrameAlloc, MmuError, UserPerm, UserSpace, GRANULE, KERNEL_VA_OFFSET};

/// 사용자 스택 최상단 VA (아래로 자람, 이미지와 멀리 떨어뜨림)
pub const USER_STACK_TOP: u64 = 0x1000_0000;
/// 사용자 스택 크기 (양 플랫폼 그래뉼의 공배수)
pub const USER_STACK_SIZE: u64 = 64 * 1024;

/// 적재할 세그먼트 하나를 나타내는 구조체입니다. (k0-boot 메타데이터에서 변환)
#[derive(Clone, Copy)]
pub struct LoadSeg {
    pub va: u64,
    pub memsz: u64,
    pub kind: SegKind,
}

/// 세그먼트 권한 종류를 나타내는 열거형입니다.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SegKind {
    Text,
    Ro,
    Rw,
}

impl SegKind {
    fn perm(self) -> UserPerm {
        match self {
            SegKind::Text => UserPerm::TextUser,
            SegKind::Ro => UserPerm::RoUser,
            SegKind::Rw => UserPerm::RwUser,
        }
    }
}

/// 태스크 상태를 나타내는 열거형입니다.
///
/// 재분류(retype) 직후의 소거된 TCB 프레임이 그대로 유효한 Inactive가
/// 되도록 0을 명시합니다.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// 재분류 직후, 구성 전
    Inactive = 0,
    /// 구성 완료, 실행 대기 진입 전
    Stopped = 1,
    /// 준비 큐에 있음
    Ready = 2,
    /// 현재 실행 중
    Running = 3,
    /// 종료됨
    Dead = 4,
    /// 엔드포인트 대기 큐에서 수신자를 기다리는 중(SEND)
    BlockedSend = 5,
    /// 엔드포인트 대기 큐에서 송신자를 기다리는 중(RECV)
    BlockedRecv = 6,
    /// CALL 전달을 마치고 응답을 기다리는 중(수신자의 reply_to가 가리킴)
    BlockedReply = 7,
    /// 엔드포인트 대기 큐에서 수신자를 기다리는 중(CALL, 전달 시 응답 대기로 전환)
    BlockedCall = 8,
}

/// TCB 내장 슬롯 배열에서 케이퍼빌리티 공간(CNode)이 드는 자리
pub const TCB_SLOT_CSPACE: usize = 1;
/// TCB 내장 슬롯 배열에서 주소 공간(AddrSpace)이 드는 자리
pub const TCB_SLOT_VSPACE: usize = 2;
/// TCB 내장 슬롯 수(0번은 CNode와 같이 케이퍼빌리티를 담지 않음)
pub const TCB_SLOTS: usize = 3;

/// 태스크 제어 블록(TCB) 구조체입니다.
///
/// 컨텍스트, 주소 공간 루트, 상태 머신, 큐 링크(intrusive), 내장 슬롯을
/// 담습니다. `next` / `prev`는 이웃 TCB의 커널 VA(0 = 없음)라서 큐 노드
/// 할당이 필요 없고, 준비 큐와 엔드포인트 대기 큐가 함께 씁니다(두 큐의
/// 소속은 상태 머신이 상호 배타로 보장). 이중 연결이라 큐 중간의 TCB도
/// O(1)로 빼낼 수 있습니다. `blocked_on`은 이 태스크가 줄 서 있는
/// 엔드포인트의 커널 VA(0 = 없음)이고 `ipc_badge`는 줄 서 있는 동안 보낼
/// 메시지에 붙을 배지입니다. `reply_to`는 이 태스크가 응답해야 할
/// BlockedReply 호출자의 커널 VA(0 = 없음)로 1회성 응답 자격의 유일한
/// 저장소이고, `reply_from`은 그 역방향(이 태스크의 응답을 쥔 태스크)이라
/// 어느 쪽이 먼저 사라져도 남은 쪽의 링크를 지울 수 있습니다.
///
/// `slots`는 이 태스크에 묶인 케이퍼빌리티입니다. [TCB_SLOT_CSPACE]의
/// CNode가 이 태스크의 시스템 콜이 슬롯 번호를 해석하는 유일한 장소이고,
/// [TCB_SLOT_VSPACE]의 AddrSpace가 이 태스크가 실행되는 주소 공간입니다.
/// 둘 다 구성 때 제시된 케이퍼빌리티의 복사본(자식)이라서 원본이 회수되면
/// 함께 사라지고, 그 순간 태스크는 종료됩니다. `ttbr0_pa`는 주소 공간
/// 루트의 사본으로 내장 슬롯이 살아 있는 동안에만 유효합니다.
///
/// TCB는 그래뉼 프레임 선두에 자리 잡고 커널이 TTBR1 별칭으로만 접근합니다
/// (EL0에는 절대 매핑되지 않음). 소거된 프레임이 그대로 유효한 Inactive
/// TCB입니다.
#[repr(C)]
pub struct Tcb {
    pub ctx: Context,
    pub ttbr0_pa: u64,
    pub state: TaskState,
    pub next: u64,
    pub prev: u64,
    pub reply_to: u64,
    pub reply_from: u64,
    pub blocked_on: u64,
    pub ipc_badge: u64,
    pub slots: [Slot; TCB_SLOTS],
}

impl Tcb {
    /// 내장 슬롯의 주소를 주는 함수입니다.
    ///
    /// # Arguments
    /// `index` - [TCB_SLOT_CSPACE] 또는 [TCB_SLOT_VSPACE]
    ///
    /// # Safety
    /// `tcb`는 유효한 TCB를 가리켜야 합니다.
    pub unsafe fn slot(tcb: *mut Tcb, index: usize) -> *mut Slot {
        // SAFETY: 함수 계약대로 tcb는 유효하고 index는 배열 범위 안의 상수임
        unsafe { (&raw mut (*tcb).slots).cast::<Slot>().add(index % TCB_SLOTS) }
    }

    /// 이 태스크의 케이퍼빌리티 공간(CNode)을 주는 함수입니다.
    ///
    /// 내장 슬롯을 매번 해석하므로 CNode가 회수된 태스크는 어떤 슬롯도
    /// 가리킬 수 없습니다.
    ///
    /// # Errors
    /// 내장 슬롯이 비었거나 CNode가 아니면 `None`, 슬롯 손상은 `Corrupt`
    ///
    /// # Safety
    /// `tcb`는 유효한 TCB를 가리켜야 하고 접근이 배타적이어야 합니다.
    pub unsafe fn cspace(tcb: *mut Tcb) -> Result<Option<CNodeRef>, k0_cap::CapError> {
        // SAFETY: 함수 계약을 중계함
        let e = unsafe { k0_cap::read(Tcb::slot(tcb, TCB_SLOT_CSPACE)) }?;
        Ok(match e {
            Some(k0_cap::Entry {
                cap: Cap::CNode { base, slots },
                ..
            }) => {
                let n = usize::try_from(slots).map_err(|_| k0_cap::CapError::Corrupt)?;
                // SAFETY: CNode 케이퍼빌리티는 재분류나 부트스트랩이 별칭 매핑한
                //         슬롯 배열만 가리키고, 케이퍼빌리티가 살아 있는 동안
                //         그 메모리는 재사용되지 않음
                Some(unsafe { CNodeRef::new((base + KERNEL_VA_OFFSET) as *mut Slot, n) })
            }
            _ => None,
        })
    }
}

// 내장 슬롯의 주소에서 TCB를 되찾을 수 있어야 하므로(슬롯 주소를 그래뉼로
// 내림) TCB 전체가 프레임 하나에 들어가야 함
const _: () = assert!(size_of::<Tcb>() <= 4096);
// 재분류된 TCB는 그래뉼 프레임 하나에 들어가야 함
const _: () = assert!(size_of::<Tcb>() <= GRANULE);

/// TCB의 intrusive 이중 연결 큐 구조체입니다.
///
/// 준비 큐와 엔드포인트 대기 큐가 같은 구현을 씁니다. 값은 전부 TCB의 커널
/// VA(0 = 없음)이고 소거된 메모리가 그대로 빈 큐입니다.
#[repr(C)]
pub struct TcbQueue {
    pub head: u64,
    pub tail: u64,
}

impl TcbQueue {
    /// 빈 큐를 만드는 함수입니다.
    pub const fn new() -> Self {
        Self { head: 0, tail: 0 }
    }

    /// 큐 꼬리에 TCB를 붙이는 함수입니다.
    ///
    /// # Safety
    /// `tcb`는 유효한 커널 VA의 TCB이고 어느 큐에도 없어야 합니다. 큐의
    /// 링크들이 유효한 TCB를 가리켜야 합니다.
    pub unsafe fn push(&mut self, tcb: *mut Tcb) {
        // SAFETY: 함수 계약대로 tcb는 유효하고 큐 밖에 있으며 tail은 유효함
        unsafe {
            (*tcb).next = 0;
            (*tcb).prev = self.tail;
            if self.tail == 0 {
                self.head = tcb as u64;
            } else {
                (*(self.tail as *mut Tcb)).next = tcb as u64;
            }
        }
        self.tail = tcb as u64;
    }

    /// 큐 머리를 꺼내는 함수입니다. 비어 있으면 null
    ///
    /// # Safety
    /// 큐의 링크들이 유효한 TCB를 가리켜야 합니다.
    pub unsafe fn pop(&mut self) -> *mut Tcb {
        let head = self.head as *mut Tcb;
        if head.is_null() {
            return head;
        }
        // SAFETY: 함수 계약대로 head와 그 이웃은 유효한 TCB임
        unsafe {
            self.head = (*head).next;
            if self.head == 0 {
                self.tail = 0;
            } else {
                (*(self.head as *mut Tcb)).prev = 0;
            }
            (*head).next = 0;
            (*head).prev = 0;
        }
        head
    }

    /// 큐 안의 TCB 하나를 위치와 무관하게 빼내는 함수입니다.
    ///
    /// 링크가 이 큐의 머리/꼬리와 맞지 않으면 아무것도 바꾸지 않고 false를
    /// 반환합니다. 그 경우는 큐 손상이므로 호출자는 fail-secure 정지해야
    /// 합니다.
    ///
    /// # Safety
    /// `tcb`는 유효한 TCB이고 이 큐에 들어 있어야 하며, 큐의 링크들이 유효한
    /// TCB를 가리켜야 합니다.
    #[must_use]
    pub unsafe fn remove(&mut self, tcb: *mut Tcb) -> bool {
        // SAFETY: 함수 계약대로 tcb와 그 이웃은 유효한 TCB임
        unsafe {
            let (prev, next) = ((*tcb).prev, (*tcb).next);
            if (prev == 0) != (self.head == tcb as u64) || (next == 0) != (self.tail == tcb as u64)
            {
                return false;
            }
            if prev == 0 {
                self.head = next;
            } else {
                (*(prev as *mut Tcb)).next = next;
            }
            if next == 0 {
                self.tail = prev;
            } else {
                (*(next as *mut Tcb)).prev = prev;
            }
            (*tcb).next = 0;
            (*tcb).prev = 0;
        }
        true
    }
}

impl Default for TcbQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// 동기 랑데부 IPC의 엔드포인트 오브젝트입니다.
///
/// 커널 메시지 버퍼가 없고 대기 큐는 블록된 TCB의 intrusive 링크로만
/// 구성됩니다. 대기 태스크는 준비 큐에 없으므로 링크 재사용이 안전합니다.
/// 큐에는 송신자들 또는 수신자들 한 방향만 담기며 머리 TCB의 상태로 방향을
/// 판별합니다. 재분류(retype)가 소거한 프레임이 그대로 유효한 빈
/// 엔드포인트입니다(0 = 빈 큐).
pub type Endpoint = TcbQueue;

/// 태스크 스폰이 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    BadLayout,
    Mmu(MmuError),
}

impl From<MmuError> for SpawnError {
    fn from(e: MmuError) -> Self {
        SpawnError::Mmu(e)
    }
}

/// 스폰이 만든 루트 태스크의 배치를 담는 구조체입니다.
pub struct RootImage {
    /// 루트 태스크의 TCB(부트 윈도우의 프레임)
    pub tcb: &'static mut Tcb,
    /// TTBR0 루트 테이블의 PA
    pub root_pa: u64,
    /// flat 이미지가 복사된 연속 프레임의 PA(이미지 베이스 VA에 대응)
    pub image_pa: u64,
}

/// 검증된 루트 태스크 이미지를 적재해 TCB를 구성하는 함수입니다.
///
/// flat 이미지를 연속 프레임에 복사하고, 세그먼트 메타데이터대로 사용자
/// 매핑(W^X)을 만들고, 스택과 bootinfo 페이지(RO)를 배치한 뒤 진입
/// 컨텍스트(SPSR EL0t, 인터럽트 언마스크)를 채웁니다. bootinfo 내용과 TCB의
/// 내장 슬롯(케이퍼빌리티 공간과 주소 공간)은 케이퍼빌리티 부트스트랩 이후
/// 호출자가 채웁니다. TTBR0 설치와 이양은 호출자의 몫입니다.
///
/// # Arguments
/// `image` - 무결성 검증을 통과한 flat 이미지
/// `base` - 이미지의 사용자 VA 베이스(그래뉼 정렬)
/// `entry` - 진입점 VA
/// `segs` - 세그먼트 메타데이터(빌드 시점에 W^X 검사 완료)
/// `bootinfo_pa` - bootinfo 페이지로 쓸 프레임의 PA(부트 윈도우에서 할당)
/// `tcb_pa` - TCB로 쓸 소거된 프레임의 PA(부트 윈도우에서 할당)
/// `fa` - 프레임 할당자(부트 윈도우)
///
/// # Errors
/// 정렬/범위 위반은 `BadLayout`, 매핑 실패는 `Mmu`
pub fn spawn_root(
    image: &[u8],
    base: u64,
    entry: u64,
    segs: &[LoadSeg],
    bootinfo_pa: u64,
    tcb_pa: u64,
    fa: &mut FrameAlloc,
) -> Result<RootImage, SpawnError> {
    let g = GRANULE as u64;
    let len = image.len() as u64;
    if base % g != 0 || len % g != 0 || len == 0 || segs.is_empty() || tcb_pa % g != 0 {
        return Err(SpawnError::BadLayout);
    }
    let image_end = base.checked_add(len).ok_or(SpawnError::BadLayout)?;

    // 이미지 전체를 연속 프레임에 복사(예약 시점에 소거됨)
    let frames = fa.alloc_contig(len / g)?;
    // SAFETY: alloc_contig가 준 구간은 TTBR1 별칭으로 접근 가능한 전용 프레임
    unsafe {
        core::ptr::copy_nonoverlapping(
            image.as_ptr(),
            (frames + KERNEL_VA_OFFSET) as *mut u8,
            image.len(),
        );
    }
    k0_arch::usermode::sync_icache((frames + KERNEL_VA_OFFSET) as usize, image.len());

    // 세그먼트 메타데이터대로 사용자 매핑 구성
    let mut us = UserSpace::new(fa)?;
    for s in segs {
        let seg_end = s.va.checked_add(s.memsz).ok_or(SpawnError::BadLayout)?;
        if s.va < base || seg_end > image_end || s.va % g != 0 || s.memsz == 0 {
            return Err(SpawnError::BadLayout);
        }
        let mapped = s.memsz.div_ceil(g) * g;
        us.map_range(fa, s.va, frames + (s.va - base), mapped, s.kind.perm())?;
        if entry >= s.va && entry < seg_end && s.kind != SegKind::Text {
            return Err(SpawnError::BadLayout);
        }
    }
    if !(base..image_end).contains(&entry) {
        return Err(SpawnError::BadLayout);
    }

    // 사용자 스택: 최상단 아래로 USER_STACK_SIZE, 그 아래는 가드(unmapped)
    let stack_base = USER_STACK_TOP - USER_STACK_SIZE;
    let stack_frames = fa.alloc_contig(USER_STACK_SIZE / g)?;
    us.map_range(fa, stack_base, stack_frames, USER_STACK_SIZE, UserPerm::RwUser)?;

    // bootinfo 페이지: EL0에는 읽기 전용, 내용은 이양 전에 커널이 별칭으로 기록
    us.map_range(fa, k0_abi::bootinfo::VA, bootinfo_pa, g, UserPerm::RoUser)?;

    // SAFETY: tcb_pa는 호출자가 부트 윈도우에서 할당한 소거된 전용 프레임이고
    //         (소거 상태가 곧 유효한 Inactive TCB), 단일 부트 코어에서 한 번만
    //         도달하므로 이 가변 참조는 유일함
    let tcb = unsafe { &mut *((tcb_pa + KERNEL_VA_OFFSET) as *mut Tcb) };
    tcb.ctx = Context::zeroed();
    tcb.ctx.elr = entry;
    tcb.ctx.sp = USER_STACK_TOP;
    tcb.ctx.spsr = 0; // EL0t, DAIF 전부 언마스크
    tcb.ttbr0_pa = us.root_pa();
    Ok(RootImage {
        tcb,
        root_pa: us.root_pa(),
        image_pa: frames,
    })
}
