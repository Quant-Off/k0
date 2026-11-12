//! 키 볼트(서명 오라클)를 띄우고 검증하는 모듈입니다.
//!
//! # Features
//! 루트 태스크가 볼트 태스크 하나와 클라이언트 태스크들을 각자의 주소 공간에
//! 만들고, 엔드포인트 하나와 클라이언트마다의 요청 프레임, 응답 프레임으로
//! 잇습니다. 볼트와 클라이언트의 코드는 이 이미지 안에 있고 프로토콜과 암호
//! 경계는 `k0-vault` 크레이트에 있습니다.
//!
//! 배선이 곧 정책입니다. 클라이언트의 CNode에는 볼트 엔드포인트의 송신 전용
//! 복사본(배지 포함)과 보고용 엔드포인트만 들어갑니다. 프레임은 루트가 직접
//! 매핑해 주고 프레임 케이퍼빌리티는 넘기지 않습니다. 요청 프레임은
//! 클라이언트에 읽기/쓰기, 볼트에 읽기 전용으로, 응답 프레임은 그 반대로
//! 매핑합니다. 볼트의 스택(키가 사는 곳)은 볼트의 주소 공간에만 있습니다.
//! 루트 태스크는 이 모든 것을 만든 쪽이므로 격리의 바깥에 있습니다.
//!
//! 격리 태스크에는 이미지의 `.data`와 `.bss`가 매핑되지 않으므로, 볼트와
//! 클라이언트의 진입점에서는 static을 쓰지 않습니다(출력과 `check`도 못 씀).
//! 결과는 보고용 엔드포인트로 루트에 알립니다.

use k0_vault::proto::{self, alg, request, state, status, MAX_MESSAGE, MAX_RESPONSE, RESPONSE_HEADER};
use k0_vault::provider;
use k0_vault::server::{fetch, publish, wipe, Vault};

use super::{
    call, cap_copy, cap_delete, cap_revoke, check, copy_here, desc, err, frames_in, ipc, make_aspace, map,
    map_into, obj, perm, put_str, recv, reply_recv, retype, rights, send, take_boot_seed, tcb_configure,
    tcb_resume, yield_now, Boot, TEST_VA,
};

/// 볼트와 클라이언트의 스택(각자의 주소 공간에서 같은 VA). 곡선 연산이 깊어
/// 격리 검증의 한 장짜리 스택으로는 모자람
const STACK_BASE: u64 = 0x52_0000;
const STACK_SIZE: u64 = 0x8000;
/// 볼트가 n번 호출자의 요청 프레임과 응답 프레임을 보는 VA(n = 0은 관리자)
const VAULT_REQ: u64 = 0x54_0000;
const VAULT_RESP: u64 = 0x58_0000;
const FRAME_STEP: u64 = 0x8000;
/// 클라이언트가 자기 요청 프레임과 응답 프레임을 보는 VA
const CLIENT_REQ: u64 = VAULT_REQ;
const CLIENT_RESP: u64 = VAULT_RESP;
/// 루트가 관리자용 프레임과, 회수한 메모리를 다시 볼 때 쓰는 VA
const ROOT_REQ: u64 = TEST_VA + 0x64 * 0x4000;
const ROOT_RESP: u64 = ROOT_REQ + 0x4000;
const ROOT_PEEK: u64 = ROOT_REQ + 0x8000;

/// 클라이언트가 볼트를 기다리는 한도(틱)
const WAIT: u64 = ipc::timeout(4);
/// 클라이언트의 보고가 성공임을 뜻하는 값
const REPORT_OK: u64 = 0x0C;
const REPORT_TAG: u64 = 0x0AC1E;

/// 클라이언트가 하는 일
const MODE_SIGNER: u64 = 0;
const MODE_READER: u64 = 1;
const MODE_WRITE_RESPONSE: u64 = 2;
const MODE_READ_FOREIGN: u64 = 3;
const MODE_AFTER_ZEROIZE: u64 = 4;

