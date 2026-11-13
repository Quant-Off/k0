//! 진입 페이즈 3의 루트 태스크(초기 사용자 공간 서버)입니다.
//!
//! # Features
//! 커널이 무결성 검증 후 EL0로 띄우는 첫 태스크입니다. bootinfo 페이지에서
//! 케이퍼빌리티 목록을 읽고, 커널이 제공하는 기능의 정상 경로와 거부 경로를
//! 부팅할 때마다 자가 검증합니다.
//!
//! - 재분류(RETYPE)와 매핑(MAP / UNMAP)
//! - 케이퍼빌리티 복사·이동·삭제·회수(CAP_COPY / CAP_MOVE / CAP_DELETE /
//!   CAP_REVOKE), 권한 축소와 배지, 회수한 untyped 메모리의 재사용
//! - 태스크 생성(TCB_CONFIGURE / TCB_RESUME)과 스케줄러(양보, 종료, 타이머
//!   선점)
//! - 동기 랑데부 IPC(SEND / RECV / CALL / REPLY_RECV)
//! - 폴트 격리(자식의 폴트와 FP/SIMD 트랩은 그 태스크만 종료, 스레드
//!   포인터는 태스크별 격리)
//! - 태스크 격리: 자기 CNode와 자기 주소 공간만 가진 태스크는 루트의
//!   케이퍼빌리티를 번호로 가리킬 수 없고 루트의 메모리를 읽을 수 없음
//! - 회수에 따른 종료: 대기 중인 태스크의 삭제, 엔드포인트 삭제, CNode
//!   회수, 주소 공간 삭제
//!
//! 검증에 실패하면 "root: FAIL <단계>"를 출력한 뒤 EXIT로 루트 태스크를
//! 끝냅니다. 부팅 시험 스크립트가 그 문구를 실패로 판정합니다. 전부 통과하면
//! 양보 루프로 들어갑니다.
//!
//! 같은 이미지 안의 함수 몇 개는 격리된 태스크의 진입점으로 쓰입니다. 그
//! 태스크의 주소 공간에는 이 이미지의 텍스트와 읽기 전용 데이터만 매핑되고
//! `.data` / `.bss`는 없으므로, 그런 함수는 static을 일절 건드리지 않고
//! 레지스터와 자기 스택, 시스템 콜만 씁니다. 진행 상황은 케이퍼빌리티와
//! 무관한 공유 페이지(pulse)에 값을 써서 알립니다. 그래야 "메시지가 오지
//! 않는다"는 관찰이 "그 태스크가 실행되지 않는다"는 뜻이 됩니다.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "vault")]
mod oracle;
#[cfg(feature = "terminal")]
mod service;
mod timeslice;

use k0_abi::bootinfo::slot;
use k0_abi::{bootinfo, err, ipc, obj, perm, rights, syscall};

/// 재분류 검증에 쓰는 테스트 VA (이미지/스택/bootinfo와 겹치지 않는 구간)
const TEST_VA: u64 = 0x2000_0000;

/// 유한 태스크가 종료 전에 도달해야 하는 카운터 목표값
const CHILD_EXIT_TARGET: u64 = 100_000;

/// untyped 재사용 검증이 루트 CNode의 맨 위에서 쓰는 슬롯 수(그 아래는
/// 범프 할당용)
const REUSE_SLOTS: u64 = 128;

/// 격리된 태스크의 주소 공간 배치 (이미지와 같은 최하위 테이블 범위 안)
const ISO_SHARED_VA: u64 = 0x50_0000;
const ISO_PULSE_VA: u64 = 0x50_8000;
const ISO_STACK_VA: u64 = 0x51_0000;
const ISO_STACK_STEP: u64 = 0x8000;
const ISO_STACKS: u64 = 30;

/// 격리 검증에 쓰는 값들
const ISO_BADGE: u64 = 0xB0B0_0000_0000_0001;
const ISO_MAGIC: u64 = 0x5EC2_E7ED_C0DE_0001;
const ISO_ROOT_ONLY: u64 = 0x0DD_BA11_0DD_BA11;
const ISO_ALL_REFUSED: u64 = 0x1FF;
const BEAT_TAG: u64 = 0xBEA7;
const REPORT_TAG: u64 = 0x3A17;
/// 격리 태스크가 자기 진행 상황을 알리는 값들
const PULSE_REPORTER: u64 = 0x4E90;
const PULSE_HOLDER: u64 = 0x401D_0000;
const PULSE_VIOLATOR: u64 = 0x0710_0000;
const PULSE_SURVIVED: u64 = 0x0BAD_0BAD;
/// 타임아웃 검증의 보고 메시지 꼬리표와 pulse 값, 느린 서버의 응답 변환 값
const TIMED_TAG: u64 = 0x71ED;
const PULSE_TIMED: u64 = 0x0071_0000;
const PULSE_SLOW: u64 = 0x0510_0000;
const SLOW_XOR: u64 = 0x5107;
/// EL0가 읽으려 시도할 커널 절반의 주소(커널 이미지가 놓이는 별칭 근처)
const KERNEL_PROBE: u64 = k0_abi::KERNEL_IMAGE_VA;

// 다음에 쓸 빈 슬롯 번호와 재사용 검증 구간의 시작 (루트 CNode의 슬롯은
// 루트 태스크가 직접 관리)
static NEXT_SLOT: AtomicU64 = AtomicU64::new(0);
static REUSE_BASE: AtomicU64 = AtomicU64::new(0);
// 격리 태스크에 내줄 다음 스택 자리 번호
static NEXT_STACK: AtomicU64 = AtomicU64::new(0);

// 자식 태스크와 공유하는 진행 카운터 (같은 주소 공간)
static EXIT_COUNTER: AtomicU64 = AtomicU64::new(0);

// IPC 서버·클라이언트 태스크와 공유하는 검증 상태 (같은 주소 공간)
static EP_SLOT: AtomicU64 = AtomicU64::new(0);
static EP2_SLOT: AtomicU64 = AtomicU64::new(0);
static SRV_COUNT: AtomicU64 = AtomicU64::new(0);
static SRV_DIE: AtomicU64 = AtomicU64::new(0);
static SRV_HOLD: AtomicU64 = AtomicU64::new(0);
// 클라이언트 CALL의 최종 상태, 1 = 아직 응답 대기 중(유효한 상태값이 아님)
static CLIENT_STATUS: AtomicU64 = AtomicU64::new(1);

// 폴트 격리·스레드 포인터 격리 검증 상태
static FAULT_STAGE: AtomicU64 = AtomicU64::new(0);
static FAULT_SURVIVED: AtomicU64 = AtomicU64::new(0);
static CHILD_SAW_TPIDR: AtomicU64 = AtomicU64::new(0);
const ROOT_TPIDR: u64 = 0x5EED_0000_C0FF_EE01;
const CHILD_TPIDR: u64 = 0xBAD0_0000_0000_0BAD;

// 격리된 태스크가 읽으려 시도할 루트 전용 값 (.data에 있어 그 태스크의 주소
// 공간에는 매핑되지 않음)
static ROOT_SECRET: AtomicU64 = AtomicU64::new(0x5EC2_E700_DEAD_BEEF);

// FIFO·자격 덮어쓰기 검증에 쓰는 클라이언트 메시지
const CM_FIFO_A: [u64; 4] = [0xC1A0, 1, 2, 3];
const CM_FIFO_B: [u64; 4] = [0xC2B0, 4, 5, 6];
const CM_CALL: [u64; 4] = [0xC3C0, 7, 8, 9];
static SRV_MR: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// 인자 여섯 개까지의 시스템 콜을 수행하는 함수입니다.
///
/// static을 쓰지 않으므로 격리된 태스크에서도 부를 수 있습니다.
///
/// # Arguments
/// `nr` - 시스템 콜 번호(x8)
/// `a` - 인자 x0-x5, 반환값은 x0
#[inline(always)]
fn sys(nr: u64, a: [u64; 6]) -> u64 {
    let ret;
    // SAFETY: svc는 커널로의 동기 트랩이고 커널이 컨텍스트 전체를 복원함
    unsafe {
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a[0] => ret,
            in("x1") a[1],
            in("x2") a[2],
            in("x3") a[3],
            in("x4") a[4],
            in("x5") a[5],
            options(nostack),
        );
    }
    ret
}

fn sys1(nr: u64, a0: u64) -> u64 {
    sys(nr, [a0, 0, 0, 0, 0, 0])
}

fn yield_now() {
    sys1(syscall::YIELD, 0);
}

fn putc(slot: u64, b: u8) -> i64 {
    sys(syscall::DEBUG_PUTC, [slot, u64::from(b), 0, 0, 0, 0]) as i64
}

/// 콘솔 입력에서 바이트 하나를 읽는 함수입니다. (상태, 바이트, 유실 표시)를
/// 돌려줍니다. 커널에 콘솔 입력 경로가 있는 이미지에서만 부를 수 있습니다.
///
/// static을 쓰지 않으므로 격리된 태스크에서도 부를 수 있습니다.
///
/// # Arguments
/// `slot` - 읽기 권한이 있는 Console 슬롯
/// `flags` - [k0_abi::console]의 플래그와 타임아웃
#[cfg(feature = "console-input")]
#[inline(always)]
fn console_read(slot: u64, flags: u64) -> (i64, u8, bool) {
    let (status, byte, lost): (u64, u64, u64);
    // SAFETY: svc는 커널로의 동기 트랩이고 커널이 컨텍스트 전체를 복원함
    unsafe {
        asm!(
            "svc #0",
            in("x8") syscall::CONSOLE_READ,
            inlateout("x0") slot => status,
            inlateout("x1") flags => byte,
            lateout("x2") lost,
            options(nostack),
        );
    }
    (status as i64, byte as u8, lost & k0_abi::console::LOST != 0)
}

fn put_str(s: &str) {
    for b in s.bytes() {
        putc(slot::CONSOLE, b);
    }
}

fn put_hex(v: u64) {
    put_str("0x");
    for i in (0..16).rev() {
        let nib = ((v >> (i * 4)) & 0xF) as u8;
        let c = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
        putc(slot::CONSOLE, c);
    }
}

/// 검증 실패를 출력하고 태스크를 끝내는 함수입니다. 복귀하지 않습니다.
///
/// # Arguments
/// `step` - 실패한 검증 단계 이름
fn fail(step: &str) -> ! {
    put_str("root: FAIL ");
    put_str(step);
    put_str("\n");
    sys1(syscall::EXIT, 1);
    loop {
        core::hint::spin_loop();
    }
}

fn check(step: &str, ok: bool) {
    if !ok {
        fail(step)
    }
}

/// 루트 CNode의 다음 빈 슬롯 번호를 하나 내주는 함수입니다.
///
/// 커널은 빈 슬롯을 찾아 주지 않으므로 슬롯 관리는 루트 태스크의 몫입니다.
/// 재사용 검증용 구간을 침범하면 검증 구성이 잘못된 것이므로 실패 처리합니다.
fn alloc_slot() -> u64 {
    let s = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
    // 구간 바로 아래 슬롯 하나는 고갈 검증이 빈 목적지로 쓰므로 남겨 둠
    if s + 1 >= REUSE_BASE.load(Ordering::Relaxed) {
        fail("slot budget");
    }
    s
}

/// 검증 하나가 받은 슬롯을 전부 비우고 돌려주는 함수입니다.
///
/// `first`부터 지금까지 내준 슬롯의 케이퍼빌리티를 모두 지우고(이미 빈 슬롯은
/// 건너뜀) 할당 위치를 `first`로 되돌립니다. 그 슬롯의 오브젝트를 더 쓰지 않는
/// 검증의 끝에서만 부릅니다. 루트 CNode의 슬롯은 뒤에 오는 검증들이 거의 다
/// 쓰므로, 끝난 검증이 슬롯을 차지한 채로 두지 않습니다.
///
/// # Arguments
/// `first` - 그 검증이 시작할 때의 할당 위치
fn release_slots(first: u64) {
    for s in first..NEXT_SLOT.load(Ordering::Relaxed) {
        let r = cap_delete(s as i64);
        check("release slot", r == 0 || r == err::BAD_SLOT);
    }
    NEXT_SLOT.store(first, Ordering::Relaxed);
}

fn retype_at(ut: u64, kind: u64, dest: u64) -> i64 {
    sys(syscall::RETYPE, [ut, kind, dest, 0, 0, 0]) as i64
}

/// 새 슬롯을 잡아 재분류하고 그 슬롯 번호를 주는 함수입니다. 실패하면 음수
/// 에러(잡았던 슬롯은 버려짐)
fn retype(ut: u64, kind: u64) -> i64 {
    let s = alloc_slot();
    match retype_at(ut, kind, s) {
        0 => s as i64,
        e => e,
    }
}

fn map_into(cap: i64, va: u64, p: u64, aspace: i64) -> i64 {
    sys(syscall::MAP, [cap as u64, va, p, aspace as u64, 0, 0]) as i64
}

/// 루트 자신의 주소 공간에 매핑하는 함수입니다.
fn map(cap: i64, va: u64, p: u64) -> i64 {
    map_into(cap, va, p, slot::ADDR_SPACE as i64)
}

fn unmap(cap: i64) -> i64 {
    sys1(syscall::UNMAP, cap as u64) as i64
}

fn cap_copy(src: i64, cnode: i64, index: u64, mask: u64, badge: u64) -> i64 {
    sys(syscall::CAP_COPY, [src as u64, cnode as u64, index, mask, badge, 0]) as i64
}

/// 루트 CNode의 새 슬롯으로 복사하고 그 슬롯 번호를 주는 함수입니다.
fn copy_here(src: i64, mask: u64, badge: u64) -> i64 {
    let s = alloc_slot();
    match cap_copy(src, slot::CNODE as i64, s, mask, badge) {
        0 => s as i64,
        e => e,
    }
}

fn cap_move(src: i64, cnode: i64, index: u64) -> i64 {
    sys(syscall::CAP_MOVE, [src as u64, cnode as u64, index, 0, 0, 0]) as i64
}

fn cap_delete(cap: i64) -> i64 {
    sys1(syscall::CAP_DELETE, cap as u64) as i64
}

fn cap_revoke(cap: i64) -> i64 {
    sys1(syscall::CAP_REVOKE, cap as u64) as i64
}

fn tcb_configure(tcb: i64, entry: u64, stack: u64, aspace: i64, cnode: i64, arg: u64) -> i64 {
    sys(
        syscall::TCB_CONFIGURE,
        [tcb as u64, entry, stack, aspace as u64, cnode as u64, arg],
    ) as i64
}

/// 루트와 주소 공간, 케이퍼빌리티 공간을 함께 쓰는 태스크(스레드)를 구성하는
/// 함수입니다.
fn thread_configure(tcb: i64, entry: u64, stack: u64) -> i64 {
    tcb_configure(tcb, entry, stack, slot::ADDR_SPACE as i64, slot::CNODE as i64, 0)
}

fn tcb_resume(tcb: i64) -> i64 {
    sys1(syscall::TCB_RESUME, tcb as u64) as i64
}

