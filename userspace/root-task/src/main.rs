//! 루트 태스크(초기 사용자 공간 서버) stub입니다.

// TODO: 진입 페이즈 3에서 커널이 서명 검증 후 적재(지금은 자리만 잡은거).

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
