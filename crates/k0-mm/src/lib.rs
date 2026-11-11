//! 물리/가상 메모리 관리 크레이트입니다.
//!
//! # Features
//! 진입 페이즈 1의 초기 페이지 테이블 구성(W^X)과 MMU 활성화, 진입 페이즈 3의
//! 부트 프레임 할당자와 사용자 주소 공간(TTBR0) 구성을 담당합니다.
//! 그래뉼은 빌드 feature로 고릅니다(granule-4k 기본, granule-16k).

#![cfg_attr(not(test), no_std)]

#[cfg(all(feature = "granule-4k", feature = "granule-16k"))]
compile_error!("granule-4k와 granule-16k는 동시에 활성화 할 수 없음");

#[cfg(not(any(feature = "granule-4k", feature = "granule-16k")))]
compile_error!("그래뉼 feature 없음(granule-4k 또는 granule-16k 중 하나만 활성화 할 것)");

pub mod paging;
pub mod user;

pub use paging::{
    alias_object, can_read, can_write, enable_paging, map_kernel_window, null_user_root, pa_bits,
    table_pool_usage, KernelLayout, Mmu, MmuError, GRANULE, KERNEL_VA_OFFSET,
};
pub use user::{
    addr_spaces, can_read_pan_checked, can_user_read, can_user_write, current_user_root,
    detach_user_root, flush_user_tlb, install_user_ttbr0, user_install_table, user_map_frame,
    user_uninstall_table, user_unmap_frame, zero_table, AddrSpaces, FrameAlloc, UserPerm,
    UserSpace, MAX_ADDR_SPACES,
};