/// IPC 반환값 묶음 구조체입니다. (x0 = 상태, x1 = 배지, x2-x5 = 수신 MR)
struct IpcRet {
    status: i64,
    badge: u64,
    mr: [u64; 4],
}

/// IPC 시스템 콜(x0-x5 사용)을 수행하는 함수입니다.
///
/// # Arguments
/// `nr` - 시스템 콜 번호(x8)
/// `slot` - 엔드포인트 슬롯(x0)
/// `flags` - 플래그(x1)
/// `mr` - 보낼 MR0-MR3(x2-x5), 수신 계열은 받은 값으로 덮임
#[inline(always)]
fn sys_ipc(nr: u64, slot: u64, flags: u64, mr: [u64; 4]) -> IpcRet {
    let status: u64;
    let badge: u64;
    let (mut m0, mut m1, mut m2, mut m3) = (mr[0], mr[1], mr[2], mr[3]);
    // SAFETY: svc는 커널로의 동기 트랩이고 커널이 컨텍스트 전체를 복원함
    unsafe {
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") slot => status,
            inlateout("x1") flags => badge,
            inlateout("x2") m0,
            inlateout("x3") m1,
            inlateout("x4") m2,
            inlateout("x5") m3,
            options(nostack),
        );
    }
    IpcRet {
        status: status as i64,
        badge,
        mr: [m0, m1, m2, m3],
    }
}

fn send(slot: i64, flags: u64, mr: [u64; 4]) -> IpcRet {
    sys_ipc(syscall::SEND, slot as u64, flags, mr)
}

fn recv(slot: i64, flags: u64) -> IpcRet {
    sys_ipc(syscall::RECV, slot as u64, flags, [0; 4])
}

fn call(slot: i64, flags: u64, mr: [u64; 4]) -> IpcRet {
    sys_ipc(syscall::CALL, slot as u64, flags, mr)
}

fn reply_recv(slot: i64, flags: u64, mr: [u64; 4]) -> IpcRet {
    sys_ipc(syscall::REPLY_RECV, slot as u64, flags, mr)
}

/// 서버가 응답에 적용하는 변환 함수입니다. 왕복 무결성 검증용
fn xform(m: [u64; 4]) -> [u64; 4] {
    [
        m[0].wrapping_add(1),
        m[1] ^ 0xF0F0_F0F0,
        m[2].wrapping_mul(3),
        m[3].rotate_left(16),
    ]
}

/// 목표까지 세고 스스로 종료하는 자식 태스크의 진입점입니다.
extern "C" fn child_exit_entry() -> ! {
    loop {
        let v = EXIT_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        if v == CHILD_EXIT_TARGET {
            sys1(syscall::EXIT, 7);
        }
    }
}

/// 수신한 메시지를 기록하고 변환 응답을 돌려주는 IPC 서버 태스크의
/// 진입점입니다. SRV_DIE가 서면 응답 없이 종료해 NO_REPLY 경로를,
/// SRV_HOLD가 서면 응답 자격을 쥔 채 일반 RECV로 넘어가 자격 덮어쓰기
/// 경로를 검증하게 합니다.
extern "C" fn server_entry() -> ! {
    let ep = EP_SLOT.load(Ordering::Relaxed) as i64;
    let mut r = recv(ep, 0);
    loop {
        if r.status != 0 {
            fail("server recv");
        }
        if SRV_DIE.load(Ordering::Relaxed) != 0 {
            sys1(syscall::EXIT, 9);
        }
        for (slot, v) in SRV_MR.iter().zip(r.mr) {
            slot.store(v, Ordering::Relaxed);
        }
        SRV_COUNT.fetch_add(1, Ordering::Relaxed);
        r = if SRV_HOLD.swap(0, Ordering::Relaxed) != 0 {
            recv(ep, 0)
        } else {
            reply_recv(ep, 0, xform(r.mr))
        };
    }
}

/// FIFO 검증용 첫 송신 뒤 서버에 CALL하고 그 결과를 기록하는 클라이언트
/// 태스크의 진입점입니다. 서버가 응답 자격을 덮으면 CALL은 NO_REPLY로
/// 깨어나야 합니다.
extern "C" fn client_entry() -> ! {
    let ep2 = EP2_SLOT.load(Ordering::Relaxed) as i64;
    if send(ep2, 0, CM_FIFO_A).status != 0 {
        fail("client send");
    }
    let ep = EP_SLOT.load(Ordering::Relaxed) as i64;
    let r = call(ep, 0, CM_CALL);
    CLIENT_STATUS.store(r.status as u64, Ordering::Relaxed);
    sys1(syscall::EXIT, 11);
    loop {
        core::hint::spin_loop();
    }
}

/// FIFO 검증용 두 번째 송신자 태스크의 진입점입니다. 전달이 끝나면
/// 종료합니다.
extern "C" fn client2_entry() -> ! {
    let ep2 = EP2_SLOT.load(Ordering::Relaxed) as i64;
    if send(ep2, 0, CM_FIFO_B).status != 0 {
        fail("client2 send");
    }
    sys1(syscall::EXIT, 12);
    loop {
        core::hint::spin_loop();
    }
}

fn get_tpidr() -> u64 {
    let v: u64;
    // SAFETY: TPIDR_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, tpidr_el0", out(reg) v, options(nomem, nostack)) };
    v
}

fn get_tpidrro() -> u64 {
    let v: u64;
    // SAFETY: TPIDRRO_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, tpidrro_el0", out(reg) v, options(nomem, nostack)) };
    v
}

fn set_tpidr(v: u64) {
    // SAFETY: TPIDR_EL0는 EL0가 자유롭게 쓰는 스레드 포인터 레지스터
    unsafe { asm!("msr tpidr_el0, {}", in(reg) v, options(nomem, nostack)) };
}

/// 스레드 포인터를 덮어쓴 뒤 null 역참조로 폴트를 내는 자식 태스크의
/// 진입점입니다. 커널이 이 태스크만 종료해야 하므로 폴트 뒤 코드는 절대
/// 실행되면 안 됩니다.
extern "C" fn child_fault_entry() -> ! {
    // 구성 시 커널이 0으로 강제한 값이어야 함 (루트의 값이 새면 안 됨)
    CHILD_SAW_TPIDR.store(get_tpidr(), Ordering::Relaxed);
    set_tpidr(CHILD_TPIDR);
    FAULT_STAGE.store(1, Ordering::Relaxed);
    yield_now();
    // SAFETY: 의도된 null 역참조, 데이터 어보트로 이 태스크가 종료돼야 함
    unsafe { asm!("str xzr, [{}]", in(reg) 0u64, options(nostack)) };
    FAULT_SURVIVED.fetch_add(1, Ordering::Relaxed);
    loop {
        yield_now();
    }
}

/// FP/SIMD 명령을 실행하는 자식 태스크의 진입점입니다. CPACR_EL1이 EL0의
/// 접근을 트랩하므로 이 태스크만 종료돼야 합니다.
extern "C" fn child_fp_entry() -> ! {
    FAULT_STAGE.store(2, Ordering::Relaxed);
    // SAFETY: fmov d0, x1의 raw 인코딩(softfloat 타겟이라 니모닉 거부), 트랩이
    //         목적이며 이 태스크가 종료돼야 함
    unsafe { asm!(".inst 0x9e670020", in("x1") 0u64, options(nostack)) };
    FAULT_SURVIVED.fetch_add(1, Ordering::Relaxed);
    loop {
        yield_now();
    }
}

/// 격리된 태스크의 진입점입니다. static을 쓰지 않습니다.
///
/// 자기 CNode에는 1번 슬롯의 송신 전용 엔드포인트 하나뿐이고, 자기 주소
/// 공간에는 이 이미지의 텍스트와 읽기 전용 데이터, 자기 스택, 읽기 전용
/// 공유 페이지 하나뿐입니다. 루트의 슬롯 번호를 그대로 써서 권한을 얻으려는
/// 시도가 전부 거부되는지를 비트맵으로 모아 CALL로 보고한 뒤, 응답으로
/// 받은 루트 전용 주소를 읽습니다. 그 주소는 이 주소 공간에 없으므로 여기서
/// 폴트로 종료돼야 하고, 그 뒤의 송신은 절대 실행되면 안 됩니다.
///
/// # Arguments
/// `ut` - 루트의 CNode에서 untyped가 든 슬롯 번호(여기서는 빈 슬롯)
extern "C" fn confined_entry(ut: u64) -> ! {
    let ep: i64 = 1;
    let mut refused: u64 = 0;
    let mut bit = 1;
    let mut note = |ok: bool| {
        if ok {
            refused |= bit;
        }
        bit <<= 1;
    };
    // 루트의 콘솔, untyped, 주소 공간, CNode 슬롯 번호는 이 CNode에서 비어 있음
    note(putc(slot::CONSOLE, b'!') == err::BAD_SLOT);
    note(retype_at(ut, obj::FRAME, 9) == err::NOT_UNTYPED);
    note(map_into(ep, ISO_SHARED_VA, perm::RW, slot::ADDR_SPACE as i64) == err::BAD_SLOT);
    note(tcb_configure(slot::TCB as i64, 0x40_0000, 0x50_0000, 2, 4, 0) == err::BAD_SLOT);
    note(cap_delete(slot::CNODE as i64) == err::BAD_SLOT);
    // 가진 것은 송신 전용 엔드포인트뿐: 수신과 복사 목적지로는 쓸 수 없음
    note(recv(ep, ipc::NONBLOCK).status == err::NO_RIGHTS);
    note(cap_copy(ep, ep, 3, rights::ALL, 0) == err::BAD_CAP);
    note(send(2, ipc::NONBLOCK, [0; 4]).status == err::BAD_SLOT);
    // 0번 슬롯은 어디서도 인자가 될 수 없음
    note(send(0, ipc::NONBLOCK, [0; 4]).status == err::BAD_SLOT);

    // SAFETY: 루트가 이 VA에 공유 프레임을 읽기 전용으로 매핑해 둠
    let shared = unsafe { (ISO_SHARED_VA as *const u64).read_volatile() };
    let r = call(ep, 0, [refused, shared, 0, 0]);

    // SAFETY: 의도된 위반. 응답으로 받은 루트 전용 주소는 이 주소 공간에
    //         매핑되지 않았으므로 데이터 어보트로 이 태스크가 종료돼야 함
    let leaked = unsafe { (r.mr[0] as *const u64).read_volatile() };
    send(ep, 0, [leaked, 0xBAD, 0, 0]);
    loop {
        yield_now();
    }
}

/// 격리 태스크가 진행 상황을 공유 페이지에 적는 함수입니다.
///
/// 케이퍼빌리티도 static도 쓰지 않으므로, 케이퍼빌리티 공간을 잃은 태스크라도
/// 실행되는 한 이 값은 계속 바뀝니다.
#[inline(always)]
fn pulse(v: u64) {
    // SAFETY: 루트가 격리 주소 공간의 이 VA에 공유 프레임을 RW로 매핑해 둠
    unsafe { (ISO_PULSE_VA as *mut u64).write_volatile(v) };
}

/// 받는 이 없는 엔드포인트에 송신해 대기 큐에서 블록되는 격리 태스크의
/// 진입점입니다. 블록된 채로 삭제되는 경로를 검증합니다. 메시지와 pulse에
/// 자기 번호를 실어 도착 여부와 큐에서의 순서를 확인할 수 있게 합니다.
///
/// # Arguments
/// `arg` - 이 태스크의 번호
extern "C" fn blocker_entry(arg: u64) -> ! {
    pulse(arg);
    send(1, 0, [0xB10C, arg, 0, 0]);
    loop {
        yield_now();
    }
}

/// 1번 엔드포인트에서 블록되는 연산 하나를 한 뒤 깨어난 상태를 2번
/// 엔드포인트로 보고하는 격리 태스크의 진입점입니다.
///
/// # Arguments
/// `mode` - 0 = RECV, 1 = SEND, 2 = CALL
extern "C" fn reporter_entry(mode: u64) -> ! {
    pulse(PULSE_REPORTER + mode);
    let status = match mode {
        0 => recv(1, 0).status,
        1 => send(1, 0, [0x51, 0, 0, 0]).status,
        _ => call(1, 0, [0xCA, 0, 0, 0]).status,
    };
    send(2, 0, [status as u64, REPORT_TAG, mode, 0]);
    loop {
        yield_now();
    }
}

/// 1번 엔드포인트에서 기한을 건 연산 하나를 하고 그 반환 코드를 2번
/// 엔드포인트로 보고하는 격리 태스크의 진입점입니다.
///
/// # Arguments
/// `arg` - 하위 8비트는 연산(0 = RECV, 1 = SEND, 2 = CALL), 그 위는 타임아웃
/// 틱 수
extern "C" fn timed_entry(arg: u64) -> ! {
    let (mode, ticks) = (arg & 0xFF, (arg >> 8) as u32);
    pulse(PULSE_TIMED + mode);
    let flags = ipc::timeout(ticks);
    let status = match mode {
        0 => recv(1, flags).status,
        1 => send(1, flags, [0x71, 0, 0, 0]).status,
        _ => call(1, flags, [0x7C, 0, 0, 0]).status,
    };
    send(2, 0, [status as u64, TIMED_TAG, mode, 0]);
    loop {
        yield_now();
    }
}

/// 첫 CALL을 받은 뒤 2번 엔드포인트의 신호를 기다렸다가 응답하는 격리 서버의
/// 진입점입니다.
///
/// 신호가 오기 전에 호출자의 기한이 지나게 해서, 뒤늦은 응답이 버려지고
/// 서버는 그 뒤의 호출에 정상으로 응답하는지 검증합니다.
extern "C" fn slow_server_entry(_arg: u64) -> ! {
    let mut r = recv(1, 0);
    pulse(PULSE_SLOW + r.mr[0]);
    recv(2, 0);
    loop {
        r = reply_recv(1, 0, [r.mr[0] ^ SLOW_XOR, 0, 0, 0]);
    }
}

/// CALL 하나를 받고 응답하지 않은 채 머무는 격리 서버의 진입점입니다. 응답
/// 자격을 쥔 태스크가 삭제되는 경로를 검증합니다.
extern "C" fn holder_entry(_arg: u64) -> ! {
    let r = recv(1, 0);
    pulse(PULSE_HOLDER + r.mr[0]);
    loop {
        yield_now();
    }
}

/// 증가하는 번호를 pulse에 적고 송신하기를 반복하는 격리 태스크의
/// 진입점입니다. 케이퍼빌리티 공간이나 주소 공간이 회수되면 pulse가 멈춰야
/// 합니다(송신이 거부될 뿐 계속 실행된다면 pulse는 계속 바뀜).
extern "C" fn beat_entry(_arg: u64) -> ! {
    let mut n = 0u64;
    loop {
        n = n.wrapping_add(1);
        pulse(n);
        send(1, 0, [n, BEAT_TAG, 0, 0]);
    }
}

