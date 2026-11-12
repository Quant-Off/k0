use crate::proto::{alg, indicator, request, state, status, status_word};
use crate::proto::{caller_of, fetch_plan, publish_plan, reader_badge, role_of, signer_badge, Caller, Role};
use crate::proto::{ADMIN_BADGE, INIT_LEN, MAX_CLIENTS, MAX_MESSAGE, MAX_RESPONSE, OPERATOR_BADGE, OPERATOR_FRAME};
use crate::server::{fetch, publish, Vault};

const SIGNER: u64 = signer_badge(1);
const READER: u64 = reader_badge(1);
const OPERATOR: u64 = OPERATOR_BADGE;
/// 시드 정보: rng-seed 32바이트
const GOOD_INFO: u64 = 32;
/// 연산 전부
const OPS: [u64; 6] = [
    request::STATUS,
    request::PUBLIC_KEY,
    request::SIGN,
    request::INIT,
    request::ZEROIZE,
    request::KEYGEN,
];

/// 볼트의 상태를 관리자의 상태 조회로 읽는 도우미입니다.
fn state_of(v: &mut Vault) -> u64 {
    let (r, _) = call(v, ADMIN_BADGE, request::STATUS, alg::NONE, &[]);
    assert_eq!(r[0], status::OK);
    status_word::state(r[1])
}

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

/// 시드를 받은 볼트를 만드는 도우미입니다. 알고리즘이 없는 이미지에서는 시드를
/// 받지 않으므로 처음 상태 그대로입니다.
fn seeded() -> Vault {
    let mut v = Vault::new();
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([7; 32], GOOD_INFO));
    if cfg!(feature = "ed25519") {
        assert_eq!(r[0], status::OK);
        assert_eq!(state_of(&mut v), state::SEEDED);
    } else {
        assert_eq!(r[0], status::UNSUPPORTED);
        assert_eq!(state_of(&mut v), state::FRESH);
    }
    v
}

/// 키가 있는 볼트를 만드는 도우미입니다(알고리즘이 있는 이미지에서만).
#[cfg(feature = "ed25519")]
fn ready() -> Vault {
    let mut v = seeded();
    let (r, _) = call(&mut v, OPERATOR, request::KEYGEN, alg::ED25519, &[]);
    assert_eq!(r[0], status::OK);
    assert_eq!(state_of(&mut v), state::READY);
    v
}

#[test]
fn badges_map_to_fixed_roles_and_frames() {
    let caller = |role, request, response| Some(Caller { role, request, response });
    assert_eq!(caller_of(0), None, "a call without a badge has no role");
    assert_eq!(caller_of(ADMIN_BADGE), caller(Role::Admin, Some(0), 0));
    assert_eq!(caller_of(ADMIN_BADGE | 1), None);
    assert_eq!(caller_of(signer_badge(1)), caller(Role::Signer, Some(1), 1));
    assert_eq!(caller_of(signer_badge(0)), None);
    assert_eq!(caller_of(signer_badge(2)), None, "no frame pair is mapped for a second caller");
    assert_eq!(caller_of(reader_badge(1)), caller(Role::Reader, Some(1), 1));
    // 운영자에게는 요청 프레임이 없고, 응답 프레임은 누구의 것과도 겹치지 않음
    assert_eq!(caller_of(OPERATOR_BADGE), caller(Role::Operator, None, OPERATOR_FRAME));
    assert_eq!(caller_of(OPERATOR_BADGE | 1), None);
    assert!(OPERATOR_FRAME > MAX_CLIENTS);
    assert_eq!(caller_of(1), None);
    assert_eq!(caller_of(u64::MAX), None);
    assert_eq!(role_of(OPERATOR_BADGE), Some(Role::Operator));
    assert_eq!(role_of(0), None);
    // 네 배지는 서로 다름
    let all = [ADMIN_BADGE, OPERATOR_BADGE, SIGNER, READER];
    for (i, a) in all.iter().enumerate() {
        assert!(all[i + 1..].iter().all(|b| a != b));
    }
}

