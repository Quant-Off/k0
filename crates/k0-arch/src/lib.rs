//! 아키텍처(AArch64) 종속 계층 크레이트입니다.
//!
//! # Features
//! earlycon(진입 페이즈 1), 예외 벡터 설치와 GIC/타이머 초기화, PAC/BTI
//! (진입 페이즈 2), EL0 진입/복귀와 시스템 콜 트랩(진입 페이즈 3)을
//! 담당합니다. 플랫폼별 MMIO 주소는 plat-virt / plat-apple feature로
//! 갈라집니다.

#![no_std]

#[cfg(all(feature = "plat-virt", feature = "plat-apple"))]
compile_error!("plat-virt와 plat-apple은 동시에 활성화 할 수 없음");

#[cfg(not(any(feature = "plat-virt", feature = "plat-apple")))]
compile_error!("플랫폼 feature 없음(plat-virt 또는 plat-apple 중 하나만 활성화할 것)");

pub mod earlycon;
pub mod hardening;
pub mod irq;
pub mod usermode;
pub mod vectors;
