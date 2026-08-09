//! 부트 정보 처리 크레이트입니다.
//!
//! # Features
//! DTB를 파싱해 물리 메모리 맵을 확보합니다.

// TODO: 루트 태스크 이미지 서명 검증 추가

#![no_std]

mod fdt;

pub use fdt::{parse, BootError, BootInfo, MemRegion, MAX_MEM_REGIONS};