#[test]
fn each_role_may_do_exactly_its_operations() {
    use request::{INIT, KEYGEN, PUBLIC_KEY, SIGN, STATUS, ZEROIZE};
    let table: [(u64, &[u64]); 4] = [
        (ADMIN_BADGE, &[STATUS, INIT, KEYGEN, ZEROIZE]),
        (OPERATOR, &[STATUS, PUBLIC_KEY, KEYGEN, ZEROIZE]),
        (SIGNER, &[STATUS, PUBLIC_KEY, SIGN]),
        (READER, &[STATUS, PUBLIC_KEY]),
    ];
    for (badge, may) in table {
        for op in OPS {
            // 거부는 볼트의 상태와 무관해야 하므로 상태마다 확인함. 요청은 그
            // 연산에 맞는 모양으로 보냄(잘못된 모양은 다른 까닭으로 거부되므로)
            for stage in 0..3 {
                let mut v = Vault::new();
                let seed = init_body([7; 32], GOOD_INFO);
                if stage >= 1 {
                    call(&mut v, ADMIN_BADGE, INIT, alg::NONE, &seed);
                }
                if stage >= 2 {
                    call(&mut v, ADMIN_BADGE, KEYGEN, alg::ED25519, &[]);
                }
                let body: &[u8] = match op {
                    INIT => &seed,
                    SIGN => b"m",
                    _ => &[],
                };
                let (r, out) = call(&mut v, badge, op, alg::ED25519, body);
                if may.contains(&op) {
                    // 시드는 한 번만 받으므로, 이미 받은 볼트는 관리자의 두 번째
                    // 시드도 같은 상태 값으로 거절함. 그 밖에는 거절이 아니어야 함
                    if !(op == INIT && stage >= 1) {
                        assert_ne!(r[0], status::DENIED, "badge {badge:#x} op {op} stage {stage}");
                    }
                } else {
                    assert_eq!(r[0], status::DENIED, "badge {badge:#x} op {op} stage {stage}");
                    assert!(out.is_empty());
                }
            }
        }
    }
}

#[test]
fn no_frame_is_touched_for_a_request_that_cannot_have_a_body() {
    let word = |op| request::word(op, alg::ED25519);
    // 본문이 있는 연산에서, 요청 프레임이 있는 호출자가 길이를 말했을 때만 읽음
    assert_eq!(fetch_plan(SIGNER, &[word(request::SIGN), 5, 0, 0]), Some((1, 5)));
    assert_eq!(fetch_plan(ADMIN_BADGE, &[word(request::INIT), 40, 0, 0]), Some((0, 40)));
    assert_eq!(fetch_plan(SIGNER, &[word(request::SIGN), 0, 0, 0]), None);
    // 운영자는 어떤 연산과 길이로도 프레임을 읽히지 못함
    for op in 0..=255u64 {
        for len in [0, 1, 40, 1024, u64::MAX] {
            assert_eq!(fetch_plan(OPERATOR, &[word(op), len, 0, 0]), None, "op {op} len {len}");
        }
    }
    // 본문이 없는 연산은 누가 길이를 말해도 읽지 않음
    for badge in [ADMIN_BADGE, SIGNER, READER] {
        for op in [request::STATUS, request::PUBLIC_KEY, request::ZEROIZE, request::KEYGEN, 0, 9] {
            assert_eq!(fetch_plan(badge, &[word(op), 7, 0, 0]), None, "badge {badge:#x} op {op}");
        }
    }
    // 모르는 배지와 배지 없는 호출
    for badge in [0, 1, 0x99, u64::MAX] {
        assert_eq!(fetch_plan(badge, &[word(request::SIGN), 5, 0, 0]), None);
        assert_eq!(publish_plan(badge, &[word(request::SIGN), 5, 0, 0], status::OK), None);
    }
    // 응답 프레임에는 응답 본문이 있는 연산이 성공했을 때만 씀
    assert_eq!(publish_plan(SIGNER, &[word(request::SIGN), 5, 0, 0], status::OK), Some(1));
    assert_eq!(publish_plan(READER, &[word(request::PUBLIC_KEY), 0, 0, 0], status::OK), Some(1));
    assert_eq!(publish_plan(OPERATOR, &[word(request::PUBLIC_KEY), 0, 0, 0], status::OK), Some(OPERATOR_FRAME));
    assert_eq!(publish_plan(SIGNER, &[word(request::SIGN), 5, 0, 0], status::DENIED), None);
    for op in [request::STATUS, request::INIT, request::ZEROIZE, request::KEYGEN] {
        for badge in [ADMIN_BADGE, OPERATOR, SIGNER, READER] {
            assert_eq!(publish_plan(badge, &[word(op), 0, 0, 0], status::OK), None);
        }
    }
}

