//! 볼트의 요청 처리 상태 기계 모듈입니다.
//!
//! # Features
//! [Vault::handle]은 배지, 요청 레지스터, 요청 본문의 사본을 받아 응답
//! 레지스터와 응답 본문을 만듭니다. 시스템 콜도 프레임 접근도 하지 않으므로
//! 호스트에서 그대로 시험됩니다. 프레임에서 본문을 읽고 쓰는 일은 [fetch]와
//! [publish]가 합니다.
//!
//! 요청 프레임은 클라이언트가 언제든 바꿀 수 있는 메모리입니다. 그래서
//! [fetch]는 길이를 먼저 확정하고 바이트를 한 번씩만 읽어 볼트의 버퍼로
//! 옮기며, 그 뒤로 볼트는 사본만 봅니다. 읽는 도중에 프레임이 바뀌면 사본이
//! 뒤섞일 뿐이고, 그것은 클라이언트가 처음부터 보낼 수 있었던 내용입니다.
//!
//! # Errors
//! 자가 시험이나 쌍 일치 시험이 실패하면 [state::FAILED]가 되어 상태 조회
//! 말고는 전부 [status::FAILED]로 거부합니다.

use crate::proto::{alg, allowed, indicator, request, role_of, state, status, Role};
use crate::proto::{INIT_LEN, MAX_MESSAGE, MAX_RESPONSE, RESPONSE_HEADER};
use crate::provider::{self, Key};

/// `rng-seed`가 이만큼은 들어가야 시드로 키를 만듦(바이트)
const MIN_RNG_SEED: u64 = 32;
/// 커널이 준 시드 정보의 비트(k0-abi의 bootinfo 상수와 같음)
const SEED_RNG_SEED_BYTES: u64 = k0_abi::bootinfo::SEED_RNG_SEED_BYTES;
const SEED_RNDR: u64 = k0_abi::bootinfo::SEED_RNDR;

/// 볼트의 상태를 담는 구조체입니다.
pub struct Vault {
    state: u64,
    key: Key,
    /// 표시 비트 가운데 자가 시험과 시드 재료에 관한 것
    flags: u64,
}

impl Vault {
    /// 자가 시험을 돌리고 볼트를 만드는 함수입니다.
    ///
    /// 시험이 실패하면 오류 상태로 시작해 어떤 서비스도 하지 않습니다.
    pub fn new() -> Self {
        let ok = provider::self_test();
        Vault {
            state: if ok { state::FRESH } else { state::FAILED },
            key: Key::empty(),
            flags: if ok { indicator::SELF_TEST_PASSED } else { 0 },
        }
    }

    /// 요청 하나를 처리하는 함수입니다.
    ///
    /// 응답 레지스터 네 개를 전부 채워 돌려주고(쓰지 않는 값이 새지 않게),
    /// 응답 본문은 `out`의 앞에 씁니다. 본문 길이는 응답의 두 번째 값입니다
    /// (상태 조회에서는 그 자리가 볼트의 상태이고 본문은 없음).
    ///
    /// # Arguments
    /// `badge` - 커널이 전달한 호출자의 배지
    /// `mr` - 요청의 메시지 레지스터
    /// `body` - 요청 프레임에서 옮겨 온 본문의 사본(길이는 이미 확정된 값)
    /// `out` - 응답 본문을 받을 버퍼
    pub fn handle(&mut self, badge: u64, mr: [u64; 4], body: &[u8], out: &mut [u8; MAX_RESPONSE]) -> [u64; 4] {
        let nonce = mr[2];
        let reply = |status: u64, len: u64, ind: u64| [status, len, ind, nonce];
        let Some((role, _)) = role_of(badge) else {
            return reply(status::DENIED, 0, 0);
        };
        let (op, algo) = (mr[0] & 0xFF, (mr[0] >> 8) & 0xFF);
        // 예약 비트가 서 있거나, 말한 길이와 옮겨 온 본문의 길이가 다르면
        // (버퍼보다 긴 본문은 잘려서 옴) 요청 전체를 거부함
        if mr[0] >> 16 != 0 || mr[3] != 0 || mr[1] != body.len() as u64 {
            return reply(status::BAD_REQUEST, 0, 0);
        }
        if !matches!(
            op,
            request::STATUS | request::PUBLIC_KEY | request::SIGN | request::INIT | request::ZEROIZE
        ) {
            return reply(status::BAD_REQUEST, 0, 0);
        }
        if !allowed(role, op) {
            return reply(status::DENIED, 0, 0);
        }
        if op == request::STATUS {
            return reply(status::OK, self.state, self.flags);
        }
        if self.state == state::FAILED {
            return reply(status::FAILED, 0, self.flags);
        }
        match op {
            request::INIT => self.init(role, body, reply),
            request::ZEROIZE => {
                self.key.wipe();
                self.state = state::ZEROIZED;
                scrub_stack();
                reply(status::OK, 0, self.flags)
            }
            _ => self.serve(op, algo, body, out, reply),
        }
    }

