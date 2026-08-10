//! 루트 태스크 임베드 준비 모듈입니다.
//!
//! # Features
//! root-task를 별도 타겟 디렉터리에서 빌드하고, 그 바이너리의 SHA-256을
//! 계산해 임베드 경로와 기준 해시를 담은 생성 파일(roottask_gen.rs)을
//! 만듭니다. 시작 시 SHA-256 구현 자체를 표준 테스트 벡터로 검증하므로
//! 구현 오류는 빌드 실패로 즉시 드러납니다.

#[path = "src/sha256.rs"]
mod sha256;

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const ABC_DIGEST: [u8; 32] = [
    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
    0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
    0x15, 0xad,
];
const EMPTY_DIGEST: [u8; 32] = [
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
    0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
    0xb8, 0x55,
];

fn main() {
    // FIPS 180-4 테스트 벡터로 SHA-256 구현 자가 검증
    assert_eq!(sha256::digest(b"abc"), ABC_DIGEST, "SHA-256 구현 오류");
    assert_eq!(sha256::digest(b""), EMPTY_DIGEST, "SHA-256 구현 오류");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest.parent().unwrap().parent().unwrap();

    // 바깥 cargo와의 락 충돌을 피하려고 전용 타겟 디렉터리에서 빌드
    let rt_target = out_dir.join("root-task-target");
    let cargo = env::var("CARGO").unwrap();
    let status = Command::new(&cargo)
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", &rt_target)
        .args([
            "build",
            "-p",
            "root-task",
            "--release",
            "--target",
            "aarch64-unknown-none-softfloat",
        ])
        .status()
        .expect("root-task 빌드 실행 실패");
    assert!(status.success(), "root-task 빌드 실패");

    let elf = rt_target.join("aarch64-unknown-none-softfloat/release/root-task");
    let image = fs::read(&elf).expect("root-task 바이너리 읽기 실패");
    let hash = sha256::digest(&image);

    let generated = format!(
        "pub static ROOT_TASK_IMAGE: &[u8] = include_bytes!(\"{}\");\n\
         pub const ROOT_TASK_SHA256: [u8; 32] = {:?};\n",
        elf.display(),
        hash
    );
    fs::write(out_dir.join("roottask_gen.rs"), generated).unwrap();

    println!(
        "cargo::rerun-if-changed={}",
        workspace.join("userspace/root-task/src/main.rs").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        workspace.join("userspace/root-task/Cargo.toml").display()
    );
}