#[test]
fn a_request_that_claims_a_body_it_cannot_have_is_refused() {
    // 요청 프레임이 없는 운영자가 길이를 말하면 볼트는 아무것도 읽지 않고
    // 빈 본문으로 처리하므로, 말한 길이와 맞지 않아 거부됨
    let mut out = [0u8; MAX_RESPONSE];
    for op in OPS {
        for len in [1, 40, 1024, u64::MAX] {
            let mut v = seeded();
            let r = v.handle(OPERATOR, [request::word(op, alg::ED25519), len, 7, 0], &[], &mut out);
            assert_eq!((r[0], r[3]), (status::BAD_REQUEST, 7), "op {op} len {len}");
            assert_eq!(state_of(&mut v), seeded_state());
        }
    }
    // 본문이 없는 연산에 본문이 오면 거부함
    for op in [request::STATUS, request::PUBLIC_KEY, request::ZEROIZE, request::KEYGEN] {
        let mut v = seeded();
        let (r, _) = call(&mut v, ADMIN_BADGE, op, alg::ED25519, b"x");
        assert!(matches!(r[0], status::BAD_REQUEST | status::DENIED), "op {op}: {}", r[0]);
        assert_eq!(state_of(&mut v), seeded_state());
    }
}

/// [seeded]가 만드는 볼트의 상태(알고리즘이 없는 이미지에서는 시드를 받지 않음)
fn seeded_state() -> u64 {
    if cfg!(feature = "ed25519") { state::SEEDED } else { state::FRESH }
}

#[test]
fn the_status_word_packs_state_algorithm_and_what_is_built() {
    assert_eq!(status_word::pack(state::SEEDED, alg::NONE, 0), state::SEEDED);
    let w = status_word::pack(state::READY, alg::ED25519, 1 << alg::ED25519);
    assert_eq!(w, 0x0002_0101);
    assert_eq!(
        (status_word::state(w), status_word::key_alg(w), status_word::built(w)),
        (state::READY, alg::ED25519, 2)
    );
    // 볼트는 어느 상태에서든 이미지에 든 알고리즘을 알림
    let built = if cfg!(feature = "ed25519") { 1 << alg::ED25519 } else { 0 };
    for mut v in [Vault::new(), seeded(), Vault::failed_for_test()] {
        let (r, _) = call(&mut v, READER, request::STATUS, alg::NONE, &[]);
        assert_eq!(status_word::built(r[1]), built);
        assert_eq!(status_word::key_alg(r[1]), alg::NONE);
    }
}

#[test]
fn a_call_without_a_known_badge_is_denied() {
    let mut v = seeded();
    for badge in [0, 0x99, ADMIN_BADGE | 1, OPERATOR_BADGE | 1, signer_badge(2), u64::MAX] {
        for op in OPS {
            let (r, out) = call(&mut v, badge, op, alg::ED25519, &[]);
            assert_eq!(r[0], status::DENIED, "badge {badge:#x} op {op}");
            assert!(out.is_empty());
        }
    }
    assert_eq!(state_of(&mut v), seeded_state());
}

#[test]
fn malformed_requests_are_refused_whole() {
    let mut v = seeded();
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
        assert_eq!(state_of(&mut v), state::FRESH);
        assert!(!v.holds_a_secret());
        // 시드를 받지 못한 볼트에 키를 만들라고 하면 같은 까닭으로 거절함
        let (r, _) = call(&mut v, OPERATOR, request::KEYGEN, alg::ED25519, &[]);
        let want = if cfg!(feature = "ed25519") { status::NO_ENTROPY } else { status::UNSUPPORTED };
        assert_eq!(r[0], want);
    }
    // 전부 0인 시드는 재료 정보가 무엇이라 하든 받지 않음(이미 지워진 시드가
    // 다시 넘어온 경우). 받아들이면 누구나 아는 키가 만들어짐
    for info in [GOOD_INFO, SEED_RNDR, GOOD_INFO | SEED_RNDR] {
        let mut v = Vault::new();
        let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([0; 32], info));
        assert_eq!(r[0], status::NO_ENTROPY);
        assert_eq!(state_of(&mut v), state::FRESH);
    }
    // 길이가 다른 INIT 본문
    let mut v = Vault::new();
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &[1; 39]);
    assert_eq!(r[0], status::BAD_REQUEST);
}