/// 볼트 태스크의 진입점입니다.
///
/// 자가 시험을 돌린 뒤 요청을 하나씩 처리합니다. 요청 본문은 호출자의 요청
/// 프레임에서 한 번만 옮겨 오고, 응답 본문은 성공했을 때만 그 호출자의 응답
/// 프레임에 씁니다. 응답 레지스터 네 개는 항상 전부 채웁니다.
extern "C" fn vault_entry(_arg: u64) -> ! {
    let mut vault = Vault::new();
    let mut body = [0u8; MAX_MESSAGE];
    let mut out = [0u8; MAX_RESPONSE];
    let mut r = recv(1, 0);
    loop {
        if r.status != 0 {
            // 엔드포인트가 사라졌으면 할 일이 없음
            loop {
                yield_now();
            }
        }
        let reply = match proto::role_of(r.badge) {
            Some((_, index)) => {
                let req = (VAULT_REQ + index * FRAME_STEP) as *const u8;
                // SAFETY: 루트가 이 호출자의 요청 프레임을 이 VA에 읽기 전용으로
                //         매핑해 두었고 버퍼는 프레임보다 작음
                let n = unsafe { fetch(req, r.mr[1], &mut body) };
                let reply = vault.handle(r.badge, r.mr, &body[..n], &mut out);
                wipe(&mut body[..n]);
                if reply[0] == status::OK && r.mr[0] & 0xFF != request::STATUS {
                    let resp = (VAULT_RESP + index * FRAME_STEP) as *mut u8;
                    let len = (reply[1] as usize).min(MAX_RESPONSE);
                    // SAFETY: 루트가 이 호출자의 응답 프레임을 이 VA에 쓰기
                    //         가능으로 매핑해 두었고 프레임은 정렬돼 있음
                    unsafe { publish(resp, r.mr[2], &out[..len]) };
                }
                reply
            }
            None => vault.handle(r.badge, r.mr, &[], &mut out),
        };
        r = reply_recv(1, 0, reply);
    }
}

/// 응답 프레임에서 본문을 읽는 함수입니다. 프레임의 일회용 번호가 자기
/// 번호와 다르거나 길이가 맞지 않으면 `false`
fn read_response(nonce: u64, want: &mut [u8]) -> bool {
    let frame = CLIENT_RESP as *const u8;
    // SAFETY: 루트가 이 VA에 응답 프레임을 읽기 전용으로 매핑해 두었음
    unsafe {
        if frame.cast::<u64>().read_volatile() != nonce
            || frame.add(8).cast::<u64>().read_volatile() != want.len() as u64
        {
            return false;
        }
        for (i, b) in want.iter_mut().enumerate() {
            *b = frame.add(RESPONSE_HEADER + i).read_volatile();
        }
    }
    true
}

/// 요청 프레임에 메시지를 쓰는 함수입니다.
fn write_request(msg: &[u8]) {
    for (i, b) in msg.iter().enumerate() {
        // SAFETY: 루트가 이 VA에 요청 프레임을 쓰기 가능으로 매핑해 두었음
        unsafe { (CLIENT_REQ as *mut u8).add(i).write_volatile(*b) };
    }
}

/// 볼트를 한 번 호출하는 함수입니다. 응답이 번호를 그대로 돌려주지 않으면
/// 상태를 믿지 않습니다.
fn ask(op: u64, algo: u64, len: u64, nonce: u64) -> (u64, u64, u64) {
    let r = call(1, WAIT, [request::word(op, algo), len, nonce, 0]);
    if r.status != 0 || r.mr[3] != nonce {
        return (u64::MAX, 0, 0);
    }
    (r.mr[0], r.mr[1], r.mr[2])
}

/// 서명 클라이언트의 정상 경로 함수입니다. 실패한 단계의 번호를 주고 전부
/// 통과하면 0
fn signer_path() -> u64 {
    let (st, vault_state, _) = ask(request::STATUS, alg::NONE, 0, 1);
    if st != status::OK || vault_state != state::READY {
        return 1;
    }
    let msg = b"k0 signing oracle self test message";
    write_request(msg);
    if !provider::HAS_ED25519 {
        // 알고리즘 없이 빌드한 이미지: 서명과 공개키는 지원하지 않음으로 거부
        let unsupported = ask(request::SIGN, alg::ED25519, msg.len() as u64, 2).0 == status::UNSUPPORTED
            && ask(request::PUBLIC_KEY, alg::ED25519, 0, 3).0 == status::UNSUPPORTED;
        return if unsupported { 0 } else { 2 };
    }
    let mut public = [0u8; provider::PUBLIC_KEY_LEN];
    if ask(request::PUBLIC_KEY, alg::ED25519, 0, 2).0 != status::OK || !read_response(2, &mut public) {
        return 3;
    }
    let mut sig = [0u8; provider::SIGNATURE_LEN];
    if ask(request::SIGN, alg::ED25519, msg.len() as u64, 3).0 != status::OK || !read_response(3, &mut sig) {
        return 4;
    }
    if !provider::verify(&public, msg, &sig) {
        return 5;
    }
    // 한 글자 다른 메시지와 한 비트 다른 서명은 검증되지 않아야 함
    let mut other = *msg;
    other[0] ^= 1;
    let mut bad = sig;
    bad[10] ^= 4;
    if provider::verify(&public, &other, &sig) || provider::verify(&public, msg, &bad) {
        return 6;
    }
    // 길이를 프레임보다 크게 말하거나 모르는 알고리즘을 대면 거부됨
    if ask(request::SIGN, alg::ED25519, MAX_MESSAGE as u64 + 1, 4).0 != status::BAD_REQUEST
        || ask(request::SIGN, 9, 1, 5).0 != status::UNSUPPORTED
        || ask(request::INIT, alg::NONE, 0, 6).0 != status::DENIED
        || ask(request::ZEROIZE, alg::NONE, 0, 7).0 != status::DENIED
    {
        return 7;
    }
    0
}

