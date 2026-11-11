//! 부트 정보 처리 크레이트입니다.
//!
//! # Features
//! DTB를 파싱해 물리 메모리 맵을 확보하고, 커널이 직접 다루는 디바이스
//! (인터럽트 컨트롤러, 타이머, 콘솔)를 찾아 플랫폼 기술서로 만들며, 커널
//! 이미지에 함께 구운 루트 태스크 이미지의 무결성(SHA-256 고정 해시)을
//! 검증합니다.

#![cfg_attr(not(test), no_std)]

mod entropy;
mod fdt;
mod platform;
mod roottask;

pub use entropy::derive_pac_keys;
pub use fdt::{
    dtb_span, parse, BootError, BootInfo, MemRegion, MAX_DTB_SIZE, MAX_ENTROPY, MAX_MEM_REGIONS,
    MAX_RSV_REGIONS,
};
pub use platform::{
    discover, discover_console, Console, ConsoleKind, DiscoverError, Discovery, Intc, Layout, NoConsole, Platform,
    Region, Timer, ValidateError, MAX_REDIST_REGIONS,
};
pub use roottask::{verify_root_task, RtSegKind, RtSegment, VerifiedRootTask, VerifyError};