#[test]
fn the_seed_is_taken_once_and_zeroize_is_final() {
    let mut v = seeded();
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([9; 32], GOOD_INFO));
    if cfg!(feature = "ed25519") {
        assert_eq!(r[0], status::DENIED, "a second seed cannot replace the first");
        assert!(v.holds_a_secret());
    }
    // 운영자도 지울 수 있고, 시드만 쥔 상태에서도 지워짐
    let (r, _) = call(&mut v, OPERATOR, request::ZEROIZE, alg::NONE, &[]);
    assert_eq!(r[0], status::OK);
    assert_eq!(state_of(&mut v), state::ZEROIZED);
    assert!(!v.holds_a_secret());
    // 지운 뒤에는 시드도 키 생성도 받지 않고, 다시 지우는 것은 그대로 성공함
    let (r, _) = call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([9; 32], GOOD_INFO));
    assert_eq!(r[0], status::DENIED);
    let (r, _) = call(&mut v, OPERATOR, request::KEYGEN, alg::ED25519, &[]);
    let want = if cfg!(feature = "ed25519") { status::ZEROIZED } else { status::UNSUPPORTED };
    assert_eq!(r[0], want);
    assert_eq!(call(&mut v, ADMIN_BADGE, request::ZEROIZE, alg::NONE, &[]).0[0], status::OK);
    // 지워진 볼트는 시드의 재료도 더는 알리지 않음
    let (r, _) = call(&mut v, READER, request::STATUS, alg::NONE, &[]);
    assert_eq!(r[2] & (indicator::SEED_RNG_SEED | indicator::SEED_RNDR), 0);
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
        assert_eq!((r[0], status_word::state(r[1])), (status::OK, state::FRESH));
        assert_eq!(r[2], indicator::SELF_TEST_PASSED);
    }

    #[test]
    fn key_generation_follows_the_states() {
        use k0_abi::bootinfo::SEED_RNDR;
        // 시드를 쥔 볼트는 그 재료를 상태 조회로 알림. 키는 아직 없음
        let mut v = Vault::new();
        let info = GOOD_INFO | SEED_RNDR;
        assert_eq!(call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init_body([7; 32], info)).0[0], status::OK);
        let (r, _) = call(&mut v, OPERATOR, request::STATUS, alg::NONE, &[]);
        assert_eq!((status_word::state(r[1]), status_word::key_alg(r[1])), (state::SEEDED, alg::NONE));
        let both = indicator::SEED_RNG_SEED | indicator::SEED_RNDR;
        assert_eq!(r[2], indicator::SELF_TEST_PASSED | both);
        assert!(v.holds_a_secret());
        for badge in [SIGNER, READER, OPERATOR] {
            assert_eq!(call(&mut v, badge, request::PUBLIC_KEY, alg::ED25519, &[]).0[0], status::NOT_READY);
        }
        assert_eq!(call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"m").0[0], status::NOT_READY);

        // 이미지에 없는 알고리즘을 고르면 거절하고 시드는 그대로 쥠
        for algo in [alg::NONE, 2, 15, 0xFF] {
            let (r, _) = call(&mut v, OPERATOR, request::KEYGEN, algo, &[]);
            assert_eq!(r[0], status::UNSUPPORTED, "alg {algo}");
        }
        assert_eq!(state_of(&mut v), state::SEEDED);

        // 운영자가 키를 만들게 함. 시드는 지워지고 그 재료의 표시는 키의 것이 됨
        let (r, _) = call(&mut v, OPERATOR, request::KEYGEN, alg::ED25519, &[]);
        assert_eq!((r[0], r[1]), (status::OK, 0));
        let (r, _) = call(&mut v, OPERATOR, request::STATUS, alg::NONE, &[]);
        assert_eq!((status_word::state(r[1]), status_word::key_alg(r[1])), (state::READY, alg::ED25519));
        assert_eq!(r[2], indicator::SELF_TEST_PASSED | both);
        // 키는 그 시드에서 나옴
        let (_, public) = call(&mut v, OPERATOR, request::PUBLIC_KEY, alg::ED25519, &[]);
        let direct = ed25519_dalek::SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
        assert_eq!(public, direct);

        // 부팅당 키 하나: 누구도 두 번째 키를 만들지 못함
        for badge in [OPERATOR, ADMIN_BADGE] {
            assert_eq!(call(&mut v, badge, request::KEYGEN, alg::ED25519, &[]).0[0], status::KEY_EXISTS);
        }
        let (_, again) = call(&mut v, READER, request::PUBLIC_KEY, alg::ED25519, &[]);
        assert_eq!(again, direct);

        // 관리자도 키를 만들게 할 수 있음(터미널이 없는 이미지의 자가 검증)
        let mut v = seeded();
        assert_eq!(call(&mut v, ADMIN_BADGE, request::KEYGEN, alg::ED25519, &[]).0[0], status::OK);
        assert_eq!(state_of(&mut v), state::READY);
    }

    #[test]
    fn the_operator_gets_the_public_key_and_never_a_signature() {
        let mut v = ready();
        let (r, public) = call(&mut v, OPERATOR, request::PUBLIC_KEY, alg::ED25519, &[]);
        assert_eq!((r[0], public.len()), (status::OK, 32));
        for body in [&b"m"[..], &[], &[0u8; MAX_MESSAGE]] {
            let (r, out) = call(&mut v, OPERATOR, request::SIGN, alg::ED25519, body);
            assert_eq!(r[0], status::DENIED);
            assert!(out.is_empty());
        }
        // 운영자는 시드를 넘기지도 못함
        let mut fresh = Vault::new();
        let (r, _) = call(&mut fresh, OPERATOR, request::INIT, alg::NONE, &init_body([7; 32], GOOD_INFO));
        assert_eq!(r[0], status::DENIED);
        assert!(!fresh.holds_a_secret());
    }

    #[test]
    fn a_vault_that_fails_holds_no_secret() {
        // 시드를 쥔 채로, 그리고 키를 쥔 채로 오류 상태에 들어가는 경우
        for mut v in [seeded(), ready()] {
            assert!(v.holds_a_secret());
            v.fail_for_test();
            assert!(!v.holds_a_secret());
            let (r, _) = call(&mut v, OPERATOR, request::STATUS, alg::NONE, &[]);
            assert_eq!((status_word::state(r[1]), status_word::key_alg(r[1]), r[2]), (state::FAILED, alg::NONE, 0));
            for op in [request::KEYGEN, request::ZEROIZE, request::PUBLIC_KEY] {
                assert_eq!(call(&mut v, OPERATOR, op, alg::ED25519, &[]).0[0], status::FAILED);
            }
        }
    }

    #[test]
    fn signatures_verify_and_the_key_follows_the_seed() {
        let mut v = Vault::new();
        let (r, _) = call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"too early");
        assert_eq!(r[0], status::NOT_READY);
        let mut v = seeded();
        let (r, _) = call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"still too early");
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
        assert_eq!((r[0], status_word::state(r[1]), r[2]), (status::OK, state::FAILED, 0));
        assert_eq!(call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"m").0[0], status::FAILED);
        assert_eq!(call(&mut v, READER, request::PUBLIC_KEY, alg::ED25519, &[]).0[0], status::FAILED);
        let init = init_body([7; 32], GOOD_INFO);
        assert_eq!(call(&mut v, ADMIN_BADGE, request::INIT, alg::NONE, &init).0[0], status::FAILED);
        assert_eq!(call(&mut v, ADMIN_BADGE, request::ZEROIZE, alg::NONE, &[]).0[0], status::FAILED);
    }
}