/// 클라이언트 태스크의 진입점입니다. 1번 슬롯이 볼트, 2번 슬롯이 보고용
/// 엔드포인트입니다.
///
/// # Arguments
/// `mode` - 할 일(`MODE_*`)
extern "C" fn client_entry(mode: u64) -> ! {
    let failed = match mode {
        MODE_SIGNER => signer_path(),
        MODE_READER => {
            let key_ok = if provider::HAS_ED25519 {
                let mut public = [0u8; provider::PUBLIC_KEY_LEN];
                ask(request::PUBLIC_KEY, alg::ED25519, 0, 1).0 == status::OK && read_response(1, &mut public)
            } else {
                true
            };
            // 공개키는 받지만 서명은 이 배지에 허용되지 않음
            let denied = ask(request::SIGN, alg::ED25519, 1, 2).0 == status::DENIED;
            u64::from(!(key_ok && denied))
        }
        MODE_WRITE_RESPONSE => {
            // 응답 프레임은 클라이언트에 읽기 전용이라 쓰면 이 태스크가 종료됨
            // SAFETY: 의도된 위반
            unsafe { (CLIENT_RESP as *mut u64).write_volatile(0xBAD) };
            9
        }
        MODE_READ_FOREIGN => {
            // 볼트가 다른 호출자의 요청 프레임을 보는 자리는 이 주소 공간에
            // 매핑돼 있지 않아 읽으면 이 태스크가 종료됨
            // SAFETY: 의도된 위반
            let leaked = unsafe { ((VAULT_REQ + FRAME_STEP) as *const u64).read_volatile() };
            9 + (leaked & 1)
        }
        _ => {
            let gone = !provider::HAS_ED25519
                || (ask(request::SIGN, alg::ED25519, 1, 1).0 == status::ZEROIZED
                    && ask(request::PUBLIC_KEY, alg::ED25519, 0, 2).0 == status::ZEROIZED);
            u64::from(!gone)
        }
    };
    let code = if failed == 0 { REPORT_OK } else { 0xF0 + failed };
    send(2, 0, [code, REPORT_TAG, mode, 0]);
    loop {
        yield_now();
    }
}

/// 이 검증이 쓸 untyped를 고르는 함수입니다. 다른 검증이 쓰지 않는 것 중 가장
/// 큰 것이고, 그런 것이 없으면 0
fn spare_untyped(b: &Boot) -> u64 {
    // SAFETY: 커널이 이 VA에 bootinfo 페이지를 RO로 매핑하고 내용을 채웠음
    let cap_count = unsafe { (*(super::bootinfo::VA as *const super::bootinfo::Header)).cap_count };
    let (mut best, mut best_frames) = (0, 0);
    for i in 0..cap_count {
        let d = desc(i);
        if d.kind != super::bootinfo::cap_kind::UNTYPED || i == b.ut_main || i == b.ut_small || i == b.ut_reuse {
            continue;
        }
        let n = frames_in(d, b.frame_size);
        if n >= 512 && n > best_frames {
            best = i;
            best_frames = n;
        }
    }
    best
}

/// 주소 공간 하나와 그 안의 스택을 만드는 함수입니다. 주소 공간 슬롯을 줍니다.
fn make_space(ut: u64, b: &Boot) -> i64 {
    let (aspace, _) = make_aspace(ut, b);
    let mut va = STACK_BASE;
    while va < STACK_BASE + STACK_SIZE {
        let f = retype(ut, obj::FRAME);
        check("oracle stack", f > 0 && map_into(f, va, perm::RW, aspace) == 0);
        va += b.frame_size;
    }
    aspace
}

