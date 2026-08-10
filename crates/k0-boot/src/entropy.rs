//! PAC 부트 키 파생 모듈입니다. (진입 페이즈 2)
//!
//! # Features
//! 여러 엔트로피 재료(DTB `/chosen`의 rng-seed/kaslr-seed, CNTPCT, RNDR)를
//! SHA-256 카운터 확장으로 혼합해 PAC 키 10워드(IA/IB/DA/DB 쌍 8개 + GA 쌍
//! 2개)를 만듭니다. 해시 혼합이라서 재료 일부가 악의적이거나 예측 가능해도
//! 나머지 재료의 엔트로피는 보존됩니다. 다만 전체 품질은 가장 좋은 재료를
//! 넘지 못하므로, 재료가 CNTPCT뿐이면 저엔트로피이고 호출자는 이를 로그로
//! 드러내야 합니다.

use sha2::{Digest, Sha256};

/// 엔트로피 재료를 혼합해 PAC 키 10워드를 파생하는 함수입니다.
///
/// 도메인 문자열과 블록 카운터를 넣은 SHA-256 카운터 확장(CTR)이라 같은
/// 재료라도 블록마다 다른 출력이 나오고, 워드 사이의 상관은 SHA-256의
/// 안전성에 귀속됩니다.
///
/// # Arguments
/// `dtb_entropy` - /chosen에서 수집한 바이트(비어 있을 수 있음)
/// `extra` - 아키텍처 재료(CNTPCT, RNDR 값 등)
pub fn derive_pac_keys(dtb_entropy: &[u8], extra: &[u64]) -> [u64; 10] {
    let mut keys = [0u64; 10];
    let mut i = 0;
    let mut counter: u8 = 0;
    while i < keys.len() {
        let mut h = Sha256::new();
        h.update(b"k0-pac-boot-key-v1");
        h.update([counter]);
        h.update((dtb_entropy.len() as u64).to_le_bytes());
        h.update(dtb_entropy);
        for w in extra {
            h.update(w.to_le_bytes());
        }
        let block = h.finalize();
        for chunk in block.chunks_exact(8) {
            if i == keys.len() {
                break;
            }
            keys[i] = u64::from_le_bytes(chunk.try_into().unwrap());
            i += 1;
        }
        counter += 1;
    }
    keys
}
