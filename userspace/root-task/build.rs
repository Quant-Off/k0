//! 루트 태스크 링커 스크립트 주입 모듈입니다.
//!
//! # Features
//! `user.ld`를 절대 경로로 링커에 전달합니다. 배치(고정 VA, 16K 정렬,
//! W^X 세그먼트 분리)는 k0-boot의 flat 변환이 전제하는 계약입니다.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let script = manifest.join("user.ld");
    println!("cargo::rustc-link-arg=-T{}", script.display());
    println!("cargo::rerun-if-changed={}", script.display());
}
