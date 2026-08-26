//! 진입 페이즈 3의 태스크(TCB) 관리 크레이트입니다.
//!
//! # Features
//! 커널 heap 없이 정적으로 자리 잡은 루트 태스크 TCB와, 검증된 flat 이미지를
//! 프레임에 적재해 사용자 주소 공간을 구성하는 스폰 로직을 담습니다.
//! 세그먼트 권한은 빌드 시점에 W^X가 검사된 메타데이터를 따르고, 코드
//! 적재 후에는 I-캐시를 동기화합니다. 스택 아래는 매핑하지 않아 사용자
//! 스택 오버플로가 즉시 폴트가 됩니다.
//!
//! # Errors
//! 배치 위반과 매핑 실패는 `SpawnError`로 반환합니다. 호출자는 실패 시
//! 부팅을 중단해야 합니다(fail-secure).

#![no_std]

use core::cell::UnsafeCell;

use k0_arch::usermode::Context;
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

/// 태스크 제어 블록(TCB) 구조체입니다.
///
/// 컨텍스트, 주소 공간 루트, 상태 머신, 큐 링크(intrusive)를 담습니다.
/// `next`는 다음 TCB의 커널 VA(0 = 없음)라서 큐 노드 할당이 필요 없고,
/// 준비 큐와 엔드포인트 대기 큐가 함께 씁니다(두 큐의 소속은 상태 머신이
/// 상호 배타로 보장). `reply_to`는 이 태스크가 응답해야 할 BlockedReply
/// 호출자의 커널 VA(0 = 없음)로, 1회성 응답 자격의 유일한 저장소입니다.
/// 재분류된 TCB는 그래뉼 프레임 선두에 자리 잡고 커널이 TTBR1 별칭으로만
/// 접근합니다(EL0에는 절대 매핑되지 않음).
pub struct Tcb {
    pub ctx: Context,
    pub ttbr0_pa: u64,
    pub state: TaskState,
    pub next: u64,
    pub reply_to: u64,
}

// 재분류된 TCB는 그래뉼 프레임 하나에 들어가야 함
const _: () = assert!(size_of::<Tcb>() <= GRANULE);

/// 동기 랑데부 IPC의 엔드포인트 오브젝트 구조체입니다.
///
/// 커널 메시지 버퍼가 없고 대기 큐는 블록된 TCB의 intrusive 링크(`next`
/// 재사용)로만 구성됩니다. 대기 태스크는 준비 큐에 없으므로 링크 재사용이
/// 안전합니다. 큐에는 송신자들 또는 수신자들 한 방향만 담기며 머리 TCB의
/// 상태로 방향을 판별합니다. 재분류(retype)가 소거한 프레임이 그대로
/// 유효한 빈 엔드포인트입니다(0 = 빈 큐).
#[repr(C)]
pub struct Endpoint {
    pub head: u64,
    pub tail: u64,
}

impl Endpoint {
    /// 대기 큐 꼬리에 TCB를 붙이는 함수입니다.
    ///
    /// # Safety
    /// `tcb`는 유효한 커널 VA의 TCB이고 어느 큐에도 없어야 합니다.
    pub unsafe fn push(&mut self, tcb: *mut Tcb) {
        // SAFETY: 함수 계약대로 tcb는 유효하고 큐 밖에 있음
        unsafe {
            (*tcb).next = 0;
            if self.tail == 0 {
                self.head = tcb as u64;
            } else {
                (*(self.tail as *mut Tcb)).next = tcb as u64;
            }
        }
        self.tail = tcb as u64;
    }

    /// 대기 큐 머리를 꺼내는 함수입니다. 비어 있으면 0
    ///
    /// # Safety
    /// 큐의 링크들이 유효한 TCB를 가리켜야 합니다.
    pub unsafe fn pop(&mut self) -> u64 {
        let head = self.head;
        if head == 0 {
            return 0;
        }
        // SAFETY: 함수 계약대로 head는 유효한 TCB임
        unsafe {
            self.head = (*(head as *mut Tcb)).next;
            (*(head as *mut Tcb)).next = 0;
        }
        if self.head == 0 {
            self.tail = 0;
        }
        head
    }
}

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

struct SyncCell(UnsafeCell<Tcb>);

/// # Safety
/// 단일 부트 코어가 spawn_root를 통해 한 번만 쓰고, 이후에는 예외 경로가
/// __current_context 포인터를 통해 배타적으로 접근하므로 동시 접근이 없습니다.
unsafe impl Sync for SyncCell {}

static ROOT_TCB: SyncCell = SyncCell(UnsafeCell::new(Tcb {
    ctx: Context::zeroed(),
    ttbr0_pa: 0,
    state: TaskState::Inactive,
    next: 0,
    reply_to: 0,
}));

/// 검증된 루트 태스크 이미지를 적재해 TCB를 구성하는 함수입니다.
///
/// flat 이미지를 연속 프레임에 복사하고, 세그먼트 메타데이터대로 사용자
/// 매핑(W^X)을 만들고, 스택과 bootinfo 페이지(RO)를 배치한 뒤 진입
/// 컨텍스트(SPSR EL0t, 인터럽트 언마스크)를 채웁니다. bootinfo 내용은
/// 케이퍼빌리티 부트스트랩 이후 호출자가 별칭으로 채웁니다. TTBR0 설치와
/// 이양은 호출자의 몫입니다.
///
/// # Arguments
/// `image` - 무결성 검증을 통과한 flat 이미지
/// `base` - 이미지의 사용자 VA 베이스(그래뉼 정렬)
/// `entry` - 진입점 VA
/// `segs` - 세그먼트 메타데이터(빌드 시점에 W^X 검사 완료)
/// `bootinfo_pa` - bootinfo 페이지로 쓸 프레임의 PA(부트 윈도우에서 할당)
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
    fa: &mut FrameAlloc,
) -> Result<(&'static mut Tcb, u64), SpawnError> {
    let g = GRANULE as u64;
    let len = image.len() as u64;
    if base % g != 0 || len % g != 0 || len == 0 || segs.is_empty() {
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

    // SAFETY: 단일 부트 코어에서 한 번만 도달하므로 이 가변 참조는 유일함
    let tcb = unsafe { &mut *ROOT_TCB.0.get() };
    tcb.ctx = Context::zeroed();
    tcb.ctx.elr = entry;
    tcb.ctx.sp = USER_STACK_TOP;
    tcb.ctx.spsr = 0; // EL0t, DAIF 전부 언마스크
    tcb.ttbr0_pa = us.root_pa();
    Ok((tcb, us.root_pa()))
}
