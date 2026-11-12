//! 볼트 프로토콜의 값을 터미널의 타입으로 옮기는 모듈입니다.
//!
//! # Features
//! 터미널은 볼트의 운영자 역할로 네 연산만 부릅니다. 이 모듈은 그 응답의
//! 숫자(상태 워드, 표시 비트, 상태 값)를 터미널이 화면에 내는 타입으로 바꾸는
//! 표를 한곳에 모읍니다. 시스템 콜을 부르는 일은 루트 태스크의 배선이 하고,
//! 여기의 함수들은 순수해서 호스트에서 시험됩니다.

use k0_vault::proto::{alg, indicator, state, status, status_word};

use crate::{Alg, Fail, KeyState, Status};

/// 알고리즘의 프로토콜 번호를 주는 함수입니다.
pub const fn alg_number(a: Alg) -> u64 {
    match a {
        Alg::Ed25519 => alg::ED25519,
    }
}

/// 프로토콜 번호를 알고리즘으로 푸는 함수입니다. 터미널이 이름을 모르는
/// 번호와 "없음"은 `None`입니다.
pub fn alg_of(number: u64) -> Option<Alg> {
    Alg::ALL.into_iter().find(|a| alg_number(*a) == number)
}

/// 상태 조회 응답을 푸는 함수입니다. 모르는 상태 값이면 `None`입니다.
///
/// # Arguments
/// `word` - 응답의 MR1(상태 워드)
/// `ind` - 응답의 MR2(표시 비트)
pub fn status_of(word: u64, ind: u64) -> Option<Status> {
    let state = match status_word::state(word) {
        state::FRESH => KeyState::Fresh,
        state::SEEDED => KeyState::Seeded,
        state::READY => KeyState::Ready,
        state::ZEROIZED => KeyState::Zeroized,
        state::FAILED => KeyState::Failed,
        _ => return None,
    };
    let built = status_word::built(word);
    Some(Status {
        state,
        alg: alg_of(status_word::key_alg(word)),
        self_test: ind & indicator::SELF_TEST_PASSED != 0,
        seed_rng: ind & indicator::SEED_RNG_SEED != 0,
        seed_rndr: ind & indicator::SEED_RNDR != 0,
        built: Alg::ALL.map(|a| built >> alg_number(a) & 1 != 0),
    })
}

/// 볼트 응답의 상태 값(MR0)을 실패로 옮기는 함수입니다. 성공이면 `None`입니다.
///
/// 터미널이 보낼 리 없는 요청에 대한 거절(잘못된 요청)과 모르는 값은 "거절"로
/// 묶습니다.
pub fn fail_of(st: u64) -> Option<Fail> {
    Some(match st {
        status::OK => return None,
        status::UNSUPPORTED => Fail::Unsupported,
        status::NOT_READY => Fail::NoKey,
        status::FAILED => Fail::VaultFailed,
        status::ZEROIZED => Fail::Zeroized,
        status::NO_ENTROPY => Fail::NoEntropy,
        status::KEY_EXISTS => Fail::KeyExists,
        _ => Fail::Denied,
    })
}

/// 키가 없는 볼트에 공개키를 물었을 때의 실패를 상태에서 정하는 함수입니다.
pub fn no_key(state: KeyState) -> Fail {
    match state {
        KeyState::Zeroized => Fail::Zeroized,
        KeyState::Failed => Fail::VaultFailed,
        _ => Fail::NoKey,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_word_becomes_the_terminal_status() {
        let ind = indicator::SELF_TEST_PASSED | indicator::SEED_RNG_SEED;
        let st = status_of(status_word::pack(state::READY, alg::ED25519, 1 << alg::ED25519), ind).unwrap();
        assert_eq!(
            st,
            Status {
                state: KeyState::Ready,
                alg: Some(Alg::Ed25519),
                self_test: true,
                seed_rng: true,
                seed_rndr: false,
                built: [true],
            }
        );
        let st = status_of(status_word::pack(state::FRESH, alg::NONE, 0), indicator::SEED_RNDR).unwrap();
        assert_eq!((st.state, st.alg, st.self_test, st.seed_rndr, st.built), (KeyState::Fresh, None, false, true, [false]));
        for (n, want) in [
            (state::FRESH, KeyState::Fresh),
            (state::SEEDED, KeyState::Seeded),
            (state::READY, KeyState::Ready),
            (state::ZEROIZED, KeyState::Zeroized),
            (state::FAILED, KeyState::Failed),
        ] {
            assert_eq!(status_of(n, 0).unwrap().state, want);
        }
        // 모르는 상태 값은 꾸며 내지 않음
        assert_eq!(status_of(5, 0), None);
        assert_eq!(status_of(0xFF, 0), None);
        // 터미널이 이름을 모르는 알고리즘의 키는 "없음"으로 보이고, 비트맵의
        // 다른 비트는 무시함
        let st = status_of(status_word::pack(state::READY, 9, 0xFFFD), 0).unwrap();
        assert_eq!((st.alg, st.built), (None, [false]));
    }

    #[test]
    fn every_vault_status_has_a_name() {
        assert_eq!(fail_of(status::OK), None);
        let table = [
            (status::DENIED, Fail::Denied),
            (status::BAD_REQUEST, Fail::Denied),
            (status::UNSUPPORTED, Fail::Unsupported),
            (status::NOT_READY, Fail::NoKey),
            (status::FAILED, Fail::VaultFailed),
            (status::ZEROIZED, Fail::Zeroized),
            (status::NO_ENTROPY, Fail::NoEntropy),
            (status::KEY_EXISTS, Fail::KeyExists),
            (9, Fail::Denied),
            (u64::MAX, Fail::Denied),
        ];
        for (st, want) in table {
            assert_eq!(fail_of(st), Some(want), "status {st}");
        }
        assert_eq!(no_key(KeyState::Fresh), Fail::NoKey);
        assert_eq!(no_key(KeyState::Seeded), Fail::NoKey);
        assert_eq!(no_key(KeyState::Zeroized), Fail::Zeroized);
        assert_eq!(no_key(KeyState::Failed), Fail::VaultFailed);
    }

    #[test]
    fn algorithm_numbers_round_trip() {
        for a in Alg::ALL {
            assert_eq!(alg_of(alg_number(a)), Some(a));
        }
        assert_eq!(alg_of(alg::NONE), None);
        assert_eq!(alg_of(200), None);
    }
}
