//! 볼트와 클라이언트가 공유하는 프로토콜 정의 모듈입니다.
//!
//! # Features
//! 클라이언트는 볼트의 엔드포인트에 `CALL`합니다. 요청의 메시지 레지스터는
//! MR0 = 연산과 알고리즘([request]), MR1 = 요청 프레임에 둔 바이트 수, MR2 =
//! 클라이언트가 고른 일회용 번호입니다. 응답은 MR0 = [status], MR1 = 응답
//! 프레임에 쓴 바이트 수(상태 조회에서는 볼트의 상태를 담은 워드,
//! [status_word]), MR2 = [indicator], MR3 = 요청의 일회용 번호입니다. 볼트는 네
//! 레지스터를 항상 전부 채웁니다.
//!
//! 본문은 프레임으로 오갑니다. 요청 프레임은 클라이언트만 쓰고 볼트는 읽기만
//! 하며, 응답 프레임은 그 반대입니다. 응답 프레임의 앞 [RESPONSE_HEADER]
//! 바이트는 일회용 번호와 길이이고 그 뒤가 본문입니다. 클라이언트는 응답이
//! 성공이고 프레임의 번호가 자기 번호와 같을 때만 본문을 믿어야 합니다
//! (타임아웃으로 버린 예전 요청의 답이 뒤늦게 쓰일 수 있음).
//!
//! 본문이 있는 연산은 서명([request::SIGN])과 시드 전달([request::INIT])뿐이고,
//! 응답 본문이 있는 연산은 공개키와 서명뿐입니다. 볼트는 그 밖의 연산에서는
//! 어떤 프레임에도 닿지 않습니다([fetch_plan], [publish_plan]).
//!
//! 누가 무엇을 할 수 있는지는 엔드포인트 배지로 정하고 빌드 때 고정됩니다
//! ([caller_of], [allowed]). 배지는 커널이 케이퍼빌리티에서 읽어 전달하므로
//! 클라이언트가 꾸밀 수 없습니다. 정책을 바꾸는 연산은 없습니다. 역할은 넷입니다.
//! 관리자(루트 태스크)는 시드를 넘기고, 운영자(직렬 터미널)는 알고리즘을 골라
//! 키를 만들게 하고 지우게 하며 공개키를 봅니다. 서명 클라이언트만 서명을
//! 받고, 읽기 클라이언트는 공개키만 받습니다. 운영자는 서명을 받지 못합니다.
//!
//! 키는 부팅당 하나입니다. 볼트는 시드를 받은 뒤([state::SEEDED]) 키 생성
//! 요청을 기다리고, 키를 만들면 시드를 지웁니다. 지운 키는 되살릴 수 없고 새
//! 키는 재부팅해야 만듭니다.

/// 요청 본문(서명할 메시지)의 최대 길이
pub const MAX_MESSAGE: usize = 1024;
/// 응답 본문의 최대 길이(가장 긴 응답은 서명)
pub const MAX_RESPONSE: usize = 64;
/// 응답 프레임 앞머리의 길이(일회용 번호 8바이트, 본문 길이 8바이트)
pub const RESPONSE_HEADER: usize = 16;
/// `INIT` 본문의 길이(시드 32바이트와 시드 재료 정보 8바이트)
pub const INIT_LEN: usize = 40;
/// 호출자 프레임 쌍의 수(관리자 제외). 볼트는 배지의 번호로 그 호출자의
/// 프레임을 고르므로, 이 수는 볼트의 주소 공간에 실제로 매핑해 둔 쌍의 수와
/// 같아야 합니다. 지금의 배선은 한 쌍입니다
pub const MAX_CLIENTS: u64 = 1;

