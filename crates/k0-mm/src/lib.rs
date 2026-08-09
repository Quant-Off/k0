//! 물리/가상 메모리 관리 크레이트입니다.
//!
//! # Features
//! 진입 페이즈 1의 초기 페이지 테이블 구성(W^X)과 MMU 활성화를 담당합니다.
//! 그래뉼은 plat-virt에서 4KiB, plat-apple에서 16KiB로 갈립니다.

// TODO: untyped 물리 메모리 관리 추가

#![no_std]

#[cfg(all(feature = "plat-virt", feature = "plat-apple"))]
compile_error!("plat-virt와 plat-apple은 동시에 활성화 할 수 없음");

#[cfg(not(any(feature = "plat-virt", feature = "plat-apple")))]
compile_error!("플랫폼 feature 없음(plat-virt 또는 plat-apple 중 하나만 활성화 할 것)");

pub mod paging;

pub use paging::{
    can_read, can_write, enable_paging, KernelLayout, Mmu, MmuError, GRANULE, KERNEL_VA_OFFSET,
};