/// 살아 있음을 알린 뒤 2번 슬롯의 케이퍼빌리티를 `op`(CAP_DELETE 또는
/// CAP_REVOKE)로 없애는 격리 태스크의 진입점입니다.
///
/// 2번 슬롯에는 루트가 이 태스크 자신을 떠받치는 것(자기 TCB의 원본, 또는
/// 자기 TCB / CNode / 주소 공간을 잘라낸 untyped)을 넣어 둡니다. 그
/// 시스템 콜은 호출한 태스크 자신을 종료시키므로 복귀하면 안 되고, 두 번째
/// 송신은 절대 실행되면 안 됩니다.
///
/// # Arguments
/// `op` - 2번 슬롯에 적용할 시스템 콜 번호
extern "C" fn self_destruct_entry(op: u64) -> ! {
    send(1, 0, [1, 0x5E1F, 0, 0]);
    sys1(op, 2);
    send(1, 0, [2, 0xBAD, 0, 0]);
    loop {
        yield_now();
    }
}

/// 하드웨어가 막아야 하는 일을 하나 시도하는 격리 태스크의 진입점입니다.
///
/// 시도 전에 pulse로 도착을 알립니다. 시도가 막히면 이 태스크는 그 자리에서
/// 종료되므로, 그 뒤의 pulse 기록과 송신은 절대 실행되면 안 됩니다.
///
/// # Arguments
/// `kind` - 0 = 읽기 전용 페이지에 쓰기, 1 = 자기 스택(쓰기 가능 페이지)으로
/// 분기, 2 = 커널 절반의 주소 읽기, 3 = 미지의 시스템 콜
extern "C" fn violator_entry(kind: u64) -> ! {
    pulse(PULSE_VIOLATOR + kind);
    match kind {
        // SAFETY: 의도된 위반, 권한 폴트로 이 태스크가 종료돼야 함
        0 => unsafe { (ISO_SHARED_VA as *mut u64).write_volatile(0xBAD) },
        1 => {
            let cell = 0u64;
            let target = (&raw const cell as u64) & !3;
            // SAFETY: 의도된 위반, 쓰기 가능한 페이지는 실행 불가라 명령 어보트로
            //         이 태스크가 종료돼야 함
            unsafe { asm!("br {}", in(reg) target, options(noreturn)) }
        }
        // SAFETY: 의도된 위반, 커널 절반은 EL0에서 접근 불가라 데이터 어보트로
        //         이 태스크가 종료돼야 함
        2 => pulse(unsafe { (KERNEL_PROBE as *const u64).read_volatile() }),
        _ => {
            sys(0xFFFF, [0; 6]);
        }
    }
    pulse(PULSE_SURVIVED);
    send(1, 0, [0xBAD, kind, 0, 0]);
    loop {
        yield_now();
    }
}

/// bootinfo에서 읽은 루트 태스크의 자원 배치를 담는 구조체입니다.
struct Boot {
    frame_size: u64,
    /// 가장 큰 untyped (수명이 긴 오브젝트용)
    ut_main: u64,
    /// 격리 검증이 오브젝트를 잘라 쓰는 작은 untyped와 그 프레임 수
    ut_small: u64,
    ut_small_frames: u64,
    /// 고갈로 재사용을 검증할 untyped와 그 프레임 수. 재사용 슬롯 구간에 다
    /// 담기는 것 중 가장 큰 것이고, 그런 untyped가 없는 배치에서는 0
    ut_reuse: u64,
    ut_reuse_frames: u64,
    /// 이미지 Frame 케이퍼빌리티가 든 슬롯 구간 [first, end)
    image_first: u64,
    image_end: u64,
}

/// bootinfo의 디스크립터 하나를 읽는 함수입니다.
fn desc(i: u64) -> &'static bootinfo::CapDesc {
    let hdr = bootinfo::VA as *const bootinfo::Header;
    // SAFETY: 커널이 헤더 뒤에 cap_count개의 디스크립터를 기록했고 호출자는
    //         그 범위 안의 번호만 넘김
    unsafe { &*(hdr.add(1) as *const bootinfo::CapDesc).add(i as usize) }
}

/// untyped 디스크립터에 온전히 들어가는 프레임 수를 주는 함수입니다.
fn frames_in(d: &bootinfo::CapDesc, frame_size: u64) -> u64 {
    let first = d.base.div_ceil(frame_size) * frame_size;
    let end = d.base + d.size;
    if first >= end { 0 } else { (end - first) / frame_size }
}

/// 커널이 진입 레지스터로 넘긴 부트 시드(워드 넷)와 그 재료 정보
///
/// 시드는 서비스(키 볼트)에 넘기기 위한 것이고 루트 태스크는 출력하지
/// 않습니다. 넘긴 뒤에는 [take_boot_seed]가 이 자리를 지웁니다.
static BOOT_SEED: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static SEED_INFO: AtomicU64 = AtomicU64::new(0);

/// 부트 시드와 그 재료 정보를 꺼내고 보관하던 자리를 지우는 함수입니다. 두
/// 번째부터는 시드도 정보도 0만 나옵니다. 정보까지 지우는 까닭은, 이미 꺼낸
/// 시드(전부 0)가 "재료가 충분함"이라는 정보와 함께 다시 넘어가 누구나 아는
/// 키가 만들어지는 일을 막기 위해서입니다.
#[allow(dead_code)]
fn take_boot_seed() -> ([u8; 32], u64) {
    let mut seed = [0u8; 32];
    for (chunk, word) in seed.chunks_exact_mut(8).zip(&BOOT_SEED) {
        chunk.copy_from_slice(&word.swap(0, Ordering::Relaxed).to_le_bytes());
    }
    (seed, SEED_INFO.swap(0, Ordering::Relaxed))
}

#[unsafe(no_mangle)]
extern "C" fn _start(s0: u64, s1: u64, s2: u64, s3: u64, seed_info: u64) -> ! {
    // 시드는 레지스터로만 오므로 다른 일을 하기 전에 옮겨 둠
    for (slot, word) in BOOT_SEED.iter().zip([s0, s1, s2, s3]) {
        slot.store(word, Ordering::Relaxed);
    }
    SEED_INFO.store(seed_info, Ordering::Relaxed);

    // 출력에도 Console 케이퍼빌리티가 필요함. 고정 슬롯이라 바로 쓸 수 있지만
    // 레이아웃 버전이 다르면 그 가정도 믿을 수 없으므로 출력 없이 끝냄
    let hdr = bootinfo::VA as *const bootinfo::Header;
    // SAFETY: 커널이 이 VA에 bootinfo 페이지를 RO로 매핑하고 내용을 채웠음
    let (version, frame_size, cap_count, cnode_slots) = unsafe {
        (
            (*hdr).version,
            (*hdr).frame_size,
            (*hdr).cap_count,
            (*hdr).cnode_slots,
        )
    };
    if version != bootinfo::VERSION {
        sys1(syscall::EXIT, 1);
    }
    put_str("root: hello from EL0\n");
    // 시드 자체는 찍지 않고 재료만 알림
    put_str("root: boot seed from rng-seed bytes ");
    put_hex(seed_info & bootinfo::SEED_RNG_SEED_BYTES);
    put_str(if seed_info & bootinfo::SEED_RNDR != 0 { " and rndr\n" } else { " without rndr\n" });
    // 시드를 쓰는 것은 서비스의 볼트뿐임(자가 검증의 볼트는 공개 시험 시드를
    // 씀). 그 서비스가 없는 이미지에서는 보관하지 않고 바로 지움
    #[cfg(not(all(feature = "vault", feature = "terminal")))]
    let _ = take_boot_seed();
    check(
        "bootinfo frame size",
        frame_size > 0 && frame_size.is_power_of_two(),
    );
    check(
        "bootinfo slots",
        cap_count > slot::CNODE && cnode_slots >= cap_count + 3 * REUSE_SLOTS,
    );
    NEXT_SLOT.store(cap_count, Ordering::Relaxed);
    REUSE_BASE.store(cnode_slots - REUSE_SLOTS, Ordering::Relaxed);

    // 고정 슬롯의 종류 확인, untyped와 이미지 프레임 탐색
    use bootinfo::cap_kind as kind;
    check("slot 0 null", desc(0).kind == kind::EMPTY);
    check("slot tcb", desc(slot::TCB).kind == kind::TCB);
    check("slot aspace", desc(slot::ADDR_SPACE).kind == kind::ADDR_SPACE);
    check("slot console", desc(slot::CONSOLE).kind == kind::CONSOLE);
    check("slot cnode", desc(slot::CNODE).kind == kind::CNODE);
    let mut b = Boot {
        frame_size,
        ut_main: 0,
        ut_small: 0,
        ut_small_frames: 0,
        ut_reuse: 0,
        ut_reuse_frames: 0,
        image_first: 0,
        image_end: 0,
    };
    let mut main_frames = 0;
    for i in 0..cap_count {
        let d = desc(i);
        if d.kind == kind::UNTYPED {
            let n = frames_in(d, frame_size);
            if n > main_frames {
                main_frames = n;
                b.ut_main = i;
            }
            if n >= 112 && (b.ut_small == 0 || n < b.ut_small_frames) {
                b.ut_small = i;
                b.ut_small_frames = n;
            }
            if (2..=REUSE_SLOTS).contains(&n) && n > b.ut_reuse_frames {
                b.ut_reuse = i;
                b.ut_reuse_frames = n;
            }
        }
        if d.kind == kind::FRAME {
            if b.image_first == 0 {
                b.image_first = i;
            }
            b.image_end = i + 1;
        }
    }
    // 재사용 검증은 untyped를 통째로 회수하므로 수명이 긴 오브젝트가 사는
    // untyped와 같으면 안 됨
    if b.ut_reuse == b.ut_main {
        b.ut_reuse = 0;
    }
    check("untyped search", b.ut_main != 0 && main_frames >= 64);
    check("small untyped search", b.ut_small != 0 && b.ut_small != b.ut_main);
    check("image frames", b.image_first != 0);
    put_str("root: untyped slot ");
    put_hex(b.ut_main);
    put_str("\n");

    test_console();
    test_retype_map(&b);
    test_caps(&b);
    test_reuse(&b);
    test_sched_ipc_fault(&b);
    test_isolation(&b);
    #[cfg(feature = "vault")]
    oracle::test_oracle(&b, seed_info);
    test_reserve(&b);
    // 입력을 처음 읽으면 커널이 주기적인 틱 줄을 멈추므로, 틱 줄을 세는 검증이
    // 모두 끝난 뒤인 맨 마지막에 돌림
    #[cfg(feature = "console-input")]
    test_console_input(&b);

    put_str("root: sched tests pass\n");
    // 자가 검증을 모두 통과한 뒤에만 서비스를 시작함. 시작했으면 돌아오지 않음
    #[cfg(feature = "terminal")]
    service::start(&b);
    loop {
        yield_now();
    }
}

/// 앞의 검증들이 쓰지 않는 untyped 가운데 가장 큰 것을 고르는 함수입니다. 그런
/// 것이 없으면 0입니다. 볼트 검증, 입력 검증, 서비스가 차례로 씁니다(앞의
/// 검증들은 주 untyped를 거의 다 쓰기도 함).
#[allow(dead_code)]
fn spare_untyped(b: &Boot) -> u64 {
    // SAFETY: 커널이 이 VA에 bootinfo 페이지를 RO로 매핑하고 내용을 채웠음
    let cap_count = unsafe { (*(bootinfo::VA as *const bootinfo::Header)).cap_count };
    let (mut best, mut best_frames) = (0, 0);
    for i in 0..cap_count {
        let d = desc(i);
        if d.kind != bootinfo::cap_kind::UNTYPED || i == b.ut_main || i == b.ut_small || i == b.ut_reuse {
            continue;
        }
        let n = frames_in(d, b.frame_size);
        if n >= 512 && n > best_frames {
            best = i;
            best_frames = n;
        }
    }
    best
}

/// 자가 검증의 끝과 그 뒤의 서비스가 오브젝트를 만들 untyped를 고르는
/// 함수입니다. 앞의 검증들이 쓰지 않는 큰 untyped가 있으면 그것을, 없으면 주
/// untyped를 씁니다. 주 untyped에는 커널 예약 검증이 이 단계의 몫을 남겨
/// 둡니다([test_reserve]).
#[allow(dead_code)]
fn late_untyped(b: &Boot) -> u64 {
    match spare_untyped(b) {
        0 => b.ut_main,
        spare => spare,
    }
}

/// 콘솔 입력을 기다리다 그 결과를 [INPUT_SEEN]에 적는 자식 스레드의
/// 진입점입니다.
///
/// # Arguments
/// `slot` - 읽을 Console 슬롯
#[cfg(feature = "console-input")]
extern "C" fn input_waiter_entry(slot: u64) -> ! {
    INPUT_SEEN.store(INPUT_WAITING, Ordering::Relaxed);
    let (status, _, _) = console_read(slot, 0);
    INPUT_SEEN.store(status as u64, Ordering::Relaxed);
    sys1(syscall::EXIT, 0);
    loop {
        core::hint::spin_loop();
    }
}

/// [input_waiter_entry]가 본 상태. 기다리는 동안은 [INPUT_WAITING]
#[cfg(feature = "console-input")]
static INPUT_SEEN: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "console-input")]
const INPUT_WAITING: u64 = 0x1D1E;