    fn init(&mut self, _role: Role, body: &[u8], reply: impl Fn(u64, u64, u64) -> [u64; 4]) -> [u64; 4] {
        if self.state != state::FRESH {
            return reply(status::DENIED, 0, self.flags);
        }
        if body.len() != INIT_LEN {
            return reply(status::BAD_REQUEST, 0, self.flags);
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&body[..32]);
        let mut info = [0u8; 8];
        info.copy_from_slice(&body[32..]);
        let info = u64::from_le_bytes(info);
        let mut seed_flags = 0;
        if info & SEED_RNG_SEED_BYTES >= MIN_RNG_SEED {
            seed_flags |= indicator::SEED_RNG_SEED;
        }
        if info & SEED_RNDR != 0 {
            seed_flags |= indicator::SEED_RNDR;
        }
        if seed_flags == 0 {
            // 재료가 카운터뿐인 시드로는 키를 만들지 않음
            wipe(&mut seed);
            return reply(status::NO_ENTROPY, 0, self.flags);
        }
        let made = provider::keygen(&mut seed);
        wipe(&mut seed);
        scrub_stack();
        match made {
            Some(key) => {
                self.key = key;
                self.state = state::READY;
                self.flags |= seed_flags;
                reply(status::OK, 0, self.flags)
            }
            None => {
                self.state = state::FAILED;
                self.flags &= !indicator::SELF_TEST_PASSED;
                reply(status::FAILED, 0, self.flags)
            }
        }
    }

    fn serve(
        &mut self,
        op: u64,
        algo: u64,
        body: &[u8],
        out: &mut [u8; MAX_RESPONSE],
        reply: impl Fn(u64, u64, u64) -> [u64; 4],
    ) -> [u64; 4] {
        if algo != alg::ED25519 || !provider::HAS_ED25519 {
            return reply(status::UNSUPPORTED, 0, self.flags);
        }
        match self.state {
            state::FRESH => return reply(status::NOT_READY, 0, self.flags),
            state::ZEROIZED => return reply(status::ZEROIZED, 0, self.flags),
            _ => {}
        }
        let ind = self.flags | indicator::FIPS_186_5_ALGORITHM;
        if op == request::PUBLIC_KEY {
            if !body.is_empty() {
                return reply(status::BAD_REQUEST, 0, self.flags);
            }
            let Some(public) = self.key.public() else {
                return reply(status::NOT_READY, 0, self.flags);
            };
            out[..public.len()].copy_from_slice(&public);
            return reply(status::OK, public.len() as u64, ind);
        }
        if body.len() > MAX_MESSAGE {
            return reply(status::BAD_REQUEST, 0, self.flags);
        }
        let signed = self.key.sign(body);
        scrub_stack();
        let Some(sig) = signed else {
            return reply(status::NOT_READY, 0, self.flags);
        };
        out[..sig.len()].copy_from_slice(&sig);
        reply(status::OK, sig.len() as u64, ind)
    }
}