/// 요청의 MR0을 만들고 푸는 모듈입니다. 하위 8비트가 연산, 그 위 8비트가
/// 알고리즘이고 나머지 비트는 0이어야 합니다.
pub mod request {
    /// 볼트의 상태와 표시를 묻는 연산
    pub const STATUS: u64 = 1;
    /// 공개키를 응답 프레임으로 받는 연산
    pub const PUBLIC_KEY: u64 = 2;
    /// 요청 프레임의 메시지에 서명해 응답 프레임으로 받는 연산
    pub const SIGN: u64 = 3;
    /// 시드와 그 재료 정보를 넘기는 연산(관리자 전용, 한 번만). 키는 만들지
    /// 않고 볼트가 시드를 쥔 채 [KEYGEN]을 기다림
    pub const INIT: u64 = 4;
    /// 키와 쥐고 있는 시드를 지우게 하는 연산(관리자와 운영자). 오류 상태의
    /// 볼트는 이미 아무것도 쥐고 있지 않으므로 지울 것이 없고, 다른 연산과
    /// 마찬가지로 [super::status::FAILED]로 답함
    pub const ZEROIZE: u64 = 5;
    /// 쥐고 있는 시드로 키를 만들게 하는 연산(관리자와 운영자, 한 번만).
    /// 알고리즘은 MR0에 실어 고름
    pub const KEYGEN: u64 = 6;

    /// 요청 본문이 있는 연산인지 알려 주는 함수입니다.
    pub const fn takes_body(op: u64) -> bool {
        matches!(op, SIGN | INIT)
    }

    /// 응답 본문이 있는 연산인지 알려 주는 함수입니다.
    pub const fn gives_body(op: u64) -> bool {
        matches!(op, PUBLIC_KEY | SIGN)
    }

    /// 연산과 알고리즘을 MR0 값으로 묶는 함수입니다.
    pub const fn word(op: u64, alg: u64) -> u64 {
        op | alg << 8
    }
}

/// 알고리즘 번호 모듈입니다. 번호는 16보다 작아야 합니다(상태 워드의 비트맵이
/// 16비트).
pub mod alg {
    /// 알고리즘을 가리지 않는 연산에 쓰는 값
    pub const NONE: u64 = 0;
    /// Ed25519(PureEdDSA, FIPS 186-5와 RFC 8032)
    pub const ED25519: u64 = 1;
}

/// 상태 조회 응답의 MR1을 만들고 푸는 모듈입니다.
///
/// 하위 8비트가 볼트의 [state], 그 위 8비트가 지금 있는 키의 알고리즘(없으면
/// [alg::NONE]), 그 위 16비트가 이 이미지에 들어 있는 알고리즘의 비트맵(비트
/// n = 알고리즘 n)입니다.
pub mod status_word {
    /// 세 값을 워드로 묶는 함수입니다.
    pub const fn pack(state: u64, key_alg: u64, built: u64) -> u64 {
        (state & 0xFF) | (key_alg & 0xFF) << 8 | (built & 0xFFFF) << 16
    }

    /// 볼트의 상태를 꺼내는 함수입니다.
    pub const fn state(word: u64) -> u64 {
        word & 0xFF
    }

    /// 지금 있는 키의 알고리즘을 꺼내는 함수입니다.
    pub const fn key_alg(word: u64) -> u64 {
        (word >> 8) & 0xFF
    }

    /// 이미지에 들어 있는 알고리즘의 비트맵을 꺼내는 함수입니다.
    pub const fn built(word: u64) -> u64 {
        (word >> 16) & 0xFFFF
    }
}

/// 응답의 MR0 값 모듈입니다.
pub mod status {
    pub const OK: u64 = 0;
    /// 이 배지에는 허용되지 않은 연산이거나 배지가 없음
    pub const DENIED: u64 = 1;
    /// 연산 번호, 길이, 예약 비트가 잘못됨
    pub const BAD_REQUEST: u64 = 2;
    /// 이 이미지에 그 알고리즘이 들어 있지 않음
    pub const UNSUPPORTED: u64 = 3;
    /// 아직 키가 없음(`INIT` 전)
    pub const NOT_READY: u64 = 4;
    /// 자가 시험이나 쌍 일치 시험이 실패해 볼트가 오류 상태임
    pub const FAILED: u64 = 5;
    /// 키가 지워졌음
    pub const ZEROIZED: u64 = 6;
    /// 시드의 재료가 키를 만들기에 모자라거나, 볼트가 시드를 받지 못했음
    pub const NO_ENTROPY: u64 = 7;
    /// 키가 이미 있음(부팅당 키 하나)
    pub const KEY_EXISTS: u64 = 8;
}

