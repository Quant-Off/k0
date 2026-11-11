//! 부트 정보 처리 크레이트입니다.
//!
//! # Features
//! DTB를 파싱해 물리 메모리 맵을 확보하고, 커널이 직접 다루는 디바이스
//! (인터럽트 컨트롤러, 타이머, 콘솔)를 찾아 플랫폼 기술서로 만들며, 커널
//! 이미지에 함께 구운 루트 태스크 이미지의 무결성(SHA-256 고정 해시)을
//! 검증합니다.
//!
//! 커널 밖의 UEFI 로더도 이 크레이트의 파서와 검증을 그대로 써서, 커널이
//! 받아들일 DTB인지 넘어가기 전에 판정합니다. 로더는 루트 태스크가 필요
//! 없으므로 기본 feature `roottask`를 끄고 씁니다. feature `test-support`는
//! 테스트용 DTB 빌더와 퍼즈 도우미를 다른 크레이트의 테스트에 내줍니다.

#![cfg_attr(not(any(test, feature = "test-support")), no_std)]

mod entropy;
mod fdt;
mod platform;
#[cfg(feature = "roottask")]
mod roottask;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
mod window;

pub use entropy::derive_pac_keys;
pub use fdt::walk::{be32, be64, read_cells, Cursor, Fdt, Token};
pub use fdt::{
    dtb_span, parse, parse_blob, BootError, BootInfo, MemRegion, MAX_DTB_SIZE, MAX_ENTROPY, MAX_MEM_REGIONS,
    MAX_RSV_REGIONS,
};
pub use platform::{
    discover, discover_blob, discover_console, Console, ConsoleKind, DiscoverError, Discovery, Intc, Layout,
    NoConsole, Platform, Region, Timer, ValidateError, MAX_REDIST_REGIONS,
};
#[cfg(feature = "roottask")]
pub use roottask::{verify_root_task, RtSegKind, RtSegment, VerifiedRootTask, VerifyError};
pub use window::{boot_window, BOOT_WINDOW_SIZE};

/// 이 빌드에 루트 태스크 이미지가 들어 있는지 여부입니다. 루트 태스크를 싣지
/// 않아야 하는 크레이트(UEFI 로더)가 컴파일 시점에 확인하는 데 씁니다.
pub const HAS_ROOTTASK: bool = cfg!(feature = "roottask");