#[cfg(test)]
impl Vault {
    /// 자가 시험이 실패했을 때의 볼트를 만드는 함수입니다(테스트 전용).
    pub fn failed_for_test() -> Self {
        Vault {
            state: state::FAILED,
            key: Key::empty(),
            flags: 0,
        }
    }
}

impl Default for Vault {
    fn default() -> Self {
        Self::new()
    }
}

/// 바이트 배열을 0으로 덮는 함수입니다(최적화로 사라지지 않게 volatile).
pub fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: b는 배열 안의 유효한 바이트임
        unsafe { core::ptr::write_volatile(b, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// 방금 끝난 암호 연산이 쓰던 스택 구간을 덮는 함수입니다.
///
/// 암호 구현은 키에서 나온 중간값을 자기 지역 변수에 두고 돌아옵니다. 그
/// 프레임들이 있던 자리에 같은 깊이의 배열을 잡아 0으로 채웁니다. 구현이 쓴
/// 깊이를 정확히 알 수는 없으므로 최선의 노력입니다. 볼트의 메모리 전체는
/// 볼트가 사라진 뒤 다시 쓰이기 전에 커널이 소거합니다.
#[inline(never)]
fn scrub_stack() {
    let mut pad = [0u8; 8192];
    wipe(&mut pad);
    core::hint::black_box(&pad);
}

/// 요청 프레임에서 본문을 볼트의 버퍼로 옮기는 함수입니다. 옮긴 길이를
/// 반환합니다.
///
/// 길이를 버퍼 크기로 먼저 자르고, 바이트마다 한 번씩만 volatile로 읽습니다.
/// 프레임을 가리키는 참조는 만들지 않습니다.
///
/// # Arguments
/// `frame` - 요청 프레임의 시작 주소
/// `len` - 요청이 말한 본문 길이(레지스터에서 온 값)
/// `dst` - 사본을 받을 버퍼
///
/// # Safety
/// `frame`부터 `dst.len()`바이트가 읽을 수 있게 매핑돼 있어야 합니다.
pub unsafe fn fetch(frame: *const u8, len: u64, dst: &mut [u8]) -> usize {
    let n = usize::try_from(len).unwrap_or(usize::MAX).min(dst.len());
    for (i, b) in dst[..n].iter_mut().enumerate() {
        // SAFETY: 함수 계약대로 i는 매핑된 범위 안임
        *b = unsafe { frame.add(i).read_volatile() };
    }
    n
}

/// 응답 프레임에 앞머리(일회용 번호와 길이)와 본문을 쓰는 함수입니다.
///
/// # Arguments
/// `frame` - 응답 프레임의 시작 주소
/// `nonce` - 요청의 일회용 번호
/// `body` - 응답 본문
///
/// # Safety
/// `frame`부터 [RESPONSE_HEADER] + [MAX_RESPONSE]바이트가 쓸 수 있게 매핑돼
/// 있어야 하고 8바이트로 정렬돼 있어야 합니다.
pub unsafe fn publish(frame: *mut u8, nonce: u64, body: &[u8]) {
    let n = body.len().min(MAX_RESPONSE);
    // SAFETY: 함수 계약대로 범위 안이고 정렬돼 있음. 본문을 먼저 쓰고 번호를
    //         마지막에 써서, 번호가 맞는 프레임에는 그 요청의 본문이 다 있음
    unsafe {
        frame.cast::<u64>().write_volatile(0);
        frame.add(8).cast::<u64>().write_volatile(n as u64);
        for (i, b) in body[..n].iter().enumerate() {
            frame.add(RESPONSE_HEADER + i).write_volatile(*b);
        }
        frame.cast::<u64>().write_volatile(nonce);
    }
}
