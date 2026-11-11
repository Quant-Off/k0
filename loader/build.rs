//! 로더에 내장할 커널 Image를 준비하는 빌드 스크립트입니다.
//!
//! # Features
//! 환경 변수 `K0_KERNEL_IMAGE`가 가리키는 raw Image를 읽어 헤더를 확인하고,
//! 사본과 SHA-256 기준 해시를 생성 파일에 굽습니다. 커널을 여기서 빌드하지
//! 않습니다. 부팅 매트릭스가 시험하는 바로 그 커널 바이너리를 내장하기
//! 위해서이고, 바깥 빌드의 플래그가 커널 빌드로 새지 않게 하기 위해서입니다.
//! UEFI가 아닌 타겟으로 빌드하면(워크스페이스 전체 검사 등) 빈 자리표시만
//! 만듭니다.

use sha2::{Digest, Sha256};

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=K0_KERNEL_IMAGE");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let generated = out_dir.join("kernel_gen.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("uefi") {
        fs::write(&generated, "").unwrap();
        return;
    }

    let path = env::var("K0_KERNEL_IMAGE")
        .expect("K0_KERNEL_IMAGE에 커널 raw Image의 절대 경로가 필요함 (tools/build-loader.sh 참고)");
    let path = PathBuf::from(path);
    assert!(path.is_absolute(), "K0_KERNEL_IMAGE는 절대 경로여야 함");
    println!("cargo::rerun-if-changed={}", path.display());

    let image = fs::read(&path).expect("커널 Image 읽기 실패");
    let info = k0_efi::image::check_image(&image).unwrap_or_else(|e| panic!("커널 Image 헤더 거부: {e:?}"));
    let hash: [u8; 32] = Sha256::digest(&image).into();

    // 해시를 계산한 바이트를 그대로 내장하도록 사본을 둠
    let copy = out_dir.join("kernel.img");
    fs::write(&copy, &image).unwrap();
    let text = format!(
        "pub static KERNEL_IMAGE: &[u8] = include_bytes!(\"{}\");\n\
         pub const KERNEL_SHA256: [u8; 32] = {:?};\n\
         pub const KERNEL_IMAGE_SIZE: u64 = {:#x};\n\
         pub const KERNEL_GRANULE: u64 = {};\n",
        copy.display(),
        hash,
        info.image_size,
        info.granule
    );
    fs::write(&generated, text).unwrap();
}
