//! 진입 페이즈 3의 루트 태스크(초기 사용자 공간 서버)입니다.
//!
//! # Features
//! 커널이 무결성 검증 후 EL0로 띄우는 첫 태스크입니다. bootinfo 페이지에서
//! 케이퍼빌리티 목록을 읽고, 재분류(RETYPE)·매핑(MAP)·태스크 생성
//! (TCB_CONFIGURE / TCB_RESUME)·스케줄러(양보, 종료, 타이머 선점)·동기
//! 랑데부 IPC(SEND / RECV / CALL / REPLY_RECV)·폴트 격리(자식의 폴트와
//! FP/SIMD 트랩은 그 태스크만 종료, 스레드 포인터는 태스크별 격리)의 정상
//! 경로와 거부 경로를 자가 검증합니다. 검증 실패는 EXIT로 즉시 드러나고
//! (fail-secure, 커널이 파킹), 성공하면 양보 루프로 들어갑니다.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};

use k0_abi::{bootinfo, err, ipc, obj, perm, syscall};

/// 재분류 검증에 쓰는 테스트 VA (이미지/스택/bootinfo와 겹치지 않는 구간)
const TEST_VA: u64 = 0x2000_0000;

/// 유한 태스크가 종료 전에 도달해야 하는 카운터 목표값
const CHILD_EXIT_TARGET: u64 = 100_000;

// 두 자식 태스크와 공유하는 진행 카운터 (같은 주소 공간)
static BUSY_COUNTER: AtomicU64 = AtomicU64::new(0);
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

/// 인자 하나짜리 시스템 콜을 수행하는 함수입니다.
///
/// # Arguments
/// `nr` - 시스템 콜 번호(x8)
/// `a0` - 첫 인자(x0), 반환값도 x0
fn sys1(nr: u64, a0: u64) -> u64 {
    let ret;
    // SAFETY: svc는 커널로의 동기 트랩이고 커널이 컨텍스트 전체를 복원함
    unsafe {
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            options(nostack),
        );
    }
    ret
}

/// 인자 세 개짜리 시스템 콜을 수행하는 함수입니다.
///
/// # Arguments
/// `nr` - 시스템 콜 번호(x8)
/// `a0` - 첫 인자(x0), 반환값도 x0
/// `a1` - 둘째 인자(x1)
/// `a2` - 셋째 인자(x2)
fn sys3(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret;
    // SAFETY: svc는 커널로의 동기 트랩이고 커널이 컨텍스트 전체를 복원함
    unsafe {
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            options(nostack),
        );
    }
    ret
}

fn put_str(s: &str) {
    for b in s.bytes() {
        sys1(syscall::DEBUG_PUTC, u64::from(b));
    }
}

