use crate::proto::{alg, indicator, request, state, status};
use crate::proto::{reader_badge, role_of, signer_badge, Role, ADMIN_BADGE, INIT_LEN, MAX_MESSAGE, MAX_RESPONSE};
use crate::server::{fetch, publish, Vault};

const SIGNER: u64 = signer_badge(1);
const READER: u64 = reader_badge(1);
/// 시드 정보: rng-seed 32바이트
const GOOD_INFO: u64 = 32;

fn call(v: &mut Vault, badge: u64, op: u64, algo: u64, body: &[u8]) -> ([u64; 4], Vec<u8>) {
    let mut out = [0xEEu8; MAX_RESPONSE];
    let r = v.handle(badge, [request::word(op, algo), body.len() as u64, 0x1234, 0], body, &mut out);
    assert_eq!(r[3], 0x1234, "the nonce is always echoed");
    let n = if op == request::STATUS { 0 } else { r[1] as usize };
    // 응답 본문이 아닌 자리는 한 바이트도 건드리지 않았어야 함. 거부된 요청은
    // 길이가 0이므로 버퍼 전체가 그대로여야 함
    assert!(out[n..].iter().all(|&b| b == 0xEE), "bytes written beyond the reply body");
    (r, out[..n].to_vec())
}

fn init_body(seed: [u8; 32], info: u64) -> Vec<u8> {
    let mut b = seed.to_vec();
    b.extend_from_slice(&info.to_le_bytes());
    assert_eq!(b.len(), INIT_LEN);
    b
}

fn ready() -> Vault {
    let mut v = Vault::new();
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([7; 32], GOOD_INFO));
    assert_eq!(r[0], status::OK);
    v
}

#[test]
fn badges_map_to_fixed_roles() {
    assert_eq!(role_of(0), None, "a call without a badge has no role");
    assert_eq!(role_of(ADMIN_BADGE), Some((Role::Admin, 0)));
    assert_eq!(role_of(ADMIN_BADGE | 1), None);
    assert_eq!(role_of(signer_badge(1)), Some((Role::Signer, 1)));
    assert_eq!(role_of(signer_badge(0)), None);
    assert_eq!(role_of(signer_badge(2)), None, "no frame pair is mapped for a second caller");
    assert_eq!(role_of(reader_badge(1)), Some((Role::Reader, 1)));
    assert_eq!(role_of(1), None);
    assert_eq!(role_of(u64::MAX), None);
}

#[test]
fn policy_is_enforced_before_anything_else() {
    let mut v = ready();
    let msg = b"m";
    for (badge, op) in [
        (0, request::STATUS),
        (0, request::SIGN),
        (0x99, request::SIGN),
        (READER, request::SIGN),
        (ADMIN_BADGE, request::SIGN),
        (ADMIN_BADGE, request::PUBLIC_KEY),
        (SIGNER, request::INIT),
        (SIGNER, request::ZEROIZE),
        (READER, request::ZEROIZE),
    ] {
        let (r, out) = call(&mut v, badge, op, alg::ED25519, msg);
        assert_eq!(r[0], status::DENIED, "badge {badge:#x} op {op}");
        assert!(out.is_empty());
    }
    // 거부된 요청들은 볼트의 상태를 바꾸지 않았음
    assert_eq!(call(&mut v, SIGNER, request::STATUS, alg::NONE, &[]).0[1], state::READY);
}

#[test]
fn malformed_requests_are_refused_whole() {
    let mut v = ready();
    let mut out = [0u8; MAX_RESPONSE];
    let ok = request::word(request::SIGN, alg::ED25519);
    // 예약 비트, 쓰지 않는 레지스터, 모르는 연산, 말한 길이와 다른 본문
    for mr in [[ok | 1 << 16, 1, 0, 0], [ok, 1, 0, 9], [request::word(9, alg::ED25519), 1, 0, 0], [ok, 2, 0, 0]] {
        assert_eq!(v.handle(SIGNER, mr, b"m", &mut out)[0], status::BAD_REQUEST, "{mr:x?}");
    }
    // 버퍼보다 길다고 말한 요청은 잘린 본문과 길이가 맞지 않아 거부됨
    let body = [0u8; MAX_MESSAGE];
    let r = v.handle(SIGNER, [ok, MAX_MESSAGE as u64 + 1, 0, 0], &body, &mut out);
    assert_eq!(r[0], status::BAD_REQUEST);
}

