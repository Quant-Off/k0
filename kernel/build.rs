//! 플랫폼별 링커 스크립트 선택 모듈입니다.
//!
//! # Features
//! virt와 Apple Silicon은 같은 타겟 트리플(aarch64-unknown-none-softfloat)을
//! 공유하므로 `.cargo/config.toml`의 `[target.*]` rustflags로는 둘을 구분할 수
//! 없습니다. 대신 플랫폼 feature(plat-virt / plat-apple)를 검사해 링커 스크립트를
//! 절대 경로로 주입합니다.
//!
//! # Errors
//! feature가 둘 다 켜지거나 둘 다 꺼진 구성은 잘못된 이미지가 조용히 만들어
//! 지는 대신 빌드가 즉시 실패합니다(fail-secure).

use std::env;
use std::path::PathBuf;

fn main() {
    let virt = env::var_os("CARGO_FEATURE_PLAT_VIRT").is_some();
    let apple = env::var_os("CARGO_FEATURE_PLAT_APPLE").is_some();

    let script = match (virt, apple) {
        (true, false) => "virt.ld",
        (false, true) => "apple.ld",
        (true, true) => {
            panic!("plat-virt와 plat-apple은 동시에 활성화 할 수 없음(--no-default-features 필요)")
        }
        (false, false) => panic!("플랫폼 feature 없음(plat-virt 또는 plat-apple 중 하나만 활성화 할 것)"),
    };

    let path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join(script);
    println!("cargo::rustc-link-arg=-T{}", path.display());
    println!("cargo::rerun-if-changed={}", path.display());
}