/// 콘솔 입력 시스템 콜의 권한과 대기를 검증하는 함수입니다.
///
/// 누가 읽을 수 있는지(읽기 권한이 있는 Console 케이퍼빌리티만), 대기자 자리가
/// 하나뿐인 것, 기한, 대기자의 삭제와 권한 회수를 봅니다. 직렬 선은 커널
/// 밖의 것이라 이 검증 도중에 바이트가 들어올 수 있습니다. 그런 경우는
/// 실패로 치지 않고 그 뒤의 검증을 건너뛴 뒤 한 줄로 알립니다(바이트 하나로
/// 자가 검증이 실패해 서비스가 뜨지 않으면 안 됨).
#[cfg(feature = "console-input")]
fn test_console_input(b: &Boot) {
    use k0_abi::console::{FLUSH, NONBLOCK};
    let (ut, fs) = (late_untyped(b), b.frame_size);
    let con = slot::CONSOLE as i64;

    // 출력만 남긴 복사본: 읽지 못하고 백스페이스도 `?`로 바뀜
    let wo = copy_here(con, rights::W, 0);
    check("console copy w", wo > 0);
    check("read needs the input right", console_read(wo as u64, NONBLOCK).0 == err::NO_RIGHTS);
    put_str("root: backspace without the input right [");
    check("putc bs w-only", putc(wo as u64, 0x08) == 0);
    put_str("]\n");
    // 입력만 남긴 복사본: 출력하지 못함. 권한은 복사로 되살릴 수 없음
    let ro = copy_here(con, rights::R, 0);
    check("console copy r", ro > 0);
    check("write needs the output right", putc(ro as u64, b'x') == err::NO_RIGHTS);
    check("rights do not come back", copy_here(wo, rights::R, 0) == err::NO_RIGHTS as i64);
    check("rights do not come back", copy_here(ro, rights::W, 0) == err::NO_RIGHTS as i64);
    // 거부 경로는 받아 둔 입력을 건드리지 않음
    check("read bad cap", console_read(slot::ADDR_SPACE, NONBLOCK).0 == err::BAD_CAP);
    check("read bad slot", console_read(9999, NONBLOCK).0 == err::BAD_SLOT);
    check("read null slot", console_read(0, NONBLOCK).0 == err::BAD_SLOT);
    check("read reserved flags", console_read(slot::CONSOLE, 4 | NONBLOCK).0 == err::BAD_FLAGS);

    let first = console_read(slot::CONSOLE, NONBLOCK | FLUSH).0;
    if first == err::NO_DEVICE {
        // 수신 드라이버가 없는 UART이거나 인터럽트를 쓸 수 없는 기계
        check("no input path", console_read(ro as u64, 0).0 == err::NO_DEVICE);
        put_str("root: console input tests pass (no input path)\n");
        return;
    }
    let busy_line = || put_str("root: console input tests cut short (the line is busy)\n");
    if first != err::WOULD_BLOCK {
        check("first read", first == 0);
        return busy_line();
    }
    // 기한이 있는 읽기는 입력이 없으면 기한에 끝남
    match console_read(ro as u64, FLUSH | ipc::timeout(1)).0 {
        0 => return busy_line(),
        e => check("read times out", e == err::TIMEOUT),
    }

    // 대기자 자리는 하나: 자식이 기다리는 동안 다른 읽기는 BUSY
    let stack = retype(ut, obj::FRAME);
    check("input waiter stack", stack > 0);
    check("input waiter stack map", map(stack, TEST_VA + 0x70 * 0x4000, perm::RW) == 0);
    let sp = TEST_VA + 0x70 * 0x4000 + fs;
    let waiter = |slot: i64| {
        INPUT_SEEN.store(0, Ordering::Relaxed);
        let tcb = retype(ut, obj::TCB);
        check(
            "input waiter",
            tcb > 0
                && tcb_configure(tcb, input_waiter_entry as usize as u64, sp, slot::ADDR_SPACE as i64, slot::CNODE as i64, slot as u64)
                    == 0
                && tcb_resume(tcb) == 0,
        );
        for _ in 0..8 {
            yield_now();
        }
        tcb
    };
    // 여기서부터는 직렬 선에서 바이트 하나가 들어오면 결과가 달라짐(기다리던
    // 자식이 그 바이트를 받아 자리가 비거나, 링에 바이트가 남음). 그런 경우는
    // 실패가 아니므로 만든 것을 치우고 건너뜀
    let still_waiting = || INPUT_SEEN.load(Ordering::Relaxed) == INPUT_WAITING;
    let empty = || console_read(slot::CONSOLE, NONBLOCK | FLUSH).0 == err::WOULD_BLOCK;
    let child = waiter(ro);
    let busy = still_waiting()
        && console_read(slot::CONSOLE, NONBLOCK).0 == err::BUSY
        && console_read(slot::CONSOLE, ipc::timeout(1)).0 == err::BUSY;
    // 기다리던 태스크를 삭제하면 자리가 빔
    check("delete the waiter", cap_delete(child) == 0);
    if !busy || !empty() {
        check("input stack unmap", unmap(stack) == 0);
        return busy_line();
    }

    // 기다리는 동안 그 케이퍼빌리티를 잃은 태스크는 곧바로 REVOKED로 깨어남.
    // 삭제와 이동 둘 다 그러함
    for by_move in [false, true] {
        let lease = copy_here(con, rights::R, 0);
        check("console lease", lease > 0);
        let child = waiter(lease);
        let waiting = still_waiting();
        let taken = if by_move {
            let away = alloc_slot();
            let moved = cap_move(lease, slot::CNODE as i64, away) == 0;
            check("console lease moved", moved);
            away as i64
        } else {
            check("take the lease back", cap_delete(lease) == 0);
            0
        };
        for _ in 0..8 {
            yield_now();
        }
        let seen = INPUT_SEEN.load(Ordering::Relaxed);
        check("input waiter delete", cap_delete(child) == 0);
        if taken != 0 {
            check("moved lease delete", cap_delete(taken) == 0);
        }
        if !waiting || seen != err::REVOKED as u64 || !empty() {
            // 자식이 바이트를 받고 끝났으면 0이 보임. 그 밖의 값은 진짜 실패임
            check("waiter saw the revocation or a byte", !waiting || seen == 0 || seen == err::REVOKED as u64);
            check("input stack unmap", unmap(stack) == 0);
            return busy_line();
        }
    }
    check("input stack unmap", unmap(stack) == 0);
    put_str("root: console input tests pass\n");
}

/// 콘솔 거부 경로와 제어 문자 필터를 검증하는 함수입니다.
fn test_console() {
    let con = slot::CONSOLE;
    // Console이 아닌 케이퍼빌리티, 범위 밖 슬롯, null 슬롯. 제어 문자는 커널이
    // `?`로 바꿔야 함 (로그에 "[?]"로 나타남)
    check("putc bad cap", putc(slot::ADDR_SPACE, b'x') == err::BAD_CAP);
    check("putc bad slot", putc(9999, b'x') == err::BAD_SLOT);
    check("putc null slot", putc(0, b'x') == err::BAD_SLOT);
    put_str("root: control byte filter [");
    check("putc esc", putc(con, 0x1b) == 0);
    put_str("]\n");
}

/// 재분류와 매핑의 정상 경로와 거부 경로를 검증하는 함수입니다.
fn test_retype_map(b: &Boot) {
    let (ut, fs) = (b.ut_main, b.frame_size);
    let con = slot::CONSOLE as i64;

    // 거부 경로: null 슬롯, untyped가 아닌 슬롯, 미지의 타입
    let spare = alloc_slot();
    check("retype null slot", retype_at(0, obj::FRAME, spare) == err::BAD_SLOT);
    check(
        "retype not untyped",
        retype_at(slot::CONSOLE, obj::FRAME, spare) == err::NOT_UNTYPED,
    );
    check("retype bad type", retype_at(ut, 99, spare) == err::BAD_TYPE);
    // 목적지 거부 경로: null 슬롯, 범위 밖, 이미 찬 슬롯
    check("retype into null", retype_at(ut, obj::FRAME, 0) == err::BAD_SLOT);
    check("retype out of range", retype_at(ut, obj::FRAME, 1 << 40) == err::BAD_SLOT);
    check(
        "retype into occupied",
        retype_at(ut, obj::FRAME, slot::CONSOLE) == err::OCCUPIED,
    );

    // 중간 테이블이 없는 동안 프레임 매핑은 거부돼야 함. 거부된 재분류가 쓰지
    // 않은 슬롯은 그대로 비어 있어 여기서 다시 쓸 수 있음
    check("retype frame", retype_at(ut, obj::FRAME, spare) == 0);
    let f = spare as i64;
    check("map before pt", map(f, TEST_VA, perm::RW) == err::MISSING_TABLE);

    // 매핑 대상은 제시한 AddrSpace 케이퍼빌리티로만 정해짐
    check("map bad aspace", map_into(f, TEST_VA, perm::RW, con) == err::BAD_CAP);
    check(
        "map bad aspace slot",
        map_into(f, TEST_VA, perm::RW, 9999) == err::BAD_SLOT,
    );

    // 페이지 테이블 재분류와 설치
    let pt = retype(ut, obj::PAGE_TABLE);
    check("retype pt", pt > 0);
    check("map pt", map(pt, TEST_VA, 0) == 0);
    check("map pt twice", map(pt, TEST_VA, 0) == err::ALREADY_MAPPED);

    // 프레임 매핑 거부 경로: null VA, 미지의 권한, 실행 권한
    check("map null va", map(f, 0, perm::RW) == err::BAD_VA);
    check("map bad perm", map(f, TEST_VA, 7) == err::BAD_PERM);
    check("map frame exec", map(f, TEST_VA, perm::RX) == err::NO_RIGHTS);

    // 정상 매핑 후 실제 읽기/쓰기
    check("map frame", map(f, TEST_VA, perm::RW) == 0);
    let head = TEST_VA as *mut u64;
    let tail = (TEST_VA + fs - 8) as *mut u64;
    // SAFETY: 방금 RW로 매핑된 전용 프레임
    unsafe {
        head.write_volatile(0xA5A5_5A5A_DEAD_BEEF);
        tail.write_volatile(0x0123_4567_89AB_CDEF);
        check("frame rw head", head.read_volatile() == 0xA5A5_5A5A_DEAD_BEEF);
        check("frame rw tail", tail.read_volatile() == 0x0123_4567_89AB_CDEF);
    }

    // 이중 매핑과 점유된 자리 매핑 거부
    check(
        "map frame twice",
        map(f, TEST_VA + fs, perm::RW) == err::ALREADY_MAPPED,
    );
    let f2 = retype(ut, obj::FRAME);
    check("retype frame 2", f2 > 0 && f2 != f);
    check("map overlap", map(f2, TEST_VA, perm::RW) == err::OVERLAP);
    check("map frame 2", map(f2, TEST_VA + fs, perm::RW) == 0);
    // SAFETY: 위와 동일
    unsafe {
        let q = (TEST_VA + fs) as *mut u64;
        q.write_volatile(0x1111_2222_3333_4444);
        check("frame 2 rw", q.read_volatile() == 0x1111_2222_3333_4444);
        check(
            "frame isolation",
            head.read_volatile() == 0xA5A5_5A5A_DEAD_BEEF,
        );
    }

    // RO 매핑과 소거(zeroize) 확인
    let f3 = retype(ut, obj::FRAME);
    check("retype frame 3", f3 > 0);
    check("map frame ro", map(f3, TEST_VA + 2 * fs, perm::RO) == 0);
    // SAFETY: 방금 RO로 매핑된 전용 프레임, 재분류가 소거를 보장함
    unsafe {
        let r = (TEST_VA + 2 * fs) as *const u64;
        check("frame ro zeroed", r.read_volatile() == 0);
    }

    put_str("root: retype/map tests pass\n");
}