fn put_hex(v: u64) {
    put_str("0x");
    for i in (0..16).rev() {
        let nib = ((v >> (i * 4)) & 0xF) as u8;
        let c = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
        sys1(syscall::DEBUG_PUTC, u64::from(c));
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

/// 인자 네 개짜리 시스템 콜을 수행하는 함수입니다.
///
/// # Arguments
/// `nr` - 시스템 콜 번호(x8)
/// `a0` - 첫 인자(x0), 반환값도 x0
/// `a1` - 둘째 인자(x1)
/// `a2` - 셋째 인자(x2)
/// `a3` - 넷째 인자(x3)
fn sys4(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret;
    // SAFETY: svc는 커널로의 동기 트랩이고 커널이 컨텍스트 전체를 복원함
    unsafe {
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
            options(nostack),
        );
    }
    ret
}

fn retype(slot: u64, kind: u64) -> i64 {
    sys3(syscall::RETYPE, slot, kind, 0) as i64
}

fn map(slot: i64, va: u64, p: u64) -> i64 {
    sys3(syscall::MAP, slot as u64, va, p) as i64
}

fn tcb_configure(slot: i64, entry: u64, stack: u64, aspace: u64) -> i64 {
    sys4(syscall::TCB_CONFIGURE, slot as u64, entry, stack, aspace) as i64
}

fn tcb_resume(slot: i64) -> i64 {
    sys1(syscall::TCB_RESUME, slot as u64) as i64
}

/// IPC 반환값 묶음 구조체입니다. (x0 = 상태, x2-x5 = 수신 MR)
struct IpcRet {
    status: i64,
    mr: [u64; 4],
}

/// IPC 시스템 콜(x0-x5 사용)을 수행하는 함수입니다.
///
/// # Arguments
/// `nr` - 시스템 콜 번호(x8)
/// `slot` - 엔드포인트 슬롯(x0)
/// `flags` - 플래그(x1)
/// `mr` - 보낼 MR0-MR3(x2-x5), 수신 계열은 받은 값으로 덮임
fn sys_ipc(nr: u64, slot: u64, flags: u64, mr: [u64; 4]) -> IpcRet {
    let status: u64;
    let (mut m0, mut m1, mut m2, mut m3) = (mr[0], mr[1], mr[2], mr[3]);
    // SAFETY: svc는 커널로의 동기 트랩이고 커널이 컨텍스트 전체를 복원함
    unsafe {
        asm!(
            "svc #0",
            in("x8") nr,
            inlateout("x0") slot => status,
            inlateout("x1") flags => _,
            inlateout("x2") m0,
            inlateout("x3") m1,
            inlateout("x4") m2,
            inlateout("x5") m3,
            options(nostack),
        );
    }
    IpcRet {
        status: status as i64,
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

/// 양보 없이 계속 도는 선점 검증용 자식 태스크의 진입점입니다.
extern "C" fn child_busy_entry() -> ! {
    loop {
        BUSY_COUNTER.fetch_add(1, Ordering::Relaxed);
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
    sys1(syscall::YIELD, 0);
    // SAFETY: 의도된 null 역참조, 데이터 어보트로 이 태스크가 종료돼야 함
    unsafe { asm!("str xzr, [{}]", in(reg) 0u64, options(nostack)) };
    FAULT_SURVIVED.fetch_add(1, Ordering::Relaxed);
    loop {
        sys1(syscall::YIELD, 0);
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
        sys1(syscall::YIELD, 0);
    }
}

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    put_str("root: hello from EL0\n");

    let hdr = bootinfo::VA as *const bootinfo::Header;
    // SAFETY: 커널이 이 VA에 bootinfo 페이지를 RO로 매핑하고 내용을 채웠음
    let (version, frame_size, cap_count) =
        unsafe { ((*hdr).version, (*hdr).frame_size, (*hdr).cap_count) };
    check("bootinfo version", version == bootinfo::VERSION);
    check(
        "bootinfo frame size",
        frame_size > 0 && frame_size.is_power_of_two(),
    );

    // 재분류 원본으로 쓸 넉넉한 untyped와 내 주소 공간 슬롯 탐색
    // SAFETY: 헤더 뒤에 커널이 기록한 cap_count개의 디스크립터가 이어짐
    let descs = unsafe { hdr.add(1) as *const bootinfo::CapDesc };
    let mut ut: u64 = 0;
    let mut aspace: u64 = 0;
    for i in 0..cap_count {
        // SAFETY: 위와 동일, i는 cap_count 미만
        let d = unsafe { &*descs.add(i as usize) };
        if ut == 0 && d.kind == bootinfo::cap_kind::UNTYPED && d.size >= frame_size * 32 {
            ut = i;
        }
        if aspace == 0 && d.kind == bootinfo::cap_kind::ADDR_SPACE {
            aspace = i;
        }
    }
    check("untyped search", ut != 0);
    check("aspace search", aspace != 0);
    put_str("root: untyped slot ");
    put_hex(ut);
    put_str("\n");

    // 거부 경로: null 슬롯 재분류, 미지의 타입
    check("retype null slot", retype(0, obj::FRAME) == err::NOT_UNTYPED);
    check("retype bad type", retype(ut, 99) == err::BAD_TYPE);

    // 중간 테이블이 없는 동안 프레임 매핑은 거부돼야 함
    let f = retype(ut, obj::FRAME);
    check("retype frame", f > 0);
    check("map before pt", map(f, TEST_VA, perm::RW) == err::MISSING_TABLE);

    // 페이지 테이블 재분류와 설치
    let pt = retype(ut, obj::PAGE_TABLE);
    check("retype pt", pt > 0);
    check("map pt", map(pt, TEST_VA, 0) == 0);
    check("map pt twice", map(pt, TEST_VA, 0) == err::ALREADY_MAPPED);

    // 프레임 매핑 거부 경로: null VA, 미지의 권한
    check("map null va", map(f, 0, perm::RW) == err::BAD_VA);
    check("map bad perm", map(f, TEST_VA, 7) == err::BAD_PERM);

    // 정상 매핑 후 실제 읽기/쓰기
    check("map frame", map(f, TEST_VA, perm::RW) == 0);
    let head = TEST_VA as *mut u64;
    let tail = (TEST_VA + frame_size - 8) as *mut u64;
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
        map(f, TEST_VA + frame_size, perm::RW) == err::ALREADY_MAPPED,
    );
    let f2 = retype(ut, obj::FRAME);
    check("retype frame 2", f2 > 0 && f2 != f);
    check("map overlap", map(f2, TEST_VA, perm::RW) == err::OVERLAP);
    check("map frame 2", map(f2, TEST_VA + frame_size, perm::RW) == 0);
    // SAFETY: 위와 동일
    unsafe {
        let q = (TEST_VA + frame_size) as *mut u64;
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
    check("map frame ro", map(f3, TEST_VA + 2 * frame_size, perm::RO) == 0);
    // SAFETY: 방금 RO로 매핑된 전용 프레임, 재분류가 소거를 보장함
    unsafe {
        let r = (TEST_VA + 2 * frame_size) as *const u64;
        check("frame ro zeroed", r.read_volatile() == 0);
    }

    put_str("root: retype/map tests pass\n");

    // 자식 스택 배치: +3, +5 그래뉼은 스택 가드로 비워 둠
    let s1 = retype(ut, obj::FRAME);
    check("retype stack 1", s1 > 0);
    check("map stack 1", map(s1, TEST_VA + 4 * frame_size, perm::RW) == 0);
    let s2 = retype(ut, obj::FRAME);
    check("retype stack 2", s2 > 0);
    check("map stack 2", map(s2, TEST_VA + 6 * frame_size, perm::RW) == 0);
    let sp_busy = TEST_VA + 5 * frame_size;
    let sp_exit = TEST_VA + 7 * frame_size;

    let tcb_busy = retype(ut, obj::TCB);
    check("retype tcb busy", tcb_busy > 0);
    let tcb_exit = retype(ut, obj::TCB);
    check("retype tcb exit", tcb_exit > 0);
    let entry_busy = child_busy_entry as usize as u64;
    let entry_exit = child_exit_entry as usize as u64;

    // 거부 경로: 구성 전 재개, 잘못된 케이퍼빌리티, 스택 정렬 위반
    check("resume unconfigured", tcb_resume(tcb_busy) == err::BAD_STATE);
    check(
        "configure bad tcb",
        tcb_configure(f, entry_busy, sp_busy, aspace) == err::BAD_CAP,
    );
    check(
        "configure bad aspace",
        tcb_configure(tcb_busy, entry_busy, sp_busy, f as u64) == err::BAD_CAP,
    );
    check(
        "configure bad stack",
        tcb_configure(tcb_busy, entry_busy, sp_busy - 8, aspace) == err::BAD_VA,
    );

    // 유한 태스크: 실행 -> 목표 도달 -> EXIT -> 루트로 복귀
    check(
        "configure tcb exit",
        tcb_configure(tcb_exit, entry_exit, sp_exit, aspace) == 0,
    );
    check(
        "configure twice",
        tcb_configure(tcb_exit, entry_exit, sp_exit, aspace) == err::BAD_STATE,
    );
    check("resume tcb exit", tcb_resume(tcb_exit) == 0);
    check("resume twice", tcb_resume(tcb_exit) == err::BAD_STATE);
    while EXIT_COUNTER.load(Ordering::Relaxed) != CHILD_EXIT_TARGET {
        sys1(syscall::YIELD, 0);
    }
    put_str("root: child exit test pass\n");

    // 엔드포인트: 재분류, 커널 오브젝트라 매핑 불가, 빈 큐 비대기 즉시 복귀
    let ep = retype(ut, obj::ENDPOINT);
    check("retype ep", ep > 0);
    check(
        "map ep",
        map(ep, TEST_VA + 10 * frame_size, perm::RW) == err::BAD_CAP,
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
    check("map stack 3", map(s3, TEST_VA + 8 * frame_size, perm::RW) == 0);
    let tcb_srv = retype(ut, obj::TCB);
    check("retype tcb srv", tcb_srv > 0);
    EP_SLOT.store(ep as u64, Ordering::Relaxed);
    check(
        "configure tcb srv",
        tcb_configure(
            tcb_srv,
            server_entry as usize as u64,
            TEST_VA + 9 * frame_size,
            aspace,
        ) == 0,
    );
    check("resume tcb srv", tcb_resume(tcb_srv) == 0);

    // 송신자 선행 랑데부: 서버가 아직 RECV 전이라 루트가 블록됐다 깨어남
    let m1 = [0x1111, 0x2222, 0x3333, 0x4444];
    check("send first", send(ep, 0, m1).status == 0);
    while SRV_COUNT.load(Ordering::Relaxed) < 1 {
        sys1(syscall::YIELD, 0);
    }
    for i in 0..4 {
        check("send first data", SRV_MR[i].load(Ordering::Relaxed) == m1[i]);
    }

    // 수신자 선행 랑데부: 서버가 RECV에서 대기 중일 때 즉시 전달.
    // NONBLOCK은 상대가 준비돼 있으면 즉시 성공해야 함
    let m2 = [0x5555, 0x6666, 0x7777, 0x8888];
    check("send to waiting nb", send(ep, nb, m2).status == 0);
    while SRV_COUNT.load(Ordering::Relaxed) < 2 {
        sys1(syscall::YIELD, 0);
    }
    for i in 0..4 {
        check("send waiting data", SRV_MR[i].load(Ordering::Relaxed) == m2[i]);
    }

    // CALL 왕복: 대기 중인 서버에 전달, 응답은 변환 확인
    let m3 = [0xAAAA, 0xBBBB, 0xCCCC, 0xDDDD];
    let r = call(ep, 0, m3);
    check("call status", r.status == 0);
    check("call reply", r.mr == xform(m3));
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
    check("map stack 4", map(s4, TEST_VA + 11 * frame_size, perm::RW) == 0);
    let s5 = retype(ut, obj::FRAME);
    check("retype stack 5", s5 > 0);
    check("map stack 5", map(s5, TEST_VA + 13 * frame_size, perm::RW) == 0);
    let tcb_cl = retype(ut, obj::TCB);
    let tcb_cl2 = retype(ut, obj::TCB);
    check("retype client tcbs", tcb_cl > 0 && tcb_cl2 > 0);
    check(
        "configure client",
        tcb_configure(
            tcb_cl,
            client_entry as usize as u64,
            TEST_VA + 12 * frame_size,
            aspace,
        ) == 0,
    );
    check(
        "configure client 2",
        tcb_configure(
            tcb_cl2,
            client2_entry as usize as u64,
            TEST_VA + 14 * frame_size,
            aspace,
        ) == 0,
    );
    check("resume client", tcb_resume(tcb_cl) == 0);
    check("resume client 2", tcb_resume(tcb_cl2) == 0);

    // 대기 큐 FIFO: 먼저 줄 선 클라이언트의 메시지가 먼저 나와야 하고,
    // 송신자가 대기 중이면 NONBLOCK RECV도 즉시 성공해야 함
    let mut r1 = recv(ep2, nb);
    while r1.status == err::WOULD_BLOCK {
        sys1(syscall::YIELD, 0);
        r1 = recv(ep2, nb);
    }
    check("fifo first", r1.status == 0 && r1.mr == CM_FIFO_A);
    let mut r2 = recv(ep2, nb);
    while r2.status == err::WOULD_BLOCK {
        sys1(syscall::YIELD, 0);
        r2 = recv(ep2, nb);
    }
    check("fifo second", r2.status == 0 && r2.mr == CM_FIFO_B);

    // 클라이언트의 CALL이 서버에 닿고, 서버가 자격을 쥔 채 RECV로 넘어가길 대기
    while SRV_COUNT.load(Ordering::Relaxed) < 7 {
        sys1(syscall::YIELD, 0);
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
    check("map stack 6", map(s6, TEST_VA + 16 * frame_size, perm::RW) == 0);
    let s7 = retype(ut, obj::FRAME);
    check("retype stack 7", s7 > 0);
    check("map stack 7", map(s7, TEST_VA + 18 * frame_size, perm::RW) == 0);
    let tcb_fault = retype(ut, obj::TCB);
    let tcb_fp = retype(ut, obj::TCB);
    check("retype fault tcbs", tcb_fault > 0 && tcb_fp > 0);
    let entry_fault = child_fault_entry as usize as u64;
    let sp_fault = TEST_VA + 17 * frame_size;
    check(
        "configure misaligned entry",
        tcb_configure(tcb_fault, entry_fault + 2, sp_fault, aspace) == err::BAD_VA,
    );

    // 자식의 폴트는 그 태스크만 종료해야 하고, 자식이 덮어쓴 TPIDR_EL0는
    // 루트에 보이면 안 되며, 자식은 루트의 값이 아닌 0을 봐야 함
    set_tpidr(ROOT_TPIDR);
    check(
        "configure tcb fault",
        tcb_configure(tcb_fault, entry_fault, sp_fault, aspace) == 0,
    );
    check("resume tcb fault", tcb_resume(tcb_fault) == 0);
    while FAULT_STAGE.load(Ordering::Relaxed) < 1 {
        sys1(syscall::YIELD, 0);
    }
    check("child tpidr zeroed", CHILD_SAW_TPIDR.load(Ordering::Relaxed) == 0);
    check("root tpidr isolated", get_tpidr() == ROOT_TPIDR);
    check("root tpidrro zero", get_tpidrro() == 0);
    // 양보하면 자식이 폴트를 내고 커널이 종료시킴, 살아 있으면 카운터가 오름
    for _ in 0..4 {
        sys1(syscall::YIELD, 0);
    }
    check("fault child killed", FAULT_SURVIVED.load(Ordering::Relaxed) == 0);
    check("root tpidr after kill", get_tpidr() == ROOT_TPIDR);
    check("dead tcb resume", tcb_resume(tcb_fault) == err::BAD_STATE);

    // FP/SIMD 명령은 CPACR_EL1 트랩으로 그 태스크만 종료돼야 함
    check(
        "configure tcb fp",
        tcb_configure(
            tcb_fp,
            child_fp_entry as usize as u64,
            TEST_VA + 19 * frame_size,
            aspace,
        ) == 0,
    );
    check("resume tcb fp", tcb_resume(tcb_fp) == 0);
    while FAULT_STAGE.load(Ordering::Relaxed) < 2 {
        sys1(syscall::YIELD, 0);
    }
    for _ in 0..4 {
        sys1(syscall::YIELD, 0);
    }
    check("fp child killed", FAULT_SURVIVED.load(Ordering::Relaxed) == 0);
    set_tpidr(0);
    put_str("root: fault isolation tests pass\n");

    // 무한 태스크: 양보 없이 돌아도 타이머 선점으로 루트가 복귀해야 함
    check(
        "configure tcb busy",
        tcb_configure(tcb_busy, entry_busy, sp_busy, aspace) == 0,
    );
    check("resume tcb busy", tcb_resume(tcb_busy) == 0);
    while BUSY_COUNTER.load(Ordering::Relaxed) == 0 {
        sys1(syscall::YIELD, 0);
    }
    put_str("root: preempt test pass\n");

    put_str("root: sched tests pass\n");
    loop {
        sys1(syscall::YIELD, 0);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let _ = sys1(syscall::EXIT, 1);
    loop {
        core::hint::spin_loop();
    }
}
