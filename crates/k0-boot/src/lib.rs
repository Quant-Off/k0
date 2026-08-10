//! 부트 정보 처리 크레이트입니다.
//!
//! # Features
//! DTB를 파싱해 물리 메모리 맵을 확보하고, 커널 이미지에 함께 구운 루트
//! 태스크 이미지의 무결성(SHA-256 고정 해시)을 검증합니다.

#![no_std]

mod fdt;
mod roottask;

pub use fdt::{dtb_span, parse, BootError, BootInfo, MemRegion, MAX_MEM_REGIONS};
pub use roottask::{verify_root_task, RtSegKind, RtSegment, VerifiedRootTask, VerifyError};
