//! 루트 태스크 이미지 임베드와 무결성 검증 모듈입니다. (진입 페이즈 1)
//!
//! # Features
//! build.rs가 root-task를 빌드해 이미지와 SHA-256 기준 해시를 커널 이미지의
//! rodata에 함께 굽습니다. 부팅 시 임베드된 이미지의 해시를 다시 계산해
//! 기준과 다르면 거부합니다. 무결성의 뿌리는 부트 체인이 보증하는 커널
//! 이미지 자체이며, 커널 재빌드 없이 루트 태스크를 교체할 수 있는 비대칭
//! 서명(예: Ed25519 + vendored 구현)은 설계된 확장 지점입니다.
//!
//! # Errors
//! 해시 불일치는 이미지 변조나 적재 오류를 뜻하므로 호출자는 부팅을
//! 중단해야 합니다(fail-secure).

use crate::sha256;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/roottask_gen.rs"));
}

/// 무결성 검증을 통과한 루트 태스크 이미지를 담는 구조체입니다.
///
/// 진입 페이즈 3의 태스크 생성이 이 토큰을 입력으로 받게 됩니다.
pub struct VerifiedRootTask {
    pub image: &'static [u8],
    pub sha256: [u8; 32],
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
    let hash = sha256::digest(generated::ROOT_TASK_IMAGE);
    if hash != generated::ROOT_TASK_SHA256 {
        return Err(VerifyError::HashMismatch);
    }
    Ok(VerifiedRootTask {
        image: generated::ROOT_TASK_IMAGE,
        sha256: hash,
    })
}