/// 볼트의 상태(상태 조회 응답의 MR1) 모듈입니다.
pub mod state {
    /// 시드도 키도 없음
    pub const FRESH: u64 = 0;
    /// 키가 있음
    pub const READY: u64 = 1;
    /// 키와 시드가 지워졌음
    pub const ZEROIZED: u64 = 2;
    /// 자가 시험이나 쌍 일치 시험이 실패했음(시드도 키도 쥐고 있지 않음)
    pub const FAILED: u64 = 3;
    /// 시드를 받았고 키는 아직 없음
    pub const SEEDED: u64 = 4;
}

/// 응답의 MR2에 실리는 표시 비트 모듈입니다.
///
/// 어떤 비트도 "인증된 모듈"이나 "승인 모드"를 뜻하지 않습니다. 구현은
/// 인증받지 않았고 키는 승인된 난수 발생기에서 나오지 않습니다.
pub mod indicator {
    /// 이 서비스가 쓴 알고리즘이 FIPS 186-5에 있는 알고리즘임
    pub const FIPS_186_5_ALGORITHM: u64 = 1;
    /// 시작 시 자가 시험을 통과했음
    pub const SELF_TEST_PASSED: u64 = 2;
    /// 키의 시드에 `rng-seed` 32바이트 이상이 들어갔음. 상태 조회에서는
    /// 볼트가 쥐고 있는 시드에 대해서도 알림. 바이트가 있었다는 뜻이지 그
    /// 바이트를 예측할 수 없었다는 뜻은 아님
    pub const SEED_RNG_SEED: u64 = 4;
    /// 키의 시드에 RNDR 난수 256비트가 들어갔음. 상태 조회에서는 볼트가 쥐고
    /// 있는 시드에 대해서도 알림
    pub const SEED_RNDR: u64 = 8;
}

/// 배지가 가리키는 역할을 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// 루트 태스크. 시드를 넘기고 키를 만들게 하고 지우게 할 수 있지만 서명도
    /// 공개키도 받지 못함
    Admin,
    /// 공개키를 받고 서명을 받을 수 있는 클라이언트
    Signer,
    /// 공개키만 받을 수 있는 클라이언트
    Reader,
    /// 직렬 터미널. 알고리즘을 골라 키를 만들게 하고 지우게 하며 공개키를
    /// 받음. 서명은 받지 못하고 시드를 넘기지도 못함
    Operator,
}

/// 역할을 담는 배지의 윗부분
const ROLE_ADMIN: u64 = 0xAD_0000_0000_0000;
const ROLE_SIGNER: u64 = 0x51_0000_0000_0000;
const ROLE_READER: u64 = 0x4E_0000_0000_0000;
const ROLE_OPERATOR: u64 = 0x0E_0000_0000_0000;

/// 관리자(루트 태스크)의 배지
pub const ADMIN_BADGE: u64 = ROLE_ADMIN;
/// 운영자(직렬 터미널)의 배지
pub const OPERATOR_BADGE: u64 = ROLE_OPERATOR;
/// 운영자의 응답 프레임 번호. 클라이언트의 프레임 번호들 바로 뒤
pub const OPERATOR_FRAME: u64 = MAX_CLIENTS + 1;

/// 서명 클라이언트의 배지를 만드는 함수입니다.
///
/// # Arguments
/// `client` - 클라이언트 번호(1부터 [MAX_CLIENTS]까지)
pub const fn signer_badge(client: u64) -> u64 {
    ROLE_SIGNER | client
}

/// 공개키만 받는 클라이언트의 배지를 만드는 함수입니다.
///
/// # Arguments
/// `client` - 클라이언트 번호(1부터 [MAX_CLIENTS]까지)
pub const fn reader_badge(client: u64) -> u64 {
    ROLE_READER | client
}

/// 호출자 하나의 역할과 프레임을 담는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub role: Role,
    /// 요청 프레임의 번호. 본문을 보낼 일이 없는 호출자(운영자)에게는 요청
    /// 프레임이 없음
    pub request: Option<u64>,
    /// 응답 프레임의 번호
    pub response: u64,
}