/// 케이퍼빌리티 복사·이동·삭제·회수와 권한, 매핑 해제를 검증하는 함수입니다.
///
/// 테스트 VA 구간의 +24 그래뉼부터를 씁니다.
fn test_caps(b: &Boot) {
    let (ut, fs) = (b.ut_main, b.frame_size);
    let me = slot::CNODE as i64;
    let va = |n: u64| TEST_VA + (24 + n) * fs;

    // 원본 프레임에 값을 쓰고, 읽기 전용 복사본을 다른 VA에 매핑하면 같은
    // 프레임이 보여야 함
    let f = retype(ut, obj::FRAME);
    check("cap frame", f > 0 && map(f, va(0), perm::RW) == 0);
    // SAFETY: 방금 RW로 매핑된 전용 프레임
    unsafe { (va(0) as *mut u64).write_volatile(0xCAFE_F00D_0000_0001) };
    let ro = copy_here(f, rights::R, 0);
    check("copy frame ro", ro > 0 && ro != f);
    check("ro copy map rw", map(ro, va(1), perm::RW) == err::NO_RIGHTS);
    check("ro copy map rx", map(ro, va(1), perm::RX) == err::NO_RIGHTS);
    check("ro copy map ro", map(ro, va(1), perm::RO) == 0);
    // SAFETY: 방금 RO로 매핑된 같은 프레임
    unsafe {
        check(
            "copy sees original",
            (va(1) as *const u64).read_volatile() == 0xCAFE_F00D_0000_0001,
        );
    }
    // 복사본의 복사본으로도 권한은 되살아나지 않음
    let ro2 = copy_here(ro, rights::ALL, 0);
    check("copy of ro copy", ro2 > 0);
    check("rights never grow", map(ro2, va(2), perm::RW) == err::NO_RIGHTS);

    // 복사 거부 경로
    let spare = alloc_slot();
    check("copy untyped", cap_copy(ut as i64, me, spare, rights::ALL, 0) == err::BAD_CAP);
    let pt = retype(ut, obj::PAGE_TABLE);
    check("copy pt", pt > 0 && cap_copy(pt, me, spare, rights::ALL, 0) == err::BAD_CAP);
    check("copy empty src", cap_copy(spare as i64, me, spare, rights::ALL, 0) == err::BAD_SLOT);
    check("copy into null", cap_copy(f, me, 0, rights::ALL, 0) == err::BAD_SLOT);
    check("copy out of range", cap_copy(f, me, 1 << 32, rights::ALL, 0) == err::BAD_SLOT);
    check("copy into occupied", cap_copy(f, me, f as u64, rights::ALL, 0) == err::OCCUPIED);
    check("copy dest not cnode", cap_copy(f, f, spare, rights::ALL, 0) == err::BAD_CAP);
    check("copy bad mask", cap_copy(f, me, spare, 0x10, 0) == err::BAD_PERM);
    check("copy no rights left", cap_copy(ro, me, spare, rights::W, 0) == err::NO_RIGHTS);
    check("copy badge on frame", cap_copy(f, me, spare, rights::ALL, 5) == err::BAD_BADGE);
    check(
        "copy badge on console",
        cap_copy(slot::CONSOLE as i64, me, spare, rights::ALL, 5) == err::BAD_BADGE,
    );

    // 실행 권한은 부트 텍스트 프레임에만 있고 그 프레임에는 쓰기 권한이 없음
    let mut text = 0;
    for i in b.image_first..b.image_end {
        let d = desc(i);
        check("image frame desc", d.kind == bootinfo::cap_kind::FRAME && d.size == fs);
        check("image frame w^x", d.rights & rights::W == 0 || d.rights & rights::X == 0);
        if d.rights & rights::X != 0 && text == 0 {
            text = i;
        }
    }
    check("text frame", text != 0);
    let tx = copy_here(text as i64, rights::ALL, 0);
    check("copy text frame", tx > 0);
    check("text map rw", map(tx, va(2), perm::RW) == err::NO_RIGHTS);
    check("text map rx", map(tx, va(2), perm::RX) == 0);
    // SAFETY: 방금 RX로 매핑된 텍스트 프레임, 원래 매핑과 같은 내용이어야 함
    unsafe {
        check(
            "text alias",
            (va(2) as *const u32).read_volatile() == (desc(text).base as *const u32).read_volatile(),
        );
    }

    // 매핑 해제: 자리가 비어 다른 프레임을 매핑할 수 있고, 해제한
    // 케이퍼빌리티는 다시 매핑할 수 있음
    check("unmap copy", unmap(ro) == 0);
    check("unmap twice", unmap(ro) == err::BAD_STATE);
    check("unmap bad cap", unmap(slot::CONSOLE as i64) == err::BAD_CAP);
    let g = retype(ut, obj::FRAME);
    check("map into freed va", g > 0 && map(g, va(1), perm::RW) == 0);
    // SAFETY: 방금 RW로 매핑된 새 프레임, 재분류가 소거를 보장함
    unsafe { check("freed va shows new frame", (va(1) as *const u64).read_volatile() == 0) };
    check("remap after unmap", map(ro, va(3), perm::RO) == 0);

    // 이동: 옛 슬롯은 비고 새 슬롯이 매핑 상태까지 이어받음
    let moved = alloc_slot();
    check("move", cap_move(ro, me, moved) == 0);
    check("moved src empty", unmap(ro) == err::BAD_SLOT);
    check("move into occupied", cap_move(moved as i64, me, f as u64) == err::OCCUPIED);
    check("move into null", cap_move(moved as i64, me, 0) == err::BAD_SLOT);
    check("moved keeps mapping", unmap(moved as i64) == 0);

    // 삭제: 복사본을 지우면 그 매핑과 그 복사본에서 파생된 것만 사라짐
    check("remap moved", map(moved as i64, va(3), perm::RO) == 0);
    check("delete copy", cap_delete(moved as i64) == 0);
    check("deleted slot empty", cap_delete(moved as i64) == err::BAD_SLOT);
    check("derived copy gone", map(ro2, va(4), perm::RO) == err::BAD_SLOT);
    let h = retype(ut, obj::FRAME);
    check("deleted mapping gone", h > 0 && map(h, va(3), perm::RW) == 0);
    // SAFETY: 원본의 매핑은 그대로임
    unsafe {
        check(
            "original survives",
            (va(0) as *const u64).read_volatile() == 0xCAFE_F00D_0000_0001,
        );
    }

    // 회수: 원본은 남고 파생된 것만 전부 사라짐. 배지는 한 번만 붙음
    let ep = retype(ut, obj::ENDPOINT);
    check("cap ep", ep > 0);
    let badged = copy_here(ep, rights::W, 77);
    check("mint badge", badged > 0);
    check("rebadge", cap_copy(badged, me, spare, rights::ALL, 78) == err::BAD_BADGE);
    let badged2 = copy_here(badged, rights::ALL, 0);
    check("copy keeps badge", badged2 > 0);
    check("send only cap recv", recv(badged, ipc::NONBLOCK).status == err::NO_RIGHTS);
    let rx = copy_here(ep, rights::R, 0);
    check("recv only cap send", rx > 0 && send(rx, ipc::NONBLOCK, [0; 4]).status == err::NO_RIGHTS);
    check("revoke ep", cap_revoke(ep) == 0);
    check("revoked copy gone", send(badged, ipc::NONBLOCK, [0; 4]).status == err::BAD_SLOT);
    check("revoked grandchild gone", send(badged2, ipc::NONBLOCK, [0; 4]).status == err::BAD_SLOT);
    check("revoked sibling gone", recv(rx, ipc::NONBLOCK).status == err::BAD_SLOT);
    check("revoke keeps original", recv(ep, ipc::NONBLOCK).status == err::WOULD_BLOCK);
    check("revoke null", cap_revoke(0) == err::BAD_SLOT);

    // 페이지 테이블 해제: 떼어 낸 테이블은 소거되므로 그 아래 매핑은 다른
    // 자리에 다시 설치해도 따라오지 않음
    let far = TEST_VA + 0x1000_0000;
    check("pt install", map(pt, far, 0) == 0);
    let under = retype(ut, obj::FRAME);
    check("map under pt", under > 0 && map(under, far, perm::RW) == 0);
    // SAFETY: 방금 RW로 매핑된 전용 프레임
    unsafe { (far as *mut u64).write_volatile(0x7AB1_E000_0000_0001) };
    check("unmap pt", unmap(pt) == 0);
    check("unmap pt twice", unmap(pt) == err::BAD_STATE);
    let far2 = far + 0x0800_0000;
    check("pt reinstall elsewhere", map(pt, far2, 0) == 0);
    let fresh = retype(ut, obj::FRAME);
    check("old leaf did not travel", fresh > 0 && map(fresh, far2, perm::RW) == 0);
    // 떼어진 테이블 아래에 있던 프레임은 기록이 낡았을 뿐이라 해제 후 재사용 가능
    check("stale frame unmap", unmap(under) == 0);
    check("stale frame remap", map(under, far2 + fs, perm::RO) == 0);
    // SAFETY: 방금 RO로 매핑된 프레임, 내용은 그대로여야 함
    unsafe {
        check(
            "frame content kept",
            ((far2 + fs) as *const u64).read_volatile() == 0x7AB1_E000_0000_0001,
        );
    }

    // 기록이 낡은 케이퍼빌리티의 삭제는 그 자리에 새로 들어온 남의 매핑을
    // 지우면 안 됨: 테이블을 떼었다가 같은 VA에 다시 설치하고 같은 자리에 다른
    // 프레임을 매핑한 뒤, 예전에 그 자리에 매핑됐던 케이퍼빌리티를 삭제함
    check("pt detach again", unmap(pt) == 0);
    check("pt back at same va", map(pt, far2, 0) == 0);
    let newcomer = retype(ut, obj::FRAME);
    check("newcomer at old va", newcomer > 0 && map(newcomer, far2, perm::RW) == 0);
    // SAFETY: 방금 RW로 매핑된 전용 프레임
    unsafe { (far2 as *mut u64).write_volatile(0x0E3C_0000_0000_0001) };
    check("delete stale cap", cap_delete(fresh) == 0);
    // SAFETY: 새 매핑이 지워졌다면 이 읽기에서 루트가 폴트로 종료됨
    unsafe {
        check(
            "newcomer mapping survives",
            (far2 as *const u64).read_volatile() == 0x0E3C_0000_0000_0001,
        );
    }
    check("stale unmap keeps newcomer", unmap(under) == 0);
    check("newcomer still mapped", unmap(newcomer) == 0);

    put_str("root: capability tests pass\n");
}

/// 회수한 untyped 메모리가 다시 재분류되는지를 고갈로 검증하는 함수입니다.
///
/// 작은 untyped를 프레임으로 끝까지 잘라낸 뒤 회수하면 같은 수를 다시
/// 잘라낼 수 있어야 합니다. 워터마크가 되돌아가지 않으면 두 번째 바퀴의 첫
/// 재분류가 EXHAUSTED입니다. 재분류된 메모리는 매번 소거돼 있어야 합니다.
fn test_reuse(b: &Boot) {
    let (ut, n, fs) = (b.ut_reuse, b.ut_reuse_frames, b.frame_size);
    let va = TEST_VA + 40 * fs;
    let base = REUSE_BASE.load(Ordering::Relaxed);
    if ut == 0 {
        // 슬롯 구간에 다 담기는 untyped가 없는 배치에서는 고갈 검증을 생략
        put_str("root: untyped reuse test skipped (no small untyped)\n");
        return;
    }
    for round in 0..3u64 {
        for i in 0..n {
            check("reuse retype", retype_at(ut, obj::FRAME, base + i) == 0);
        }
        check(
            "reuse exhausted",
            retype_at(ut, obj::FRAME, base - 1) == err::EXHAUSTED,
        );
        // 첫 프레임과 마지막 프레임은 소거돼 있어야 하고, 다음 바퀴를 위해 더럽힘
        for i in [0, n - 1] {
            let f = (base + i) as i64;
            check("reuse map", map(f, va, perm::RW) == 0);
            // SAFETY: 방금 RW로 매핑된 전용 프레임
            unsafe {
                check("reused memory zeroed", (va as *const u64).read_volatile() == 0);
                (va as *mut u64).write_volatile(0xD1127_0000 + round);
            }
            if i == 0 {
                check("reuse unmap", unmap(f) == 0);
            }
        }
        // 마지막 프레임은 매핑된 채로 회수: 매핑도 함께 사라져야 함
        check("reuse revoke", cap_revoke(ut as i64) == 0);
        check("revoked frame gone", unmap((base + n - 1) as i64) == err::BAD_SLOT);
        check("revoked first gone", unmap(base as i64) == err::BAD_SLOT);
    }
    // 회수된 매핑 자리에는 다른 프레임을 매핑할 수 있어야 함
    let f = retype(b.ut_main, obj::FRAME);
    check("revoked mapping gone", f > 0 && map(f, va, perm::RW) == 0);
    put_str("root: untyped reuse test pass\n");
}

