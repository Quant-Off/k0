//! 암호 구현을 감싼 경계 모듈입니다.
//!
//! # Features
//! 볼트의 나머지 코드는 이 모듈의 함수로만 암호 연산을 합니다. 경계는 암호
//! 모듈에 흔히 요구되는 것을 갖춥니다. 시작 시 기지 답 자가 시험([self_test]),
//! 밖에서 받은 시드로 하는 키 생성과 그 직후의 쌍 일치 시험([keygen]), 키를
//! 내보내는 함수의 부재, 명시적 소거([Key::wipe])입니다. 그렇다고 인증된
//! 모듈이 되는 것은 아닙니다.
//!
//! feature `ed25519`가 없으면 알고리즘이 하나도 없는 경계가 되어 서명과
//! 공개키 요청이 전부 거부됩니다.

/// 공개키의 길이
pub const PUBLIC_KEY_LEN: usize = 32;
/// 서명의 길이
pub const SIGNATURE_LEN: usize = 64;

#[cfg(feature = "ed25519")]
mod imp {
    use super::{PUBLIC_KEY_LEN, SIGNATURE_LEN};
    use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
    use zeroize::Zeroize;

    /// 이 이미지에 Ed25519가 들어 있음
    pub const HAS_ED25519: bool = true;

    /// 볼트가 쥔 서명 키 구조체입니다. 비밀을 꺼내는 길은 없습니다.
    pub struct Key(Option<SigningKey>);

    impl Key {
        /// 키가 없는 상태를 만드는 함수입니다.
        pub const fn empty() -> Self {
            Key(None)
        }

        /// 키를 지우는 함수입니다. `SigningKey`는 버려질 때 비밀을 소거합니다.
        pub fn wipe(&mut self) {
            self.0 = None;
        }

        /// 공개키를 주는 함수입니다. 키가 없으면 `None`
        pub fn public(&self) -> Option<[u8; PUBLIC_KEY_LEN]> {
            self.0.as_ref().map(|k| k.verifying_key().to_bytes())
        }

        /// 메시지에 서명하는 함수입니다. 키가 없으면 `None`
        pub fn sign(&self, msg: &[u8]) -> Option<[u8; SIGNATURE_LEN]> {
            self.0.as_ref().map(|k| k.sign(msg).to_bytes())
        }
    }

    /// 시드로 키를 만드는 함수입니다.
    ///
    /// 만든 직후 고정 메시지에 서명하고 그 공개키로 검증합니다(쌍 일치 시험).
    /// 실패하면 키를 지우고 `None`을 줍니다. 받은 시드 사본은 지웁니다.
    ///
    /// # Arguments
    /// `seed` - 비밀 시드 32바이트(호출 뒤 0으로 덮임)
    pub fn keygen(seed: &mut [u8; 32]) -> Option<Key> {
        let key = SigningKey::from_bytes(seed);
        seed.zeroize();
        let probe = b"k0 vault pairwise consistency test";
        let sig = key.sign(probe);
        if key.verifying_key().verify_strict(probe, &sig).is_err() {
            return None;
        }
        Some(Key(Some(key)))
    }

    /// 서명을 검증하는 함수입니다. 비밀을 다루지 않으므로 클라이언트도 씁니다.
    ///
    /// 약한 공개키와 표준형이 아닌 서명을 거부하는 엄격한 검증입니다.
    pub fn verify(public: &[u8; PUBLIC_KEY_LEN], msg: &[u8], sig: &[u8; SIGNATURE_LEN]) -> bool {
        match VerifyingKey::from_bytes(public) {
            Ok(k) => k.verify_strict(msg, &Signature::from_bytes(sig)).is_ok(),
            Err(_) => false,
        }
    }

    /// RFC 8032 7.1절의 시험 벡터 하나
    struct Kat {
        seed: [u8; 32],
        public: [u8; 32],
        msg: &'static [u8],
        sig: [u8; 64],
    }

    const KATS: [Kat; 2] = [
        Kat {
            seed: hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"),
            public: hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
            msg: &[],
            sig: hex(
                "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
                 5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
            ),
        },
        Kat {
            seed: hex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb"),
            public: hex("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"),
            msg: &[0x72],
            sig: hex(
                "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
                 085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
            ),
        },
    ];

    /// 16진 문자열을 바이트 배열로 바꾸는 함수입니다(상수 문맥 전용). 공백은
    /// 건너뜁니다.
    const fn hex<const N: usize>(s: &str) -> [u8; N] {
        let b = s.as_bytes();
        let mut out = [0u8; N];
        let (mut i, mut n) = (0, 0);
        while i < b.len() {
            let c = b[i];
            i += 1;
            let v = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                _ => continue,
            };
            out[n / 2] |= v << if n % 2 == 0 { 4 } else { 0 };
            n += 1;
        }
        assert!(n == 2 * N);
        out
    }

    /// 기지 답 자가 시험 함수입니다.
    ///
    /// 시험 벡터의 시드에서 공개키와 서명이 그대로 나오는지, 그 서명이
    /// 검증되는지, 한 비트를 바꾼 메시지와 서명은 거부되는지 봅니다.
    pub fn self_test() -> bool {
        for kat in &KATS {
            let key = SigningKey::from_bytes(&kat.seed);
            if key.verifying_key().to_bytes() != kat.public {
                return false;
            }
            if key.sign(kat.msg).to_bytes() != kat.sig {
                return false;
            }
            if !verify(&kat.public, kat.msg, &kat.sig) {
                return false;
            }
            let mut bad = kat.sig;
            bad[0] ^= 1;
            if verify(&kat.public, kat.msg, &bad) || verify(&kat.public, b"other", &kat.sig) {
                return false;
            }
        }
        true
    }
}

#[cfg(not(feature = "ed25519"))]
mod imp {
    use super::{PUBLIC_KEY_LEN, SIGNATURE_LEN};

    /// 이 이미지에 Ed25519가 들어 있지 않음
    pub const HAS_ED25519: bool = false;

    /// 알고리즘이 없는 이미지의 키 자리 구조체입니다. 아무것도 담지 않습니다.
    pub struct Key;

    impl Key {
        pub const fn empty() -> Self {
            Key
        }

        pub fn wipe(&mut self) {}

        pub fn public(&self) -> Option<[u8; PUBLIC_KEY_LEN]> {
            None
        }

        pub fn sign(&self, _msg: &[u8]) -> Option<[u8; SIGNATURE_LEN]> {
            None
        }
    }

    /// 알고리즘이 없으므로 시드를 지우기만 하고 빈 키 자리를 주는 함수입니다.
    pub fn keygen(seed: &mut [u8; 32]) -> Option<Key> {
        for b in seed.iter_mut() {
            // SAFETY: b는 호출자가 준 배열 안의 유효한 바이트임
            unsafe { core::ptr::write_volatile(b, 0) };
        }
        Some(Key)
    }

    pub fn verify(_public: &[u8; PUBLIC_KEY_LEN], _msg: &[u8], _sig: &[u8; SIGNATURE_LEN]) -> bool {
        false
    }

    /// 시험할 알고리즘이 없으므로 항상 통과하는 함수입니다.
    pub fn self_test() -> bool {
        true
    }
}

pub use imp::{keygen, self_test, verify, Key, HAS_ED25519};