/// 배지를 호출자로 푸는 함수입니다. 이 표에 없는 배지와 배지 없는 호출(0)은
/// `None`입니다.
///
/// 프레임 번호는 볼트가 그 호출자의 요청 프레임과 응답 프레임을 고르는 데
/// 씁니다. 관리자가 0번, 클라이언트는 자기 번호, 운영자의 응답 프레임은
/// [OPERATOR_FRAME]입니다.
pub const fn caller_of(badge: u64) -> Option<Caller> {
    let client = badge & 0xFFFF_FFFF_FFFF;
    let numbered = client >= 1 && client <= MAX_CLIENTS;
    let (role, request, response) = match badge & !0xFFFF_FFFF_FFFF {
        ROLE_ADMIN if client == 0 => (Role::Admin, Some(0), 0),
        ROLE_SIGNER if numbered => (Role::Signer, Some(client), client),
        ROLE_READER if numbered => (Role::Reader, Some(client), client),
        ROLE_OPERATOR if client == 0 => (Role::Operator, None, OPERATOR_FRAME),
        _ => return None,
    };
    Some(Caller {
        role,
        request,
        response,
    })
}

/// 배지가 가리키는 역할을 주는 함수입니다.
pub const fn role_of(badge: u64) -> Option<Role> {
    match caller_of(badge) {
        Some(c) => Some(c.role),
        None => None,
    }
}

/// 역할이 그 연산을 해도 되는지 알려 주는 함수입니다.
pub const fn allowed(role: Role, op: u64) -> bool {
    match role {
        Role::Admin => matches!(op, request::STATUS | request::INIT | request::KEYGEN | request::ZEROIZE),
        Role::Signer => matches!(op, request::STATUS | request::PUBLIC_KEY | request::SIGN),
        Role::Reader => matches!(op, request::STATUS | request::PUBLIC_KEY),
        Role::Operator => {
            matches!(op, request::STATUS | request::PUBLIC_KEY | request::KEYGEN | request::ZEROIZE)
        }
    }
}

/// 이 요청의 본문을 어느 요청 프레임에서 몇 바이트 옮겨 올지 정하는
/// 함수입니다. `None`이면 볼트는 어떤 프레임도 읽지 않습니다.
///
/// 본문이 있는 연산이고, 그 호출자에게 요청 프레임이 있고, 말한 길이가 0이
/// 아닐 때만 옮깁니다. 그 밖의 요청이 길이를 말했다면 볼트는 빈 본문으로
/// 처리하므로, 말한 길이와 본문의 길이가 달라 요청 전체가 거부됩니다. 요청
/// 프레임이 없는 호출자가 길이를 꾸며 볼트에게 매핑되지 않은 주소를 읽히는
/// 길은 없습니다.
///
/// # Arguments
/// `badge` - 커널이 전달한 호출자의 배지
/// `mr` - 요청의 메시지 레지스터
pub const fn fetch_plan(badge: u64, mr: &[u64; 4]) -> Option<(u64, u64)> {
    let Some(caller) = caller_of(badge) else {
        return None;
    };
    let Some(frame) = caller.request else {
        return None;
    };
    if !request::takes_body(mr[0] & 0xFF) || mr[1] == 0 {
        return None;
    }
    Some((frame, mr[1]))
}

/// 이 요청의 응답 본문을 어느 응답 프레임에 쓸지 정하는 함수입니다. `None`이면
/// 볼트는 어떤 프레임에도 쓰지 않습니다.
///
/// # Arguments
/// `badge` - 커널이 전달한 호출자의 배지
/// `mr` - 요청의 메시지 레지스터
/// `status` - 볼트가 이 요청에 낸 응답의 MR0
pub const fn publish_plan(badge: u64, mr: &[u64; 4], status: u64) -> Option<u64> {
    let Some(caller) = caller_of(badge) else {
        return None;
    };
    if status != status::OK || !request::gives_body(mr[0] & 0xFF) {
        return None;
    }
    Some(caller.response)
}

const _: () = assert!(alg::ED25519 < 16);
