//! 아키텍처(AArch64) 종속 계층 크레이트입니다.
//!
//! # Features
//! 진입 페이즈 1의 earlycon을 제공하며, 이후 예외 벡터 설치와 GIC 초기화
//! (진입 페이즈 2)가 여기에 추가됩니다. 플랫폼별 MMIO 주소는
//! plat-virt / plat-apple feature로 갈라집니다.

#![no_std]

#[cfg(all(feature = "plat-virt", feature = "plat-apple"))]
compile_error!("plat-virt와 plat-apple은 동시에 켤 수 없음");

#[cfg(not(any(feature = "plat-virt", feature = "plat-apple")))]
compile_error!("플랫폼 feature 없음(plat-virt 또는 plat-apple 중 하나만 활성화할 것)");

pub mod earlycon;
pub mod hardening;
pub mod irq;
pub mod vectors;
