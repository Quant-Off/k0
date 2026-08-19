//! 진입 페이즈 3의 루트 태스크(초기 사용자 공간 서버)입니다.
//!
//! # Features
//! 커널이 무결성 검증 후 EL0로 띄우는 첫 태스크입니다. bootinfo 페이지에서
//! 케이퍼빌리티 목록을 읽고, 재분류(RETYPE)와 매핑(MAP) 시스템 콜의 정상
//! 경로와 거부 경로를 자가 검증합니다. 검증 실패는 EXIT로 즉시 드러나고
//! (fail-secure, 커널이 파킹), 성공하면 양보 루프로 들어갑니다.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

use k0_abi::{bootinfo, err, obj, perm, syscall};

/// 재분류 검증에 쓰는 테스트 VA (이미지/스택/bootinfo와 겹치지 않는 구간)
const TEST_VA: u64 = 0x2000_0000;

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
    loop {}
}

fn check(step: &str, ok: bool) {
    if !ok {
        fail(step)
    }
}

fn retype(slot: u64, kind: u64) -> i64 {
    sys3(syscall::RETYPE, slot, kind, 0) as i64
}

fn map(slot: i64, va: u64, p: u64) -> i64 {
    sys3(syscall::MAP, slot as u64, va, p) as i64
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

    // 재분류 원본으로 쓸 넉넉한 untyped 슬롯 탐색
    // SAFETY: 헤더 뒤에 커널이 기록한 cap_count개의 디스크립터가 이어짐
    let descs = unsafe { hdr.add(1) as *const bootinfo::CapDesc };
    let mut ut: u64 = 0;
    for i in 0..cap_count {
        // SAFETY: 위와 동일, i는 cap_count 미만
        let d = unsafe { &*descs.add(i as usize) };
        if d.kind == bootinfo::cap_kind::UNTYPED && d.size >= frame_size * 8 {
            ut = i;
            break;
        }
    }
    check("untyped search", ut != 0);
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
    loop {
        sys1(syscall::YIELD, 0);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let _ = sys1(syscall::EXIT, 1);
    loop {}
}