/// 스케줄러, IPC, 폴트 격리를 루트와 주소 공간을 함께 쓰는 태스크들로
/// 검증하는 함수입니다.
fn test_sched_ipc_fault(b: &Boot) {
    let (ut, fs) = (b.ut_main, b.frame_size);
    let f = slot::CONSOLE as i64;
    // 이 검증이 받는 슬롯은 끝에서 전부 돌려줌
    let first_slot = NEXT_SLOT.load(Ordering::Relaxed);

    // 자식 스택 배치: +5 그래뉼은 스택 가드로 비워 둠
    let s2 = retype(ut, obj::FRAME);
    check("retype stack 2", s2 > 0);
    check("map stack 2", map(s2, TEST_VA + 6 * fs, perm::RW) == 0);
    let sp_exit = TEST_VA + 7 * fs;

    // 구성하지 않고 거부 경로 확인에만 쓰는 TCB
    let tcb_spare = retype(ut, obj::TCB);
    check("retype tcb spare", tcb_spare > 0);
    let tcb_exit = retype(ut, obj::TCB);
    check("retype tcb exit", tcb_exit > 0);
    let entry_exit = child_exit_entry as usize as u64;
    let (aspace, cnode) = (slot::ADDR_SPACE as i64, slot::CNODE as i64);

    // 거부 경로: 구성 전 재개, 잘못된 케이퍼빌리티, 스택 정렬 위반
    check("resume unconfigured", tcb_resume(tcb_spare) == err::BAD_STATE);
    check(
        "configure bad tcb",
        tcb_configure(f, entry_exit, sp_exit, aspace, cnode, 0) == err::BAD_CAP,
    );
    check(
        "configure bad aspace",
        tcb_configure(tcb_spare, entry_exit, sp_exit, cnode, cnode, 0) == err::BAD_CAP,
    );
    check(
        "configure bad cnode",
        tcb_configure(tcb_spare, entry_exit, sp_exit, aspace, aspace, 0) == err::BAD_CAP,
    );
    check(
        "configure empty cnode slot",
        tcb_configure(tcb_spare, entry_exit, sp_exit, aspace, 9999, 0) == err::BAD_SLOT,
    );
    check(
        "configure bad stack",
        thread_configure(tcb_spare, entry_exit, sp_exit - 8) == err::BAD_VA,
    );
    // 거부된 구성은 TCB를 건드리지 않았어야 함
    check("still unconfigured", tcb_resume(tcb_spare) == err::BAD_STATE);
    // 실행 중인 자신의 TCB는 재구성할 수 없음
    check(
        "configure self",
        thread_configure(slot::TCB as i64, entry_exit, sp_exit) == err::BAD_STATE,
    );
    // 구성된 적 없는 TCB도 삭제할 수 있음
    check("delete unconfigured tcb", cap_delete(tcb_spare) == 0);

    // 유한 태스크: 실행 -> 목표 도달 -> EXIT -> 루트로 복귀
    check(
        "configure tcb exit",
        thread_configure(tcb_exit, entry_exit, sp_exit) == 0,
    );
    check(
        "configure twice",
        thread_configure(tcb_exit, entry_exit, sp_exit) == err::BAD_STATE,
    );
    check("resume tcb exit", tcb_resume(tcb_exit) == 0);
    check("resume twice", tcb_resume(tcb_exit) == err::BAD_STATE);
    while EXIT_COUNTER.load(Ordering::Relaxed) != CHILD_EXIT_TARGET {
        yield_now();
    }
    put_str("root: child exit test pass\n");

    // 엔드포인트: 재분류, 커널 오브젝트라 매핑 불가, 빈 큐 비대기 즉시 복귀
    let ep = retype(ut, obj::ENDPOINT);
    check("retype ep", ep > 0);
    check(
        "map ep",
        map(ep, TEST_VA + 10 * fs, perm::RW) == err::BAD_CAP,
    );
    let nb = ipc::NONBLOCK;
    check("send nb empty", send(ep, nb, [1, 2, 3, 4]).status == err::WOULD_BLOCK);
    check("recv nb empty", recv(ep, nb).status == err::WOULD_BLOCK);
    check("call nb empty", call(ep, nb, [1, 2, 3, 4]).status == err::WOULD_BLOCK);
    check(
        "reply_recv nb empty",
        reply_recv(ep, nb, [0; 4]).status == err::WOULD_BLOCK,
    );

    // 거부 경로: 엔드포인트가 아닌 케이퍼빌리티, 범위 밖 슬롯
    check("send bad cap", send(f, 0, [0; 4]).status == err::BAD_CAP);
    check(
        "send bad slot",
        sys_ipc(syscall::SEND, 9999, 0, [0; 4]).status == err::BAD_SLOT,
    );

    // 서버 태스크 스폰 (스택은 +8 그래뉼, 아래 +7은 가드)
    let s3 = retype(ut, obj::FRAME);
    check("retype stack 3", s3 > 0);
    check("map stack 3", map(s3, TEST_VA + 8 * fs, perm::RW) == 0);
    let tcb_srv = retype(ut, obj::TCB);
    check("retype tcb srv", tcb_srv > 0);
    EP_SLOT.store(ep as u64, Ordering::Relaxed);
    check(
        "configure tcb srv",
        thread_configure(tcb_srv, server_entry as usize as u64, TEST_VA + 9 * fs) == 0,
    );
    check("resume tcb srv", tcb_resume(tcb_srv) == 0);

    // 송신자 선행 랑데부: 서버가 아직 RECV 전이라 루트가 블록됐다 깨어남
    let m1 = [0x1111, 0x2222, 0x3333, 0x4444];
    check("send first", send(ep, 0, m1).status == 0);
    while SRV_COUNT.load(Ordering::Relaxed) < 1 {
        yield_now();
    }
    for i in 0..4 {
        check("send first data", SRV_MR[i].load(Ordering::Relaxed) == m1[i]);
    }

    // 수신자 선행 랑데부: 서버가 RECV에서 대기 중일 때 즉시 전달.
    // NONBLOCK은 상대가 준비돼 있으면 즉시 성공해야 함
    let m2 = [0x5555, 0x6666, 0x7777, 0x8888];
    check("send to waiting nb", send(ep, nb, m2).status == 0);
    while SRV_COUNT.load(Ordering::Relaxed) < 2 {
        yield_now();
    }
    for i in 0..4 {
        check("send waiting data", SRV_MR[i].load(Ordering::Relaxed) == m2[i]);
    }

    // CALL 왕복: 대기 중인 서버에 전달, 응답은 변환 확인. 응답 방향의 x1은
    // 언제나 0이어야 함(응답자의 다른 상태가 새지 않음)
    let m3 = [0xAAAA, 0xBBBB, 0xCCCC, 0xDDDD];
    let r = call(ep, 0, m3);
    check("call status", r.status == 0);
    check("call reply", r.mr == xform(m3));
    check("call reply badge zero", r.badge == 0);
    check("call count", SRV_COUNT.load(Ordering::Relaxed) == 3);

    // CALL 대기 큐 경로: 서버가 앞 메시지를 처리하는 동안 호출자가 줄을 섬
    let m4 = [0x1010, 0x2020, 0x3030, 0x4040];
    check("send before call", send(ep, 0, m4).status == 0);
    let m5 = [0x0101, 0x0202, 0x0303, 0x0404];
    let r = call(ep, 0, m5);
    check("queued call status", r.status == 0);
    check("queued call reply", r.mr == xform(m5));
    check("queued call count", SRV_COUNT.load(Ordering::Relaxed) == 5);

    // 응답 자격 1회성: 응답 후 리셋된 자격으로 다시 CALL이 돼야 하고,
    // 수신자가 대기 중이면 NONBLOCK CALL도 즉시 성공해야 함
    let m6 = [0x9999, 0x8888, 0x7777, 0x6666];
    let r = call(ep, nb, m6);
    check("call again nb", r.status == 0 && r.mr == xform(m6));

    // 자격 덮어쓰기 검증 준비: 두 번째 엔드포인트와 클라이언트 두 개 스폰
    // (스택은 +11, +13 그래뉼, 아래 +10, +12는 가드)
    let ep2 = retype(ut, obj::ENDPOINT);
    check("retype ep2", ep2 > 0);
    EP2_SLOT.store(ep2 as u64, Ordering::Relaxed);
    SRV_HOLD.store(1, Ordering::Relaxed);
    let s4 = retype(ut, obj::FRAME);
    check("retype stack 4", s4 > 0);
    check("map stack 4", map(s4, TEST_VA + 11 * fs, perm::RW) == 0);
    let s5 = retype(ut, obj::FRAME);
    check("retype stack 5", s5 > 0);
    check("map stack 5", map(s5, TEST_VA + 13 * fs, perm::RW) == 0);
    let tcb_cl = retype(ut, obj::TCB);
    let tcb_cl2 = retype(ut, obj::TCB);
    check("retype client tcbs", tcb_cl > 0 && tcb_cl2 > 0);
    check(
        "configure client",
        thread_configure(tcb_cl, client_entry as usize as u64, TEST_VA + 12 * fs) == 0,
    );
    check(
        "configure client 2",
        thread_configure(tcb_cl2, client2_entry as usize as u64, TEST_VA + 14 * fs) == 0,
    );
    check("resume client", tcb_resume(tcb_cl) == 0);
    check("resume client 2", tcb_resume(tcb_cl2) == 0);

    // 대기 큐 FIFO: 먼저 줄 선 클라이언트의 메시지가 먼저 나와야 하고,
    // 송신자가 대기 중이면 NONBLOCK RECV도 즉시 성공해야 함. 배지 없는
    // 케이퍼빌리티로 보낸 메시지의 배지는 0
    let mut r1 = recv(ep2, nb);
    while r1.status == err::WOULD_BLOCK {
        yield_now();
        r1 = recv(ep2, nb);
    }
    check("fifo first", r1.status == 0 && r1.mr == CM_FIFO_A && r1.badge == 0);
    let mut r2 = recv(ep2, nb);
    while r2.status == err::WOULD_BLOCK {
        yield_now();
        r2 = recv(ep2, nb);
    }
    check("fifo second", r2.status == 0 && r2.mr == CM_FIFO_B);

    // 클라이언트의 CALL이 서버에 닿고, 서버가 자격을 쥔 채 RECV로 넘어가길 대기
    while SRV_COUNT.load(Ordering::Relaxed) < 7 {
        yield_now();
    }
    for (slot, v) in SRV_MR.iter().zip(CM_CALL) {
        check("held call data", slot.load(Ordering::Relaxed) == v);
    }
    check("client still held", CLIENT_STATUS.load(Ordering::Relaxed) == 1);

    // 자격 덮어쓰기: 루트의 CALL이 자격을 덮으면 이전 호출자(클라이언트)는
    // NO_REPLY로 깨어나고 루트는 정상 응답을 받아야 함
    let m7 = [0x0707, 0x1717, 0x2727, 0x3737];
    let r = call(ep, 0, m7);
    check("overwrite call", r.status == 0 && r.mr == xform(m7));
    check(
        "old caller aborted",
        CLIENT_STATUS.load(Ordering::Relaxed) == err::NO_REPLY as u64,
    );
    check("overwrite count", SRV_COUNT.load(Ordering::Relaxed) == 8);

    // 응답 없는 종료: 서버가 응답 전에 EXIT하면 호출자는 NO_REPLY로 깨어남
    SRV_DIE.store(1, Ordering::Relaxed);
    let r = call(ep, 0, [0xE0E0, 0, 0, 0]);
    check("call no reply", r.status == err::NO_REPLY);
    check("no reply count", SRV_COUNT.load(Ordering::Relaxed) == 8);

    put_str("root: ipc tests pass\n");

    // 폴트 격리와 스레드 포인터 격리 (스택은 +16, +18 그래뉼, 아래 +15,
    // +17은 가드)
    let s6 = retype(ut, obj::FRAME);
    check("retype stack 6", s6 > 0);
    check("map stack 6", map(s6, TEST_VA + 16 * fs, perm::RW) == 0);
    let s7 = retype(ut, obj::FRAME);
    check("retype stack 7", s7 > 0);
    check("map stack 7", map(s7, TEST_VA + 18 * fs, perm::RW) == 0);
    let tcb_fault = retype(ut, obj::TCB);
    let tcb_fp = retype(ut, obj::TCB);
    check("retype fault tcbs", tcb_fault > 0 && tcb_fp > 0);
    let entry_fault = child_fault_entry as usize as u64;
    let sp_fault = TEST_VA + 17 * fs;
    check(
        "configure misaligned entry",
        thread_configure(tcb_fault, entry_fault + 2, sp_fault) == err::BAD_VA,
    );

    // 자식의 폴트는 그 태스크만 종료해야 하고, 자식이 덮어쓴 TPIDR_EL0는
    // 루트에 보이면 안 되며, 자식은 루트의 값이 아닌 0을 봐야 함
    set_tpidr(ROOT_TPIDR);
    check(
        "configure tcb fault",
        thread_configure(tcb_fault, entry_fault, sp_fault) == 0,
    );
    check("resume tcb fault", tcb_resume(tcb_fault) == 0);
    while FAULT_STAGE.load(Ordering::Relaxed) < 1 {
        yield_now();
    }
    check("child tpidr zeroed", CHILD_SAW_TPIDR.load(Ordering::Relaxed) == 0);
    check("root tpidr isolated", get_tpidr() == ROOT_TPIDR);
    check("root tpidrro zero", get_tpidrro() == 0);
    // 양보하면 자식이 폴트를 내고 커널이 종료시킴, 살아 있으면 카운터가 오름
    for _ in 0..4 {
        yield_now();
    }
    check("fault child killed", FAULT_SURVIVED.load(Ordering::Relaxed) == 0);
    check("root tpidr after kill", get_tpidr() == ROOT_TPIDR);
    check("dead tcb resume", tcb_resume(tcb_fault) == err::BAD_STATE);

    // FP/SIMD 명령은 CPACR_EL1 트랩으로 그 태스크만 종료돼야 함
    check(
        "configure tcb fp",
        thread_configure(tcb_fp, child_fp_entry as usize as u64, TEST_VA + 19 * fs) == 0,
    );
    check("resume tcb fp", tcb_resume(tcb_fp) == 0);
    while FAULT_STAGE.load(Ordering::Relaxed) < 2 {
        yield_now();
    }
    for _ in 0..4 {
        yield_now();
    }
    check("fp child killed", FAULT_SURVIVED.load(Ordering::Relaxed) == 0);
    set_tpidr(0);

    // 종료된 태스크의 TCB를 삭제해도 시스템은 그대로여야 하고, 삭제된
    // 케이퍼빌리티는 더 쓸 수 없음
    check("delete dead tcb", cap_delete(tcb_fault) == 0);
    check("deleted tcb gone", tcb_resume(tcb_fault) == err::BAD_SLOT);
    put_str("root: fault isolation tests pass\n");

    // 양보 없이 도는 태스크가 타이머에 끊기는지, 그리고 틱 사이에 CPU를 받은
    // 태스크가 처음 맞는 틱을 넘기는지. 앞선 검증의 태스크는 모두 종료됐으므로
    // 준비 큐에는 루트뿐임(이 검증의 전제)
    timeslice::test(b);

    // 여기서 만든 태스크는 모두 끝났고 엔드포인트와 스택도 더 쓰지 않음
    release_slots(first_slot);
}

/// 격리된 태스크 하나를 만드는 데 필요한 케이퍼빌리티 묶음 구조체입니다.
struct Confined {
    tcb: i64,
    cnode: i64,
}

/// 격리 검증들이 함께 쓰는 환경을 담는 구조체입니다.
struct Iso {
    /// 오브젝트를 잘라낼 untyped 슬롯
    ut: u64,
    fs: u64,
    /// 격리 태스크들이 실행될 주소 공간(텍스트와 읽기 전용 데이터가 이미 매핑됨)
    aspace: i64,
    /// pulse 페이지를 루트가 보는 VA
    pulse_view: u64,
}

impl Iso {
    /// 격리 태스크가 마지막으로 적은 pulse 값을 읽는 함수입니다.
    fn pulse(&self) -> u64 {
        // SAFETY: 루트가 이 VA에 pulse 프레임을 RW로 매핑해 둠
        unsafe { (self.pulse_view as *const u64).read_volatile() }
    }

    /// pulse가 기대한 값이 될 때까지 양보하며 기다리는 함수입니다. 격리
    /// 태스크가 검증하려는 지점에 실제로 도달했음을 확인하는 데 씁니다.
    fn wait_pulse(&self, want: u64) -> bool {
        for _ in 0..64 {
            if self.pulse() == want {
                return true;
            }
            yield_now();
        }
        false
    }

    /// 격리된 태스크를 만들어 실행 대기 상태로 만드는 함수입니다.
    ///
    /// 새 CNode를 만들어 `caps`의 (원본 슬롯, 권한 마스크, 배지)를 1번 슬롯부터
    /// 차례로 복사해 넣고, 주소 공간에 스택 한 장을 매핑한 뒤 TCB를 구성해
    /// 재개합니다. 태스크가 쥐는 권한은 그 CNode에 든 것이 전부입니다.
    ///
    /// # Arguments
    /// `entry` - 진입점
    /// `arg` - 태스크가 x0으로 받을 값
    /// `caps` - CNode에 넣어 줄 케이퍼빌리티들
    fn spawn(&self, entry: u64, arg: u64, caps: &[(i64, u64, u64)]) -> Confined {
        let index = NEXT_STACK.fetch_add(1, Ordering::Relaxed);
        check("iso stack budget", index < ISO_STACKS);
        let stack_va = ISO_STACK_VA + index * ISO_STACK_STEP;
        let stack = retype(self.ut, obj::FRAME);
        check(
            "iso stack",
            stack > 0 && map_into(stack, stack_va, perm::RW, self.aspace) == 0,
        );
        let cnode = retype(self.ut, obj::CNODE);
        check("iso cnode", cnode > 0);
        for (i, &(src, mask, badge)) in caps.iter().enumerate() {
            check("iso grant", cap_copy(src, cnode, 1 + i as u64, mask, badge) == 0);
        }
        let tcb = retype(self.ut, obj::TCB);
        check("iso tcb", tcb > 0);
        check(
            "iso configure",
            tcb_configure(tcb, entry, stack_va + self.fs, self.aspace, cnode, arg) == 0,
        );
        check("iso resume", tcb_resume(tcb) == 0);
        Confined { tcb, cnode }
    }
}

/// 받을 메시지가 없는 상태가 여러 번의 양보 뒤에도 유지되는지 확인하는
/// 함수입니다.
///
/// 상대가 메시지를 보낼 수 있는데도 보내지 않았다는 뜻일 뿐, 상대가 실행
/// 중이 아니라는 증거는 아닙니다. 실행 여부는 pulse로 따로 확인합니다.
fn stays_silent(ep: i64) -> bool {
    for _ in 0..6 {
        yield_now();
        if recv(ep, ipc::NONBLOCK).status != err::WOULD_BLOCK {
            return false;
        }
    }
    true
}

/// 격리 태스크용 주소 공간을 만드는 함수입니다.
///
/// 비어 있는 새 루트에 테이블 세 장을 깔고 이 이미지의 텍스트(RX)와 읽기
/// 전용 데이터(RO)만 같은 VA에 매핑합니다. `.data`와 `.bss`는 매핑하지
/// 않으므로 그 안에서 실행되는 태스크는 루트의 static을 볼 수 없습니다.
/// 매핑하지 않은 첫 데이터 프레임의 VA를 함께 돌려줍니다.
fn make_aspace(ut: u64, b: &Boot) -> (i64, u64) {
    let aspace = retype(ut, obj::ADDR_SPACE);
    check("iso aspace", aspace > 0);
    let image_va = desc(b.image_first).base;
    check("iso leaf needs tables", {
        let probe = copy_here(b.image_first as i64, rights::ALL, 0);
        probe > 0 && map_into(probe, image_va, perm::RO, aspace) == err::MISSING_TABLE
    });
    for _ in 0..3 {
        let pt = retype(ut, obj::PAGE_TABLE);
        check("iso pt", pt > 0 && map_into(pt, image_va, 0, aspace) == 0);
    }
    let mut data_va = 0;
    for i in b.image_first..b.image_end {
        let d = desc(i);
        let p = if d.rights & rights::X != 0 {
            perm::RX
        } else if d.rights & rights::W == 0 {
            perm::RO
        } else {
            if data_va == 0 {
                data_va = d.base;
            }
            continue;
        };
        let c = copy_here(i as i64, rights::ALL, 0);
        check("iso image copy", c > 0);
        check("iso image map", map_into(c, d.base, p, aspace) == 0);
    }
    (aspace, data_va)
}