#[test]
fn a_seed_without_real_entropy_makes_no_key() {
    use k0_abi::bootinfo::SEED_RNDR;
    for info in [0, 8, 31] {
        let mut v = Vault::new();
        let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([1; 32], info));
        assert_eq!(r[0], status::NO_ENTROPY, "info {info}");
        assert_eq!(call(&mut v, SIGNER, request::STATUS, alg::NONE, &[]).0[1], state::FRESH);
    }
    let mut v = Vault::new();
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([1; 32], SEED_RNDR));
    assert_eq!(r[0], status::OK);
    assert_eq!(r[2] & (indicator::SEED_RNDR | indicator::SEED_RNG_SEED), indicator::SEED_RNDR);
    // 길이가 다른 INIT 본문
    let mut v = Vault::new();
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &[1; 39]);
    assert_eq!(r[0], status::BAD_REQUEST);
}

#[test]
fn init_happens_once_and_zeroize_is_final() {
    let mut v = ready();
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([9; 32], GOOD_INFO));
    assert_eq!(r[0], status::DENIED, "a second key cannot replace the first");
    let (r, _) = call(&mut v, ADMIN_BADGE, request::ZEROIZE, alg::NONE, &[]);
    assert_eq!(r[0], status::OK);
    assert_eq!(call(&mut v, SIGNER, request::STATUS, alg::NONE, &[]).0[1], state::ZEROIZED);
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([9; 32], GOOD_INFO));
    assert_eq!(r[0], status::DENIED);
}

#[test]
fn fetch_clamps_the_length_and_publish_writes_header_and_body() {
    let frame = [0x5Au8; 64];
    let mut dst = [0u8; 16];
    // 말한 길이가 버퍼보다 길면 버퍼만큼만 옮김
    // SAFETY: frame은 64바이트이고 dst는 16바이트임
    assert_eq!(unsafe { fetch(frame.as_ptr(), 1000, &mut dst) }, 16);
    assert_eq!(dst, [0x5A; 16]);
    let mut dst = [0u8; 16];
    // SAFETY: 위와 같음
    assert_eq!(unsafe { fetch(frame.as_ptr(), 5, &mut dst) }, 5);
    assert_eq!(&dst[..6], &[0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0]);

    let mut resp = [0xFFu64; 16];
    // SAFETY: resp는 128바이트이고 8바이트로 정렬돼 있음
    unsafe { publish(resp.as_mut_ptr().cast(), 0xABCD, &[1, 2, 3]) };
    assert_eq!(resp[0], 0xABCD);
    assert_eq!(resp[1], 3);
    assert_eq!(resp[2].to_le_bytes()[..3], [1, 2, 3]);
}

#[cfg(feature = "ed25519")]
mod ed25519 {
    use super::*;
    use crate::provider;

    #[test]
    fn the_self_test_passes_and_is_reported() {
        assert!(provider::self_test());
        let mut v = Vault::new();
        let (r, _) = call(&mut v, READER, request::STATUS, alg::NONE, &[]);
        assert_eq!((r[0], r[1]), (status::OK, state::FRESH));
        assert_eq!(r[2], indicator::SELF_TEST_PASSED);
    }

    #[test]
    fn signatures_verify_and_the_key_follows_the_seed() {
        let mut v = Vault::new();
        let (r, _) = call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"too early");
        assert_eq!(r[0], status::NOT_READY);
        let mut v = ready();
        let (r, public) = call(&mut v, READER, request::PUBLIC_KEY, alg::ED25519, &[]);
        assert_eq!((r[0], r[1]), (status::OK, 32));
        let want = indicator::FIPS_186_5_ALGORITHM | indicator::SELF_TEST_PASSED | indicator::SEED_RNG_SEED;
        assert_eq!(r[2], want);
        let public: [u8; 32] = public.try_into().unwrap();
        // 같은 시드에서 구현이 직접 내는 공개키와 같음
        let direct = ed25519_dalek::SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
        assert_eq!(public, direct);