/// 프레임 하나를 만들어 한쪽에는 쓰기 가능으로, 다른 쪽에는 읽기 전용으로
/// 매핑하는 함수입니다. 읽기 쪽에는 권한을 줄인 복사본을 써서, 그 매핑을
/// 쓰기 가능으로 바꿀 길이 없게 합니다.
///
/// # Arguments
/// `rw` - (주소 공간 슬롯, VA). 주소 공간이 0이면 루트 자신
/// `ro` - 읽기 전용으로 볼 쪽
fn link(ut: u64, rw: (i64, u64), ro: (i64, u64)) {
    let frame = retype(ut, obj::FRAME);
    let view = copy_here(frame, rights::R, 0);
    check("oracle frame", frame > 0 && view > 0);
    let put = |cap: i64, (aspace, va): (i64, u64), p: u64| {
        if aspace == 0 { map(cap, va, p) } else { map_into(cap, va, p, aspace) }
    };
    check("oracle frame rw", put(frame, rw, perm::RW) == 0);
    check("oracle frame ro", put(view, ro, perm::RO) == 0);
}

/// 태스크 하나를 만들어 실행 대기 상태로 만드는 함수입니다. TCB 슬롯을 줍니다.
fn spawn(ut: u64, aspace: i64, entry: u64, arg: u64, caps: &[(i64, u64, u64)]) -> i64 {
    let cnode = retype(ut, obj::CNODE);
    check("oracle cnode", cnode > 0);
    for (i, &(src, mask, badge)) in caps.iter().enumerate() {
        check("oracle grant", cap_copy(src, cnode, 1 + i as u64, mask, badge) == 0);
    }
    let tcb = retype(ut, obj::TCB);
    check(
        "oracle task",
        tcb > 0
            && tcb_configure(tcb, entry, STACK_BASE + STACK_SIZE, aspace, cnode, arg) == 0
            && tcb_resume(tcb) == 0,
    );
    tcb
}

/// 클라이언트 태스크 하나를 클라이언트 주소 공간에 만드는 함수입니다. TCB
/// 슬롯을 줍니다.
///
/// 루트 CNode의 슬롯을 아끼려고 클라이언트들은 주소 공간과 스택, 프레임 한
/// 쌍을 차례로 물려 씁니다. 그래서 한 번에 하나만 살아 있어야 하고, 호출자는
/// 다음 클라이언트를 만들기 전에 앞의 것을 삭제합니다. 여기서 보려는 것은
/// 클라이언트와 볼트 사이의 격리입니다(클라이언트끼리의 격리는 격리 검증이
/// 이미 다룸).
fn client(ut: u64, space: i64, ep: i64, report: i64, badge: u64, mode: u64) -> i64 {
    spawn(ut, space, client_entry as usize as u64, mode, &[(ep, rights::W, badge), (report, rights::W, 0)])
}

