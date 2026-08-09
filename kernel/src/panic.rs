//! fail-secure panic 핸들러 모듈입니다.
//!
//! # Features
//! 고보안 시스템의 panic은 인터럽트 마스크 -> (필요 시 민감 상태 zeroize) ->
//! 정지로 동작합니다. 디버그 빌드에서만 earlycon으로 위치와 메시지를 출력하고,
//! 프로덕션 프로파일(release)에서는 어떤 정보도 출력하지 않습니다.

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // SAFETY: 모든 예외 마스크는 fail-secure 정지의 전제 조건
    unsafe { core::arch::asm!("msr DAIFSet, #0xf", options(nomem, nostack)) };

    #[cfg(debug_assertions)]
    {
        use core::fmt::Write;
        let mut con = k0_arch::earlycon::EarlyCon;
        let _ = writeln!(con, "k0: PANIC {info}");
    }
    #[cfg(not(debug_assertions))]
    let _ = info;

    park()
}

/// 저전력 파킹 루프로 영원히 대기하는 함수입니다.
#[inline(always)]
pub(crate) fn park() -> ! {
    loop {
        // SAFETY: wfe는 대기만 함
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}