        let msg = b"pay 10 to alice";
        let (r, sig) = call(&mut v, SIGNER, request::SIGN, alg::ED25519, msg);
        assert_eq!((r[0], r[1], r[2]), (status::OK, 64, want));
        let sig: [u8; 64] = sig.try_into().unwrap();
        assert!(provider::verify(&public, msg, &sig));
        assert!(!provider::verify(&public, b"pay 99 to alice", &sig));
        let mut bad = sig;
        bad[63] ^= 0x80;
        assert!(!provider::verify(&public, msg, &bad));
        // 가장 긴 메시지와 빈 메시지
        for m in [&[0x33u8; MAX_MESSAGE][..], &[][..]] {
            let (r, sig) = call(&mut v, SIGNER, request::SIGN, alg::ED25519, m);
            assert_eq!(r[0], status::OK);
            assert!(provider::verify(&public, m, &sig.try_into().unwrap()));
        }
    }

    #[test]
    fn a_public_key_request_carries_no_body() {
        let mut v = ready();
        let (r, _) = call(&mut v, SIGNER, request::PUBLIC_KEY, alg::ED25519, b"x");
        assert_eq!(r[0], status::BAD_REQUEST);
    }

    #[test]
    fn unknown_algorithms_are_unsupported() {
        let mut v = ready();
        for algo in [alg::NONE, 2, 0xFF] {
            assert_eq!(call(&mut v, SIGNER, request::SIGN, algo, b"m").0[0], status::UNSUPPORTED);
            assert_eq!(call(&mut v, READER, request::PUBLIC_KEY, algo, &[]).0[0], status::UNSUPPORTED);
        }
    }

    #[test]
    fn nothing_is_served_after_zeroize() {
        let mut v = ready();
        assert_eq!(call(&mut v, ADMIN_BADGE, request::ZEROIZE, alg::NONE, &[]).0[0], status::OK);
        let (r, out) = call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"m");
        assert_eq!(r[0], status::ZEROIZED);
        assert!(out.is_empty());
        assert_eq!(call(&mut v, READER, request::PUBLIC_KEY, alg::ED25519, &[]).0[0], status::ZEROIZED);
    }

    #[test]
    fn a_small_order_public_key_is_refused_even_with_a_matching_signature() {
        // 공개키가 항등원이면 R = 항등원, S = 0인 서명이 어떤 메시지에도 등식을
        // 만족함. 느슨한 검증은 이것을 받아들이므로, 거부된다는 것이 엄격한
        // 검증을 쓰고 있다는 증거임
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let mut forged = [0u8; 64];
        forged[0] = 1;
        let loose = ed25519_dalek::VerifyingKey::from_bytes(&identity).unwrap();
        assert!(ed25519_dalek::Verifier::verify(&loose, b"m", &ed25519_dalek::Signature::from_bytes(&forged)).is_ok());
        assert!(!provider::verify(&identity, b"m", &forged));
        assert!(!provider::verify(&[0xFF; 32], b"m", &[0; 64]));
    }

    #[test]
    fn a_failed_vault_refuses_everything_but_status() {
        let mut v = Vault::failed_for_test();
        let (r, _) = call(&mut v, SIGNER, request::STATUS, alg::NONE, &[]);
        assert_eq!((r[0], r[1], r[2]), (status::OK, state::FAILED, 0));
        assert_eq!(call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"m").0[0], status::FAILED);
        assert_eq!(call(&mut v, READER, request::PUBLIC_KEY, alg::ED25519, &[]).0[0], status::FAILED);
        let init = init_body([7; 32], GOOD_INFO);
        assert_eq!(call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init).0[0], status::FAILED);
        assert_eq!(call(&mut v, ADMIN_BADGE, request::ZEROIZE, alg::NONE, &[]).0[0], status::FAILED);
    }
}

#[cfg(not(feature = "ed25519"))]
#[test]
fn without_an_algorithm_every_signing_request_is_unsupported() {
    let mut v = ready();
    assert_eq!(call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"m").0[0], status::UNSUPPORTED);
    assert_eq!(call(&mut v, READER, request::PUBLIC_KEY, alg::ED25519, &[]).0[0], status::UNSUPPORTED);
    assert_eq!(call(&mut v, READER, request::STATUS, alg::NONE, &[]).0[0], status::OK);
}
