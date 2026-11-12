//! 볼트와 클라이언트가 공유하는 프로토콜 정의 모듈입니다.
//!
//! # Features
//! 클라이언트는 볼트의 엔드포인트에 `CALL`합니다. 요청의 메시지 레지스터는
//! MR0 = 연산과 알고리즘([request]), MR1 = 요청 프레임에 둔 바이트 수, MR2 =
//! 클라이언트가 고른 일회용 번호입니다. 응답은 MR0 = [status], MR1 = 응답
//! 프레임에 쓴 바이트 수(상태 조회에서는 볼트의 [state]), MR2 = [indicator],
//! MR3 = 요청의 일회용 번호입니다. 볼트는 네 레지스터를 항상 전부 채웁니다.
//!
//! 본문은 프레임으로 오갑니다. 요청 프레임은 클라이언트만 쓰고 볼트는 읽기만
//! 하며, 응답 프레임은 그 반대입니다. 응답 프레임의 앞 [RESPONSE_HEADER]
//! 바이트는 일회용 번호와 길이이고 그 뒤가 본문입니다. 클라이언트는 응답이
//! 성공이고 프레임의 번호가 자기 번호와 같을 때만 본문을 믿어야 합니다
//! (타임아웃으로 버린 예전 요청의 답이 뒤늦게 쓰일 수 있음).
//!
//! 누가 무엇을 할 수 있는지는 엔드포인트 배지로 정하고 빌드 때 고정됩니다
//! ([role_of]). 배지는 커널이 케이퍼빌리티에서 읽어 전달하므로 클라이언트가
//! 꾸밀 수 없습니다. 정책을 바꾸는 연산은 없습니다.

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
    /// 시드를 주어 키를 만들게 하는 연산(관리자 전용, 한 번만)
    pub const INIT: u64 = 4;
    /// 키를 지우게 하는 연산(관리자 전용)
    pub const ZEROIZE: u64 = 5;

    /// 연산과 알고리즘을 MR0 값으로 묶는 함수입니다.
    pub const fn word(op: u64, alg: u64) -> u64 {
        op | alg << 8
    }
}

/// 알고리즘 번호 모듈입니다.
pub mod alg {
    /// 알고리즘을 가리지 않는 연산에 쓰는 값
    pub const NONE: u64 = 0;
    /// Ed25519(PureEdDSA, FIPS 186-5와 RFC 8032)
    pub const ED25519: u64 = 1;
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
    /// 시드의 재료가 키를 만들기에 모자람
    pub const NO_ENTROPY: u64 = 7;
}

/// 볼트의 상태(상태 조회 응답의 MR1) 모듈입니다.
pub mod state {
    pub const FRESH: u64 = 0;
    pub const READY: u64 = 1;
    pub const ZEROIZED: u64 = 2;
    pub const FAILED: u64 = 3;
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
    /// 키의 시드에 `rng-seed` 32바이트 이상이 들어갔음
    pub const SEED_RNG_SEED: u64 = 4;
    /// 키의 시드에 RNDR 난수 256비트가 들어갔음
    pub const SEED_RNDR: u64 = 8;
}

/// 배지가 가리키는 역할을 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// 루트 태스크. 키를 만들게 하고 지우게 할 수 있지만 서명은 받지 못함
    Admin,
    /// 공개키를 받고 서명을 받을 수 있는 클라이언트
    Signer,
    /// 공개키만 받을 수 있는 클라이언트
    Reader,
}

/// 역할을 담는 배지의 윗부분
const ROLE_ADMIN: u64 = 0xAD_0000_0000_0000;
const ROLE_SIGNER: u64 = 0x51_0000_0000_0000;
const ROLE_READER: u64 = 0x4E_0000_0000_0000;

/// 관리자(루트 태스크)의 배지
pub const ADMIN_BADGE: u64 = ROLE_ADMIN;

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

/// 배지를 (역할, 프레임 번호)로 푸는 함수입니다. 이 표에 없는 배지와 배지
/// 없는 호출(0)은 `None`입니다.
///
/// 프레임 번호는 볼트가 그 호출자의 요청 프레임과 응답 프레임을 고르는 데
/// 씁니다. 관리자가 0번이고 클라이언트는 자기 번호입니다.
pub const fn role_of(badge: u64) -> Option<(Role, u64)> {
    let client = badge & 0xFFFF_FFFF_FFFF;
    match badge & !0xFFFF_FFFF_FFFF {
        ROLE_ADMIN if client == 0 => Some((Role::Admin, 0)),
        ROLE_SIGNER if client >= 1 && client <= MAX_CLIENTS => Some((Role::Signer, client)),
        ROLE_READER if client >= 1 && client <= MAX_CLIENTS => Some((Role::Reader, client)),
        _ => None,
    }
}

/// 역할이 그 연산을 해도 되는지 알려 주는 함수입니다.
pub const fn allowed(role: Role, op: u64) -> bool {
    match role {
        Role::Admin => matches!(op, request::STATUS | request::INIT | request::ZEROIZE),
        Role::Signer => matches!(op, request::STATUS | request::PUBLIC_KEY | request::SIGN),
        Role::Reader => matches!(op, request::STATUS | request::PUBLIC_KEY),
    }
}