#[cfg(not(feature = "ed25519"))]
#[test]
fn without_an_algorithm_the_vault_takes_no_seed_and_serves_nothing() {
    // 쓸 일이 없는 비밀은 쥐지 않음: 시드를 받지 않고 처음 상태로 남음
    let mut v = seeded();
    assert!(!v.holds_a_secret());
    assert_eq!(state_of(&mut v), state::FRESH);
    for badge in [OPERATOR, ADMIN_BADGE] {
        assert_eq!(call(&mut v, badge, request::KEYGEN, alg::ED25519, &[]).0[0], status::UNSUPPORTED);
    }
    assert_eq!(call(&mut v, SIGNER, request::SIGN, alg::ED25519, b"m").0[0], status::UNSUPPORTED);
    assert_eq!(call(&mut v, READER, request::PUBLIC_KEY, alg::ED25519, &[]).0[0], status::UNSUPPORTED);
    let (r, _) = call(&mut v, READER, request::STATUS, alg::NONE, &[]);
    assert_eq!((r[0], status_word::built(r[1]), status_word::key_alg(r[1])), (status::OK, 0, alg::NONE));
    // 지우는 것은 여기서도 됨
    assert_eq!(call(&mut v, OPERATOR, request::ZEROIZE, alg::NONE, &[]).0[0], status::OK);
    assert_eq!(state_of(&mut v), state::ZEROIZED);
}
