//! 아키텍처(AArch64) 종속 계층 크레이트입니다.
//!
//! # Features
//! earlycon(진입 페이즈 1), 예외 벡터 설치와 GIC/타이머 초기화, PAC/BTI
//! (진입 페이즈 2), EL0 진입/복귀와 시스템 콜 트랩(진입 페이즈 3)을
//! 담당합니다.

#![no_std]

#[cfg(feature = "console-input")]
pub mod conin;
pub mod earlycon;
pub mod hardening;
pub mod irq;
pub mod usermode;
pub mod vectors;

/// 커널 불변식 위반을 fail-secure로 정지시키는 함수입니다. 복귀하지 않습니다.
///
/// 커널 전용 메모리의 자료구조(케이퍼빌리티 슬롯, 큐 링크, 사용자 페이지
/// 테이블)가 있을 수 없는 상태일 때 부릅니다. 계속 실행하면 손상된 상태로
/// 권한을 판단하게 되므로 모든 예외를 마스크하고 원인만 출력한 뒤
/// 정지합니다.
///
/// # Arguments
/// `what` - 위반된 불변식의 짧은 이름
pub fn fatal(what: &str) -> ! {
    // SAFETY: 모든 예외 마스크는 fail-secure 정지의 전제 조건
    unsafe { core::arch::asm!("msr DAIFSet, #0xf", options(nomem, nostack)) };
    let mut con = earlycon::EarlyCon;
    con.put_str("k0: FATAL ");
    con.put_str(what);
    con.put_str("\n");
    loop {
        // SAFETY: wfe는 대기만 함
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}