/// 태스크 격리와 회수에 따른 종료를 검증하는 함수입니다.
fn test_isolation(b: &Boot) {
    let (ut, fs) = (b.ut_small, b.frame_size);
    let nb = ipc::NONBLOCK;
    let (aspace, data_va) = make_aspace(ut, b);
    let secret = &raw const ROOT_SECRET as u64;
    check("secret is in data", data_va != 0 && secret >= data_va);
    // 부팅 시험 스크립트가 격리 태스크의 폴트 주소와 대조할 수 있게 출력
    put_str("root: secret at ");
    put_hex(secret);
    put_str("\n");

    // 공유 페이지: 루트는 RW로, 격리 태스크는 같은 프레임을 RO로 봄. 격리
    // 태스크가 보는 VA에는 루트 쪽에서 전혀 다른 프레임을 매핑해 두 주소
    // 공간의 변환이 서로 독립임을 확인함
    let shared = retype(ut, obj::FRAME);
    let root_view = TEST_VA + 44 * fs;
    check("iso shared", shared > 0 && map(shared, root_view, perm::RW) == 0);
    // SAFETY: 방금 RW로 매핑된 전용 프레임
    unsafe { (root_view as *mut u64).write_volatile(ISO_MAGIC) };
    let shared_ro = copy_here(shared, rights::R, 0);
    check(
        "iso shared map",
        shared_ro > 0 && map_into(shared_ro, ISO_SHARED_VA, perm::RO, aspace) == 0,
    );
    // 격리 태스크의 주소 공간에 쓰기 가능한 공유 매핑은 만들 수 없음
    let shared_ro2 = copy_here(shared_ro, rights::ALL, 0);
    check(
        "iso shared stays ro",
        map_into(shared_ro2, ISO_SHARED_VA + fs, perm::RW, aspace) == err::NO_RIGHTS,
    );
    // 루트 쪽에는 이 VA까지의 테이블이 이미 있으므로 설치할 자리가 없음
    let extra_pt = retype(ut, obj::PAGE_TABLE);
    check("root tables complete", extra_pt > 0 && map(extra_pt, ISO_SHARED_VA, 0) == err::OVERLAP);
    let root_only = retype(ut, obj::FRAME);
    check(
        "iso root-only frame",
        root_only > 0 && map(root_only, ISO_SHARED_VA, perm::RW) == 0,
    );
    // SAFETY: 방금 RW로 매핑된 전용 프레임
    unsafe { (ISO_SHARED_VA as *mut u64).write_volatile(ISO_ROOT_ONLY) };

    // pulse 페이지: 격리 태스크가 케이퍼빌리티 없이 진행 상황을 적는 곳
    let pulse_frame = retype(ut, obj::FRAME);
    let pulse_view = TEST_VA + 46 * fs;
    check("pulse frame", pulse_frame > 0 && map(pulse_frame, pulse_view, perm::RW) == 0);
    let pulse_c = copy_here(pulse_frame, rights::ALL, 0);
    check(
        "pulse map",
        pulse_c > 0 && map_into(pulse_c, ISO_PULSE_VA, perm::RW, aspace) == 0,
    );
    let iso = Iso {
        ut,
        fs,
        aspace,
        pulse_view,
    };

    // 보고용 엔드포인트. 격리 태스크에는 배지가 붙은 송신 전용 복사본만 줌
    let ep = retype(ut, obj::ENDPOINT);
    check("iso ep", ep > 0);
    let t = iso.spawn(confined_entry as usize as u64, b.ut_main, &[(ep, rights::W, ISO_BADGE)]);

    // 격리 태스크의 보고: 배지는 커널이 케이퍼빌리티에서 읽어 주므로 위조
    // 불가. 거부 비트맵이 전부 서 있어야 하고, 공유 페이지는 루트가 쓴 값으로
    // 보여야 함(루트가 같은 VA에 둔 다른 프레임의 값이 아님)
    let r = recv(ep, 0);
    check("iso report", r.status == 0);
    check("iso badge", r.badge == ISO_BADGE);
    check("iso refused everything", r.mr[0] == ISO_ALL_REFUSED);
    check("iso shared value", r.mr[1] == ISO_MAGIC);
    put_str("root: confined task reported\n");

    // 루트 전용 주소를 알려 주고 읽게 함. 그 주소는 격리 태스크의 주소 공간에
    // 없으므로 폴트로 종료돼야 하고, 읽은 값을 보내는 송신은 오지 않아야 함
    let r = reply_recv(ep, nb, [secret, 0, 0, 0]);
    check("iso reply", r.status == err::WOULD_BLOCK);
    check("iso secret unread", stays_silent(ep));
    // 종료된 격리 태스크의 정리: TCB를 삭제하면 TCB에 묶여 있던 CNode 복사본도
    // 사라지고, CNode 원본을 삭제하면 그 안에 넣어 준 복사본까지 정리됨
    check("iso delete tcb", cap_delete(t.tcb) == 0);
    check("iso delete cnode", cap_delete(t.cnode) == 0);
    put_str("root: isolation tests pass\n");

    // 하드웨어가 막아야 하는 접근. 태스크가 시도 지점에 도달했음을 pulse로
    // 확인한 뒤, 그 뒤의 코드가 실행되지 않았음을(pulse 불변, 송신 없음) 확인.
    // 폴트의 종류와 개수는 부팅 시험 스크립트가 커널 로그에서 대조함
    let q = retype(ut, obj::ENDPOINT);
    check("report ep", q > 0);
    let violator = violator_entry as usize as u64;
    for kind in 0..4 {
        let v = iso.spawn(violator, kind, &[(q, rights::W, 0)]);
        check("violator arrived", iso.wait_pulse(PULSE_VIOLATOR + kind));
        check("violator stopped", stays_silent(q));
        check("violator did not continue", iso.pulse() == PULSE_VIOLATOR + kind);
        check("violator cleanup", cap_delete(v.tcb) == 0);
    }
    // 읽기 전용 공유 페이지는 쓰이지 않았어야 함
    // SAFETY: 루트의 RW 매핑으로 같은 프레임을 읽음
    unsafe { check("ro page unchanged", (root_view as *const u64).read_volatile() == ISO_MAGIC) };
    put_str("root: violation tests pass\n");

    test_teardown(&iso, q);
    // 타임아웃 검증의 오브젝트는 가장 큰 untyped에서 잘라, 이 작은 untyped가
    // 대야 하는 프레임 수를 늘리지 않음. 태스크는 같은 주소 공간에서 실행되므로
    // 아래의 회수가 그 주소 공간을 없앨 때 함께 종료됨
    let timed_iso = Iso {
        ut: b.ut_main,
        fs,
        aspace,
        pulse_view,
    };
    test_timeouts(&timed_iso, q);

    // CNode 회수: 태스크에 묶인 케이퍼빌리티 공간이 사라지면 그 태스크는
    // 종료됨. 송신이 거부될 뿐 계속 실행된다면 pulse가 계속 바뀜
    let beat = beat_entry as usize as u64;
    let t = iso.spawn(beat, 0, &[(q, rights::W, 0)]);
    let r = recv(q, 0);
    check("beat alive", r.status == 0 && r.mr[1] == BEAT_TAG);
    check("beat revoke cnode", cap_revoke(t.cnode) == 0);
    let frozen = iso.pulse();
    check("beat silent after cnode revoke", stays_silent(q));
    check("beat stopped by cnode revoke", iso.pulse() == frozen);
    check("beat tcb is dead", tcb_resume(t.tcb) == err::BAD_STATE);

    // 자기 TCB의 원본을 스스로 삭제: 시스템 콜 도중에 호출자가 종료되는 경로
    let destruct = self_destruct_entry as usize as u64;
    let d = iso.spawn(destruct, syscall::CAP_DELETE, &[(q, rights::W, 0)]);
    check("hand over own tcb", cap_move(d.tcb, d.cnode, 2) == 0);
    let r = recv(q, 0);
    check("self delete started", r.status == 0 && r.mr == [1, 0x5E1F, 0, 0]);
    check("self delete did not return", stays_silent(q));

    // 살아 있는(현재 설치돼 있지 않은) 주소 공간에서의 매핑 해제: 자리가 비어
    // 다른 프레임을 매핑할 수 있어야 함
    check("unmap in other aspace", unmap(shared_ro) == 0);
    check(
        "other aspace slot freed",
        map_into(shared_ro2, ISO_SHARED_VA, perm::RO, aspace) == 0,
    );

    // 주소 공간 삭제: 그 안에서 실행 중이던 태스크가 종료되고, 그 주소 공간에
    // 매핑돼 있던 프레임은 기록이 낡은 채로 남아 해제 후 다시 쓸 수 있음
    let t = iso.spawn(beat, 0, &[(q, rights::W, 0)]);
    let r = recv(q, 0);
    check("beat 2 alive", r.status == 0 && r.mr[1] == BEAT_TAG);
    check("delete aspace", cap_delete(aspace) == 0);
    let frozen = iso.pulse();
    check("beat silent after aspace delete", stays_silent(q));
    check("beat stopped by aspace delete", iso.pulse() == frozen);
    check("beat 2 tcb is dead", tcb_resume(t.tcb) == err::BAD_STATE);
    check(
        "aspace cap gone",
        map_into(shared_ro, ISO_SHARED_VA + fs, perm::RO, aspace) == err::BAD_SLOT,
    );
    check("stale mapping unmap", unmap(shared_ro2) == 0);
    check("stale mapping remap", map(shared_ro2, TEST_VA + 45 * fs, perm::RO) == 0);
    // SAFETY: 방금 RO로 매핑된 공유 프레임
    unsafe {
        check(
            "shared frame intact",
            ((TEST_VA + 45 * fs) as *const u64).read_volatile() == ISO_MAGIC,
        );
    }
    check(
        "root memory intact",
        ROOT_SECRET.load(Ordering::Relaxed) == 0x5EC2_E700_DEAD_BEEF,
    );

    // 이 검증이 만든 것을 전부 회수: 루트 쪽 매핑까지 사라지고 메모리는 다시
    // 재분류할 수 있음
    check("revoke everything", cap_revoke(ut as i64) == 0);
    check("revoked ep gone", recv(q, nb).status == err::BAD_SLOT);
    check("revoked frame gone", unmap(shared) == err::BAD_SLOT);
    let f = alloc_slot();
    check("retype after revoke", retype_at(ut, obj::FRAME, f) == 0);
    check(
        "revoked root mapping gone",
        map(f as i64, root_view, perm::RW) == 0,
    );
    // SAFETY: 방금 RW로 매핑된 프레임, 재분류가 소거를 보장함
    unsafe { check("recycled frame zeroed", (root_view as *const u64).read_volatile() == 0) };

    // 마지막으로 가장 거친 경로: 격리 태스크에게 그 태스크 자신(TCB, CNode,
    // 주소 공간, 스택)을 잘라낸 untyped를 통째로 넘기고 회수하게 함. 실행
    // 중인 태스크가 자기 발밑을 전부 없애는 것이므로 그 시스템 콜에서 종료돼야
    // 하고, 현재 설치된 주소 공간까지 사라지지만 시스템은 계속 돌아야 함.
    // 보고용 엔드포인트만 다른 untyped에서 만들어 살아남게 함
    let out = retype(b.ut_main, obj::ENDPOINT);
    check("last ep", out > 0);
    let (aspace, _) = make_aspace(ut, b);
    let last_iso = Iso {
        ut,
        fs,
        aspace,
        pulse_view,
    };
    NEXT_STACK.store(0, Ordering::Relaxed);
    let last = last_iso.spawn(destruct, syscall::CAP_REVOKE, &[(out, rights::W, 0)]);
    check("hand over untyped", cap_move(ut as i64, last.cnode, 2) == 0);
    check("untyped moved away", retype_at(ut, obj::FRAME, f + 1) == err::NOT_UNTYPED);
    let r = recv(out, 0);
    check("self revoke started", r.status == 0 && r.mr == [1, 0x5E1F, 0, 0]);
    check("self revoke did not return", stays_silent(out));
    // 그 untyped에서 나온 것은 루트가 쥐고 있던 것까지 전부 사라졌어야 함
    check("self revoke took tcb", tcb_resume(last.tcb) == err::BAD_SLOT);
    check("self revoke took cnode", cap_delete(last.cnode) == err::BAD_SLOT);
    check("self revoke took aspace", cap_delete(aspace) == err::BAD_SLOT);
    check("self revoke took frame", unmap(f as i64) == err::BAD_SLOT);
    // 루트는 여전히 자기 주소 공간에서 돌고 있고 다른 untyped는 멀쩡함
    check(
        "root memory still intact",
        ROOT_SECRET.load(Ordering::Relaxed) == 0x5EC2_E700_DEAD_BEEF,
    );
    let g = retype(b.ut_main, obj::FRAME);
    check("other untyped still works", g > 0 && map(g, root_view, perm::RW) == 0);
    put_str("root: revocation tests pass\n");
}

