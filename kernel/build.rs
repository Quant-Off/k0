//! 그래뉼에 맞춘 링커 스크립트를 생성하는 모듈입니다.
//!
//! # Features
//! 섹션 경계와 가드 페이지의 정렬은 변환 그래뉼을 따라야 합니다. 그래뉼
//! feature(granule-4k / granule-16k)를 검사해 `kernel.ld` 앞에 그래뉼 상수를
//! 붙인 스크립트를 OUT_DIR에 만들고 절대 경로로 링커에 넘깁니다.
//!
//! # Errors
//! feature가 둘 다 켜지거나 둘 다 꺼진 구성은 잘못된 이미지가 조용히 만들어
//! 지는 대신 빌드가 즉시 실패합니다(fail-secure).

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let g4 = env::var_os("CARGO_FEATURE_GRANULE_4K").is_some();
    let g16 = env::var_os("CARGO_FEATURE_GRANULE_16K").is_some();

    // (그래뉼 크기, Image 헤더 flags: 리틀 엔디언 + 페이지 크기 힌트)
    let (granule, flags) = match (g4, g16) {
        (true, false) => (0x1000u64, 2u64),
        (false, true) => (0x4000, 4),
        (true, true) => {
            panic!("granule-4k와 granule-16k는 동시에 활성화 할 수 없음(--no-default-features 필요)")
        }
        (false, false) => {
            panic!("그래뉼 feature 없음(granule-4k 또는 granule-16k 중 하나만 활성화 할 것)")
        }
    };

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let template = manifest.join("kernel.ld");
    let body = fs::read_to_string(&template).expect("kernel.ld 읽기 실패");
    let script = format!("__granule = {granule:#x};\n__image_flags = {flags};\n{body}");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("kernel.ld");
    fs::write(&out, script).expect("링커 스크립트 쓰기 실패");

    println!("cargo::rustc-link-arg=-T{}", out.display());
    println!("cargo::rerun-if-changed={}", template.display());
}