/// 서명 오라클을 띄우고 검증하는 함수입니다.
pub fn test_oracle(b: &Boot) {
    let spare = spare_untyped(b);
    let ut = if spare != 0 { spare } else { b.ut_main };
    let (ep, report) = (retype(ut, obj::ENDPOINT), retype(ut, obj::ENDPOINT));
    check("oracle endpoints", ep > 0 && report > 0);

    // 볼트: 자기 주소 공간, 스택, 수신 전용 엔드포인트. 관리자(루트)의 프레임 둘
    let vault_space = make_space(ut, b);
    link(ut, (0, ROOT_REQ), (vault_space, VAULT_REQ));
    link(ut, (vault_space, VAULT_RESP), (0, ROOT_RESP));
    let vault_tcb = spawn(ut, vault_space, vault_entry as usize as u64, 0, &[(ep, rights::R, 0)]);
    let admin = copy_here(ep, rights::W, proto::ADMIN_BADGE);
    check("oracle admin cap", admin > 0);
    let ask = |cap: i64, op: u64, len: u64, nonce: u64| {
        let r = call(cap, WAIT, [request::word(op, alg::NONE), len, nonce, 0]);
        (if r.status == 0 && r.mr[3] == nonce { r.mr[0] } else { u64::MAX }, r.mr[1], r.mr[2])
    };

    // 자가 시험을 통과한 볼트는 아직 키가 없는 상태임. 배지 없는 호출은 거부됨
    let (st, vault_state, ind) = ask(admin, request::STATUS, 0, 1);
    check("vault self test", st == status::OK && vault_state == state::FRESH);
    check("vault self test indicator", ind == proto::indicator::SELF_TEST_PASSED);
    check("call without a badge refused", ask(ep, request::STATUS, 0, 2).0 == status::DENIED);

    // 커널이 준 부트 시드를 관리자 프레임으로 넘겨 키를 만들게 하고, 루트 쪽
    // 사본은 지움. 재료가 모자란 시드로는 볼트가 키를 만들지 않음
    let (mut seed, info) = take_boot_seed();
    for (i, byte) in seed.iter().chain(&info.to_le_bytes()).enumerate() {
        // SAFETY: 방금 이 VA에 관리자 요청 프레임을 쓰기 가능으로 매핑했음
        unsafe { (ROOT_REQ as *mut u8).add(i).write_volatile(*byte) };
    }
    wipe(&mut seed);
    let init = ask(admin, request::INIT, proto::INIT_LEN as u64, 3).0;
    for i in 0..proto::INIT_LEN {
        // SAFETY: 위와 같음
        unsafe { (ROOT_REQ as *mut u8).add(i).write_volatile(0) };
    }
    if init == status::NO_ENTROPY {
        check("no key without entropy", ask(admin, request::STATUS, 0, 4).1 == state::FRESH);
        check("cleanup", cap_delete(vault_tcb) == 0);
        put_str("root: oracle made no key (boot seed has no entropy)\n");
        return;
    }
    check("vault key made", init == status::OK);
    check("second init refused", ask(admin, request::INIT, proto::INIT_LEN as u64, 5).0 == status::DENIED);
    check("admin cannot sign", ask(admin, request::SIGN, 1, 6).0 == status::DENIED);

    // 클라이언트들의 주소 공간과 1번 호출자의 프레임 한 쌍
    let space = make_space(ut, b);
    link(ut, (space, CLIENT_REQ), (vault_space, VAULT_REQ + FRAME_STEP));
    link(ut, (vault_space, VAULT_RESP + FRAME_STEP), (space, CLIENT_RESP));
    // 보고하는 클라이언트는 보고를 받은 뒤에, 위반하는 클라이언트는 보고 없이
    // 종료된 것을 확인한 뒤에 삭제함
    let reports = |mode: u64, badge: u64| {
        let tcb = client(ut, space, ep, report, badge, mode);
        let r = recv(report, ipc::timeout(8));
        check("oracle client report", r.status == 0 && r.mr == [REPORT_OK, REPORT_TAG, mode, 0]);
        check("oracle client delete", cap_delete(tcb) == 0);
    };
    let dies = |mode: u64| {
        let tcb = client(ut, space, ep, report, proto::signer_badge(1), mode);
        for _ in 0..8 {
            yield_now();
        }
        check("violator stayed silent", recv(report, ipc::NONBLOCK).status == err::WOULD_BLOCK);
        check("violator is dead", tcb_resume(tcb) == err::BAD_STATE);
        check("oracle client delete", cap_delete(tcb) == 0);
    };
    reports(MODE_SIGNER, proto::signer_badge(1));
    reports(MODE_READER, proto::reader_badge(1));
    dies(MODE_WRITE_RESPONSE);
    dies(MODE_READ_FOREIGN);

    // 관리자가 키를 지우게 하면 볼트는 더는 서명하지 않음
    check("zeroize", ask(admin, request::ZEROIZE, 0, 7).0 == status::OK);
    check("vault zeroized", ask(admin, request::STATUS, 0, 8).1 == state::ZEROIZED);
    reports(MODE_AFTER_ZEROIZE, proto::signer_badge(1));

    // 볼트가 사라진 뒤의 호출은 영원히 기다리지 않고 기한에 끝남
    check("delete vault", cap_delete(vault_tcb) == 0);
    let r = call(admin, ipc::timeout(1), [request::word(request::STATUS, alg::NONE), 0, 9, 0]);
    check("call to a dead vault ends", r.status == err::TIMEOUT);

    // 볼트와 클라이언트가 쓰던 메모리를 통째로 회수해 다시 재분류하면 전부
    // 0으로 읽힘. 다른 검증과 untyped를 나눠 쓰는 배치에서는 회수할 수 없음
    if spare != 0 {
        check("revoke oracle memory", cap_revoke(ut as i64) == 0);
        for _ in 0..(4 * STACK_SIZE / b.frame_size) {
            let f = retype(ut, obj::FRAME);
            check("oracle memory again", f > 0 && map(f, ROOT_PEEK, perm::RO) == 0);
            let words = (b.frame_size / 8) as usize;
            // SAFETY: 방금 이 VA에 프레임 하나를 읽기 전용으로 매핑했음
            let clean = (0..words).all(|i| unsafe { (ROOT_PEEK as *const u64).add(i).read_volatile() } == 0);
            check("oracle memory zeroed", clean);
            check("oracle peek unmap", super::unmap(f) == 0);
        }
    }
    put_str("root: oracle tests pass\n");
}
