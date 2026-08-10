//! 진입 페이즈 3의 루트 태스크(초기 사용자 공간 서버)입니다.
//!
//! # Features
//! 커널이 무결성 검증 후 EL0로 띄우는 첫 태스크입니다. 지금은 시스템 콜
//! 경로 검증용으로 인사를 출력하고 양보 루프를 돕니다. 케이퍼빌리티
//! 수신(untyped 재분류)과 서버 적재는 다음 슬라이스입니다.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

use k0_abi::syscall;

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

fn put_str(s: &str) {
    for b in s.bytes() {
        sys1(syscall::DEBUG_PUTC, u64::from(b));
    }
}

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    put_str("root: hello from EL0\n");
    loop {
        sys1(syscall::YIELD, 0);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let _ = sys1(syscall::EXIT, 1);
    loop {}
}
