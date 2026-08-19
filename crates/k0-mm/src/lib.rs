//! 물리/가상 메모리 관리 크레이트입니다.
//!
//! # Features
//! 진입 페이즈 1의 초기 페이지 테이블 구성(W^X)과 MMU 활성화, 진입 페이즈 3의
//! 부트 프레임 할당자와 사용자 주소 공간(TTBR0) 구성을 담당합니다.
//! 그래뉼은 plat-virt에서 4KiB, plat-apple에서 16KiB로 갈립니다.

#![no_std]

#[cfg(all(feature = "plat-virt", feature = "plat-apple"))]
compile_error!("plat-virt와 plat-apple은 동시에 활성화 할 수 없음");

#[cfg(not(any(feature = "plat-virt", feature = "plat-apple")))]
compile_error!("플랫폼 feature 없음(plat-virt 또는 plat-apple 중 하나만 활성화 할 것)");

pub mod paging;
pub mod user;

pub use paging::{
    can_read, can_write, enable_paging, map_kernel_window, KernelLayout, Mmu, MmuError, GRANULE,
    KERNEL_VA_OFFSET,
};
pub use user::{
    can_read_pan_checked, can_user_read, can_user_write, current_user_root, install_user_ttbr0,
    user_install_table, user_map_frame, FrameAlloc, UserPerm, UserSpace,
};
