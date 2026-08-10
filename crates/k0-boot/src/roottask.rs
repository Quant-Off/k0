//! 루트 태스크 이미지 임베드와 무결성 검증 모듈입니다. (진입 페이즈 1 + 3)
//!
//! # Features
//! build.rs가 root-task ELF를 호스트에서 파싱해 flat 이미지와 세그먼트
//! 메타데이터로 변환하고, flat 이미지의 SHA-256 기준 해시와 함께 커널
//! rodata에 굽습니다. 커널은 ELF를 전혀 해석하지 않습니다(W^X와 배치 계약은
//! 빌드 시점에 검사됨). 부팅 시 임베드된 이미지의 해시를 다시 계산해
//! 기준과 다르면 거부합니다. 무결성의 뿌리는 부트 체인이 보증하는 커널
//! 이미지 자체이며(메타데이터도 rodata의 일부), 커널 재빌드 없이 루트
//! 태스크를 교체할 수 있는 비대칭 서명(예: Ed25519 + vendored 구현)은
//! 설계된 확장 지점입니다.
//!
//! # Errors
//! 해시 불일치는 이미지 변조나 적재 오류를 뜻하므로 호출자는 부팅을
//! 중단해야 합니다(fail-secure).

use sha2::{Digest, Sha256};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/roottask_gen.rs"));
}

/// flat 이미지 안의 적재 세그먼트 하나를 나타내는 구조체입니다.
///
/// `va`는 사용자 VA고 이미지 안의 위치는 `va - base`입니다. bss는 flat
/// 변환이 이미 0으로 채워 뒀기 때문에 filesz 구분이 없습니다.
#[derive(Clone, Copy, Debug)]
pub struct RtSegment {
    pub va: u64,
    pub memsz: u64,
    pub kind: RtSegKind,
}

/// 세그먼트 권한 종류를 나타내는 열거형입니다. (빌드 시점에 W^X 검사 완료)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtSegKind {
    Text,
    Ro,
    Rw,
}

/// 무결성 검증을 통과한 루트 태스크 이미지를 담는 구조체입니다.
///
/// 진입 페이즈 3의 태스크 생성이 이 토큰을 입력으로 받습니다.
pub struct VerifiedRootTask {
    pub image: &'static [u8],
    pub sha256: [u8; 32],
    pub base: u64,
    pub entry: u64,
    pub segments: &'static [RtSegment],
}

/// 루트 태스크 검증이 거부된 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    HashMismatch,
}

/// 임베드된 루트 태스크 이미지의 무결성을 검증하는 함수입니다.
///
/// # Errors
/// 재계산한 SHA-256이 빌드 시 고정된 기준 해시와 다르면 `HashMismatch`
pub fn verify_root_task() -> Result<VerifiedRootTask, VerifyError> {
    let hash: [u8; 32] = Sha256::digest(generated::ROOT_TASK_IMAGE).into();
    if hash != generated::ROOT_TASK_SHA256 {
        return Err(VerifyError::HashMismatch);
    }
    Ok(VerifiedRootTask {
        image: generated::ROOT_TASK_IMAGE,
        sha256: hash,
        base: generated::ROOT_TASK_BASE,
        entry: generated::ROOT_TASK_ENTRY,
        segments: generated::ROOT_TASK_SEGMENTS,
    })
}