/// 태스크가 놓일 수 있는 상태마다 그 태스크를 삭제해 보는 함수입니다.
///
/// 삭제된 TCB를 가리키는 큐 링크나 응답 링크가 남으면 그 뒤의 IPC가 사라진
/// 태스크로 메시지를 전달하거나 그 태스크를 다시 실행시키게 됩니다. 각
/// 단계는 그런 일이 있었다면 달라졌을 결과(도착한 메시지, 반환 코드, pulse)를
/// 확인합니다.
///
/// # Arguments
/// `q` - 격리 태스크들이 보고에 쓰는 엔드포인트
fn test_teardown(iso: &Iso, q: i64) {
    let (ut, nb) = (iso.ut, ipc::NONBLOCK);
    let blocker = blocker_entry as usize as u64;
    let reporter = reporter_entry as usize as u64;
    let report = |status: i64, mode: u64| [status as u64, REPORT_TAG, mode, 0];

    // 대기 큐에 혼자 블록된 송신자의 삭제: 큐에서 빠져 메시지가 전달되지 않음
    let t = iso.spawn(blocker, 5, &[(q, rights::W, 0)]);
    check("blocker arrived", iso.wait_pulse(5));
    check("blk delete blocked tcb", cap_delete(t.tcb) == 0);
    check("blk queue emptied", stays_silent(q));

    // 대기 큐 한가운데, 꼬리, 머리의 삭제: 남은 태스크의 순서와 큐 양끝이
    // 온전해야 함. 네 태스크가 차례로 줄을 선 뒤 둘째, 넷째, 첫째를 지우면
    // 셋째의 메시지만 도착해야 함
    let mut line = [0i64; 4];
    for (i, tcb) in line.iter_mut().enumerate() {
        let n = 10 + i as u64;
        *tcb = iso.spawn(blocker, n, &[(q, rights::W, 0)]).tcb;
        check("queue member arrived", iso.wait_pulse(n));
    }
    // 마지막 태스크가 pulse를 적고 송신에서 블록될 틈을 줌
    yield_now();
    check("queue delete middle", cap_delete(line[1]) == 0);
    check("queue delete tail", cap_delete(line[3]) == 0);
    check("queue delete head", cap_delete(line[0]) == 0);
    let r = recv(q, nb);
    check("queue survivor", r.status == 0 && r.mr == [0xB10C, 12, 0, 0]);
    check("queue now empty", stays_silent(q));
    check("queue delete survivor", cap_delete(line[2]) == 0);

    // 준비 큐에서의 삭제: 재개만 한 태스크 셋을 가운데, 꼬리, 머리 순으로 지움.
    // 종료된 TCB가 큐에 남으면 소거된 컨텍스트로 실행돼 폴트 줄이 하나 더
    // 생기고(부팅 시험 스크립트가 개수를 대조함), 살아서 실행되면 메시지가 옴.
    // 지우기 전에 타이머 선점으로 잠깐 실행됐더라도 송신에서 블록될 뿐이라
    // 판정에는 영향이 없음
    for (i, tcb) in line.iter_mut().take(3).enumerate() {
        *tcb = iso.spawn(blocker, 20 + i as u64, &[(q, rights::W, 0)]).tcb;
    }
    check("ready delete middle", cap_delete(line[1]) == 0);
    check("ready delete tail", cap_delete(line[2]) == 0);
    check("ready delete head", cap_delete(line[0]) == 0);
    check("ready queue emptied", stays_silent(q));

    // 구성 전(Inactive) TCB의 삭제
    let raw = retype(ut, obj::TCB);
    check("delete inactive tcb", raw > 0 && cap_delete(raw) == 0);
    // 구성만 된(Stopped) TCB: 묶인 CNode가 회수되면 종료되어 재개할 수 없음.
    // 종료 처리가 빠졌다면 재개가 성공해 케이퍼빌리티 공간 없는 태스크가 실행됨
    let cn = retype(ut, obj::CNODE);
    let stopped = retype(ut, obj::TCB);
    check("stopped tcb", cn > 0 && stopped > 0);
    check(
        "configure stopped",
        tcb_configure(stopped, blocker, ISO_STACK_VA + iso.fs, iso.aspace, cn, 99) == 0,
    );
    check("revoke stopped cnode", cap_revoke(cn) == 0);
    check("stopped tcb torn down", tcb_resume(stopped) == err::BAD_STATE);
    check("delete stopped tcb", cap_delete(stopped) == 0);
    check("stopped tcb gone", tcb_resume(stopped) == err::BAD_SLOT);
    check("delete its cnode", cap_delete(cn) == 0);

    // 수신 대기 중(BlockedRecv)인 태스크의 삭제: 큐에서 빠지므로 뒤이은 비대기
    // 송신은 받을 상대가 없음. 남아 있었다면 송신이 사라진 태스크로 전달됨
    let lone = retype(ut, obj::ENDPOINT);
    check("lone ep", lone > 0);
    let l = iso.spawn(reporter, 0, &[(lone, rights::R, 0), (q, rights::W, 0)]);
    check("receiver arrived", iso.wait_pulse(PULSE_REPORTER));
    yield_now();
    check("delete blocked receiver", cap_delete(l.tcb) == 0);
    check("receiver left the queue", send(lone, nb, [1, 2, 3, 4]).status == err::WOULD_BLOCK);

    // CALL로 줄 선(BlockedCall) 태스크의 삭제: 뒤이은 비대기 수신은 받을 것이 없음
    let l = iso.spawn(reporter, 2, &[(lone, rights::W, 0), (q, rights::W, 0)]);
    check("queued caller arrived", iso.wait_pulse(PULSE_REPORTER + 2));
    yield_now();
    check("delete queued caller", cap_delete(l.tcb) == 0);
    check("caller left the queue", recv(lone, nb).status == err::WOULD_BLOCK);
    check("deleted waiters stay silent", stays_silent(q));

    // 응답 대기 중(BlockedReply)인 호출자의 삭제: 응답 자격이 함께 사라짐.
    // 남아 있었다면 뒤늦은 응답이 사라진 TCB를 깨워 폴트 로그가 하나 더 생김
    // (부팅 시험 스크립트가 폴트 개수를 대조함)
    let c = iso.spawn(reporter, 2, &[(q, rights::W, 0), (q, rights::W, 0)]);
    let r = recv(q, 0);
    check("caller arrived", r.status == 0 && r.mr[0] == 0xCA);
    check("delete waiting caller", cap_delete(c.tcb) == 0);
    check("reply right withdrawn", reply_recv(q, nb, [0xDEAD; 4]).status == err::WOULD_BLOCK);
    check("caller stays gone", stays_silent(q));

    // 응답 자격을 쥔 서버의 삭제: 기다리던 호출자는 NO_REPLY로 깨어남
    let svc = retype(ut, obj::ENDPOINT);
    check("svc ep", svc > 0);
    let holder = iso.spawn(holder_entry as usize as u64, 0, &[(svc, rights::R, 0)]);
    let client = iso.spawn(reporter, 2, &[(svc, rights::W, 0), (q, rights::W, 7)]);
    check("holder took the call", iso.wait_pulse(PULSE_HOLDER + 0xCA));
    check("client still waiting", recv(q, nb).status == err::WOULD_BLOCK);
    check("delete reply holder", cap_delete(holder.tcb) == 0);
    let r = recv(q, 0);
    check(
        "client woke with no reply",
        r.status == 0 && r.badge == 7 && r.mr == report(err::NO_REPLY, 2),
    );
    check("delete client", cap_delete(client.tcb) == 0);

    // 수신 대기자가 있는 엔드포인트의 삭제: REVOKED로 깨어남
    let gone = retype(ut, obj::ENDPOINT);
    check("wait ep", gone > 0);
    let w = iso.spawn(reporter, 0, &[(gone, rights::R, 0), (q, rights::W, 9)]);
    check("waiter arrived", iso.wait_pulse(PULSE_REPORTER));
    yield_now();
    check("wait still blocked", recv(q, nb).status == err::WOULD_BLOCK);
    check("wait delete ep", cap_delete(gone) == 0);
    let r = recv(q, 0);
    check(
        "waiter woke with revoked",
        r.status == 0 && r.badge == 9 && r.mr == report(err::REVOKED, 0),
    );
    check("wait delete tcb", cap_delete(w.tcb) == 0);

    // 송신자와 호출자가 함께 줄 선 엔드포인트의 삭제: 둘 다 REVOKED로 깨어남
    let doomed = retype(ut, obj::ENDPOINT);
    check("doomed ep", doomed > 0);
    let a = iso.spawn(reporter, 1, &[(doomed, rights::W, 0), (q, rights::W, 1)]);
    check("queued sender arrived", iso.wait_pulse(PULSE_REPORTER + 1));
    let c = iso.spawn(reporter, 2, &[(doomed, rights::W, 0), (q, rights::W, 2)]);
    check("second caller arrived", iso.wait_pulse(PULSE_REPORTER + 2));
    yield_now();
    check("both still blocked", recv(q, nb).status == err::WOULD_BLOCK);
    check("delete doomed ep", cap_delete(doomed) == 0);
    let mut seen = 0;
    for _ in 0..2 {
        let r = recv(q, 0);
        check(
            "queued task woke with revoked",
            r.status == 0 && r.mr == report(err::REVOKED, r.badge) && (r.badge == 1 || r.badge == 2),
        );
        seen |= r.badge;
    }
    check("both queued tasks reported", seen == 3);
    check("delete queued sender", cap_delete(a.tcb) == 0);
    check("delete second caller", cap_delete(c.tcb) == 0);
    check("teardown leaves q quiet", stays_silent(q));
    put_str("root: teardown tests pass\n");
}

/// 커널의 별칭 테이블과 주소 공간 등록부가 untyped를 쥔 태스크에게 바닥나지
/// 않는 것을 검증하는 함수입니다.
///
/// 예전 커널은 별칭 테이블을 정적 풀 40장에서, 주소 공간 번호를 256칸짜리
/// 표에서 내주었습니다. 그래서 untyped를 넉넉히 쥔 태스크 하나가 둘 다 바닥내
/// 다른 모든 태스크의 재분류를 실패시킬 수 있었습니다. 지금은 둘 다 부팅 때
/// 메모리 크기에 맞춰 잡습니다. 다른 검증이 끝난 뒤에 가장 큰 untyped를 써서
/// 예전 한계를 넘겨 봅니다.
fn test_reserve(b: &Boot) {
    let (ut, fs) = (b.ut_main, b.frame_size);
    let tmp = alloc_slot();

    // 주소 공간 300개가 동시에 살아 있음. 루트 CNode에는 그만한 빈 슬롯이
    // 없으므로 만든 것을 새 CNode들로 옮겨 둠
    const NODES: usize = 5;
    const PER_NODE: u64 = 60;
    let mut nodes = [0i64; NODES];
    for n in nodes.iter_mut() {
        *n = retype(ut, obj::CNODE);
        check("reserve cnode", *n > 0);
    }
    for &cn in &nodes {
        for i in 1..=PER_NODE {
            check("address space beyond 256", retype_at(ut, obj::ADDR_SPACE, tmp) == 0);
            check("park address space", cap_move(tmp as i64, cn, i) == 0);
        }
    }
    // CNode를 지우면 그 안의 주소 공간이 함께 종료되고 등록부 항목이 풀림
    for &cn in &nodes {
        check("drop address spaces", cap_delete(cn) == 0);
    }
    check(
        "address space after the burst",
        retype_at(ut, obj::ADDR_SPACE, tmp) == 0 && cap_delete(tmp as i64) == 0,
    );

    // 프레임을 하나씩 재분류하며 untyped를 따라 올라감. 재분류마다 커널이 그
    // 그래뉼의 별칭을 만들고, 최하위 테이블 하나가 덮는 구획(4K 그래뉼에서
    // 2MiB)을 넘을 때마다 테이블이 한 장 더 듦. 예전 풀의 여유는 24장
    // 남짓이었으므로 40구획을 넘기면 반드시 바닥났음. untyped가 그보다 작은
    // 배치에서는 끝까지 감
    let per_table = fs / 8;
    // 이 검증 뒤에 오브젝트를 더 만드는 이미지(입력 검증, 서비스)에서는 주
    // untyped의 절반까지만 걸어 그 몫을 남김. 그래뉼이 큰 배치에서는 40구획이
    // untyped보다 커서, 끝까지 걸으면 뒤에 쓸 메모리가 없음
    let limit = if cfg!(feature = "console-input") {
        (40 * per_table).min(frames_in(desc(ut), fs) / 2)
    } else {
        40 * per_table
    };
    let mut walked = 0u64;
    while walked < limit {
        let r = retype_at(ut, obj::FRAME, tmp);
        if r == err::EXHAUSTED {
            break;
        }
        check("alias tables never run out", r == 0);
        check("drop walked frame", cap_delete(tmp as i64) == 0);
        walked += 1;
    }
    // untyped가 테이블 한 장 분량보다 작은 배치에서는 끝까지 간 것으로 충분함
    check("walked some frames", walked > 0);
    put_str("root: kernel reserve tests pass\n");
}

/// IPC 타임아웃과, 모든 태스크가 블록돼도 커널이 멈추지 않는 것을 검증하는
/// 함수입니다.
///
/// 틱 주기가 1초라서 기한을 기다리는 단계마다 1초에서 2초가 걸립니다. 그래서
/// 여러 대기자를 같은 틱에 겹쳐 놓습니다.
///
/// # Arguments
/// `q` - 격리 태스크들이 보고에 쓰는 엔드포인트
fn test_timeouts(iso: &Iso, q: i64) {
    let (ut, nb) = (iso.ut, ipc::NONBLOCK);
    let timed = timed_entry as usize as u64;
    let lone = retype(ut, obj::ENDPOINT);
    check("timeout lone ep", lone > 0);

    // 뜻이 정해지지 않은 플래그 비트는 거부되고, 비대기 플래그는 타임아웃보다
    // 우선함
    check("flag bit 1 refused", recv(lone, 2).status == err::BAD_FLAGS);
    check("flag bit 31 refused", send(lone, 1 << 31, [0; 4]).status == err::BAD_FLAGS);
    check("flag bits refused on call", call(lone, nb | 4, [0; 4]).status == err::BAD_FLAGS);
    check(
        "flag bits refused on reply_recv",
        reply_recv(lone, 0x100, [0; 4]).status == err::BAD_FLAGS,
    );
    check(
        "nonblock beats timeout",
        recv(lone, nb | ipc::timeout(9)).status == err::WOULD_BLOCK,
    );

    // 느린 서버와, 상대가 없는 엔드포인트에서 기한을 걸고 기다리는 세 태스크
    // (수신, 송신, 수신자를 기다리는 CALL)
    let (srv_ep, go) = (retype(ut, obj::ENDPOINT), retype(ut, obj::ENDPOINT));
    check("slow server eps", srv_ep > 0 && go > 0);
    iso.spawn(
        slow_server_entry as usize as u64,
        0,
        &[(srv_ep, rights::R, 0), (go, rights::R, 0)],
    );
    for mode in 0..3u64 {
        let ep = retype(ut, obj::ENDPOINT);
        check("timed ep", ep > 0);
        let need = if mode == 0 { rights::R } else { rights::W };
        iso.spawn(timed, mode | (1 << 8), &[(ep, need, 0), (q, rights::W, 0)]);
    }

    // 루트는 서버에 기한을 걸고 CALL함. 서버는 메시지를 받고 신호를 기다리며
    // 블록되고 세 태스크도 각자 블록되므로 실행할 태스크가 하나도 남지 않음.
    // 예전 커널은 여기서 코어를 세웠음. 이제는 틱을 기다렸다가 기한이 지난
    // 대기자를 깨우고, 루트는 메시지가 전달된 뒤였으므로 REPLY_TIMEOUT을 받음
    let r = call(srv_ep, ipc::timeout(1), [1, 0, 0, 0]);
    check("call reply timeout", r.status == err::REPLY_TIMEOUT);

    // 세 태스크는 전부 TIMEOUT으로 깨어나 보고함. 아직 기한 전이면 루트는
    // 기한 없이 기다리고, 그동안에도 틱이 그들을 깨움
    let mut seen = 0u64;
    for _ in 0..3 {
        let r = recv(q, 0);
        check(
            "timed report",
            r.status == 0 && r.mr[1] == TIMED_TAG && r.mr[0] == err::TIMEOUT as u64 && r.mr[2] < 3,
        );
        seen |= 1 << r.mr[2];
    }
    check("all three timed out", seen == 0b111);

    // 서버의 뒤늦은 응답은 받을 상대가 없어 버려지고, 서버는 다음 호출에
    // 그 호출의 응답을 돌려줌(첫 호출의 응답이 섞여 오지 않음)
    check("release slow server", send(go, 0, [0; 4]).status == 0);
    let r = call(srv_ep, 0, [2, 0, 0, 0]);
    check("late reply dropped", r.status == 0 && r.mr[0] == 2 ^ SLOW_XOR);

    // 기한을 건 채 블록된 태스크의 삭제: 기한 목록에서 빠지므로, 그 기한이
    // 지나는 뒤의 틱에서 커널이 사라진 TCB를 따라가지 않음
    let gone_ep = retype(ut, obj::ENDPOINT);
    check("gone ep", gone_ep > 0);
    let g = iso.spawn(timed, 1 << 8, &[(gone_ep, rights::R, 0), (q, rights::W, 0)]);
    check("timed waiter arrived", iso.wait_pulse(PULSE_TIMED));
    yield_now();
    check("delete timed waiter", cap_delete(g.tcb) == 0);

    // 기한 전에 메시지로 깨어난 대기자의 기한은 취소됨. 남아 있었다면 바로
    // 다음의 기한 있는 대기에서 커널이 이중 등록을 발견하고 정지함. 이
    // 시점에는 앞의 세 태스크가 양보 루프를 돌고 있으므로 기한 처리는 대기
    // 루프가 아니라 선점 경로에서 일어남
    let b = iso.spawn(blocker_entry as usize as u64, 77, &[(q, rights::W, 0)]);
    let r = recv(q, ipc::timeout(1));
    check("woken before deadline", r.status == 0 && r.mr == [0xB10C, 77, 0, 0]);
    check("own recv times out", recv(lone, ipc::timeout(1)).status == err::TIMEOUT);
    check("deleted waiter stayed gone", recv(q, nb).status == err::WOULD_BLOCK);
    check("delete blocker", cap_delete(b.tcb) == 0);
    put_str("root: timeout tests pass\n");
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let _ = sys1(syscall::EXIT, 1);
    loop {
        core::hint::spin_loop();
    }
}
