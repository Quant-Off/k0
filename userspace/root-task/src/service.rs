//! 자가 검증 뒤에 시작하는 서비스(키 볼트와 운영자 터미널) 모듈입니다.
//!
//! # Features
//! 자가 검증을 모두 통과한 루트 태스크가 서비스를 새로 띄웁니다. 볼트가 든
//! 이미지에서는 볼트 태스크를 자기 주소 공간에 만들어 커널이 준 부트 시드를
//! 넘기고, 터미널 태스크를 또 다른 주소 공간에 만듭니다. 줄 편집과 명령
//! 해석은 `k0-term` 크레이트가 하고 이 모듈은 그것을 시스템 콜에 잇습니다.
//!
//! 배선이 곧 정책입니다. 터미널의 CNode에는 케이퍼빌리티가 둘뿐입니다. 입력과
//! 출력 권한이 있는 Console 복사본, 그리고 볼트 엔드포인트의 송신 전용
//! 복사본(운영자 배지)입니다. 운영자 배지로 볼트가 해 주는 일은 상태 조회,
//! 공개키, 키 생성, 소거뿐이고 서명은 없습니다. 볼트가 공개키를 쓰는 응답
//! 프레임은 터미널에 읽기 전용으로 매핑됩니다. 직렬 선에는 인증이 없으므로,
//! 선에 닿는 사람은 누구나 이 운영자 권한을 갖습니다(그 이상은 갖지 못함).
//!
//! 진짜 터미널을 띄우기 전에, 터미널과 똑같이 배선한 적대적 태스크를 먼저
//! 돌립니다. 그 태스크는 볼트에 온갖 요청을 보내고, 출력 필터를 두드리고,
//! 읽기 전용 프레임에 쓰고, 루트의 데이터를 읽으려 합니다. 터미널이 장악돼도
//! 운영자 권한 밖으로 나가지 못한다는 것을 배선 그대로 확인하는 검증입니다.
//!
//! 서비스가 시작되면 루트 태스크는 아무도 쥐지 않은 엔드포인트에서 영원히
//! 기다립니다. 터미널이나 볼트가 죽어도 다시 띄우지 않습니다.

use k0_abi::console::{FLUSH, NONBLOCK};
use k0_term::Term;

use super::{
    cap_copy, cap_delete, check, console_read, err, make_aspace, map_into, obj, perm, put_str, putc, recv,
    retype, rights, slot, sys1, syscall, tcb_configure, tcb_resume, yield_now, Boot,
};

/// 서비스 태스크의 스택(각자의 주소 공간에서 같은 VA)
const STACK_BASE: u64 = 0x52_0000;
const STACK_SIZE: u64 = 0x8000;
/// 터미널 CNode에서 Console이 드는 슬롯
const CON: u64 = 1;

fn say(text: &str) {
    for b in text.bytes() {
        putc(CON, b);
    }
}

fn exit(code: u64) -> ! {
    sys1(syscall::EXIT, code);
    loop {
        core::hint::spin_loop();
    }
}

use k0_term::Out;

/// 터미널이 글자를 내보내는 통로입니다. 자기 CNode의 Console로 한 바이트씩
/// 냅니다. 이 케이퍼빌리티에는 입력 권한이 있어 백스페이스가 그대로 나갑니다.
struct Con;

impl Out for Con {
    fn put(&mut self, b: u8) {
        putc(CON, b);
    }
}

/// 볼트로 가는 터미널 쪽 통로입니다(볼트가 든 이미지에서만).
#[cfg(feature = "vault")]
mod port {
    use k0_term::{wire, Alg, Fail, Status, Vault, KEY_MAX};
    use k0_vault::proto::{alg, request, RESPONSE_HEADER};

    use super::super::{call, err, ipc};

    /// 터미널 CNode에서 볼트 엔드포인트가 드는 슬롯
    pub const VAULT: i64 = 2;
    /// 터미널이 운영자 응답 프레임을 보는 VA
    pub const RESP: u64 = 0x5C_0000;
    /// 볼트의 답을 기다리는 한도(틱)
    const WAIT: u64 = ipc::timeout(5);

    /// 볼트를 부르는 상태입니다. 요청마다 새 번호를 써서, 기한이 지나 버린
    /// 예전 요청의 답을 지금 요청의 답으로 읽지 않게 합니다.
    pub struct Port {
        pub nonce: u64,
    }

    impl Port {
        fn ask(&mut self, op: u64, algo: u64) -> Result<[u64; 4], Fail> {
            self.nonce += 1;
            let r = call(VAULT, WAIT, [request::word(op, algo), 0, self.nonce, 0]);
            match r.status {
                0 => {}
                e if e == err::TIMEOUT => return Err(Fail::VaultTimeout),
                e if e == err::REPLY_TIMEOUT => return Err(Fail::VaultUnanswered),
                _ => return Err(Fail::NoVault),
            }
            // 자기 번호를 돌려주지 않는 답은 믿지 않음
            if r.mr[3] != self.nonce {
                return Err(Fail::NoVault);
            }
            match wire::fail_of(r.mr[0]) {
                Some(f) => Err(f),
                None => Ok(r.mr),
            }
        }
    }

    impl Vault for Port {
        fn status(&mut self) -> Result<Status, Fail> {
            let mr = self.ask(request::STATUS, alg::NONE)?;
            wire::status_of(mr[1], mr[2]).ok_or(Fail::VaultFailed)
        }

        fn pubkey(&mut self, key: &mut [u8; KEY_MAX]) -> Result<usize, Fail> {
            // 공개키 요청은 알고리즘을 대야 하므로 지금 있는 키의 것을 먼저 물음
            let st = self.status()?;
            let Some(a) = st.alg else {
                return Err(wire::no_key(st.state));
            };
            let mr = self.ask(request::PUBLIC_KEY, wire::alg_number(a))?;
            let len = usize::try_from(mr[1]).unwrap_or(usize::MAX);
            if len > KEY_MAX {
                return Err(Fail::Denied);
            }
            let frame = RESP as *const u8;
            // SAFETY: 루트가 이 VA에 운영자 응답 프레임을 읽기 전용으로 매핑해
            //         두었고, 앞머리와 본문은 프레임 하나보다 작음
            unsafe {
                // 프레임의 번호와 길이가 이 요청의 것일 때만 본문을 믿음
                if frame.cast::<u64>().read_volatile() != self.nonce
                    || frame.add(8).cast::<u64>().read_volatile() != len as u64
                {
                    return Err(Fail::NoVault);
                }
                for (i, b) in key[..len].iter_mut().enumerate() {
                    *b = frame.add(RESPONSE_HEADER + i).read_volatile();
                }
            }
            Ok(len)
        }

        fn keygen(&mut self, a: Alg) -> Result<(), Fail> {
            self.ask(request::KEYGEN, wire::alg_number(a)).map(|_| ())
        }

        fn zeroize(&mut self) -> Result<(), Fail> {
            self.ask(request::ZEROIZE, alg::NONE).map(|_| ())
        }
    }
}

/// 터미널 태스크의 진입점입니다.
///
/// 입력 경로를 열고 그동안 들어온 것을 버린 뒤 시작 줄을 내고, 한 번 더 버린
/// 다음 프롬프트를 내고 바이트를 하나씩 읽어 터미널에 넘깁니다. 그래서
/// 프롬프트가 뜨기 전에 UART에 도착한 것은 명령으로 읽히지 않습니다(장치
/// 밖에서 붙들려 있다가 나중에 도착하는 바이트는 커널이 구별할 수 없음). 입력
/// 경로가 꺼지면 한 줄을 남기고 끝납니다.
extern "C" fn term_entry(_arg: u64) -> ! {
    let mut out = Con;
    let mut term = Term::new();
    let _ = console_read(CON, NONBLOCK | FLUSH);
    for b in k0_term::BANNER.bytes() {
        putc(CON, b);
    }
    // 시작 줄을 내는 동안 들어온 것까지 버린 다음에 프롬프트를 냄
    let _ = console_read(CON, NONBLOCK | FLUSH);
    for b in k0_term::PROMPT.bytes() {
        putc(CON, b);
    }
    #[cfg(feature = "vault")]
    let mut vault = port::Port { nonce: 0 };
    loop {
        let (status, byte, lost) = console_read(CON, 0);
        if status != 0 {
            break;
        }
        #[cfg(feature = "vault")]
        term.feed(byte, lost, &mut out, Some(&mut vault));
        #[cfg(not(feature = "vault"))]
        term.feed(byte, lost, &mut out, None::<&mut k0_term::NoVault>);
    }
    say("\nK0 operator console closed (no console input)\n");
    exit(0)
}

/// 터미널과 똑같이 배선한 적대적 태스크의 진입점입니다.
///
/// 기대대로면 두 방식 모두 폴트로 끝납니다. 폴트가 나지 않거나 볼트가 허용
/// 밖의 요청에 답하면 `root: FAIL` 줄을 남깁니다.
///
/// # Arguments
/// `mode` - 0이면 볼트와 출력 필터를 두드린 뒤 읽기 전용 페이지에 쓰고,
///          1이면 루트의 데이터를 읽음
extern "C" fn hostile_entry(mode: u64) -> ! {
    if mode != 0 {
        // 루트의 .data는 이 주소 공간에 매핑돼 있지 않음
        // SAFETY: 의도된 위반
        let leaked = unsafe { (&raw const super::ROOT_SECRET).cast::<u64>().read_volatile() };
        say(if leaked == 0 { "root: FAIL terminal wiring (read)\n" } else { "root: FAIL terminal wiring (leak)\n" });
        exit(1)
    }
    #[cfg(feature = "vault")]
    {
        use k0_vault::proto::{alg, request, status};
        let mut wrong = 0u64;
        for op in 0..=8u64 {
            for algo in [alg::NONE, alg::ED25519, 2] {
                for len in [0, 1, 40, u64::MAX] {
                    // 운영자에게 허용된, 상태를 바꾸는 두 요청만 빼고 전부 보냄
                    // (이 볼트는 곧 서비스가 쓸 것이므로 상태를 건드리지 않음)
                    let changes = len == 0
                        && (op == request::ZEROIZE || (op == request::KEYGEN && algo == alg::ED25519));
                    if changes {
                        continue;
                    }
                    let r = super::call(port::VAULT, super::ipc::timeout(5), [request::word(op, algo), len, 7, 0]);
                    // 운영자의 연산이 아닌 것은 거절이나 잘못된 요청으로만 끝나야 함.
                    // 키가 없는 볼트가 "아직 없음"으로 답하는 것은 운영자에게
                    // 허용된 연산(공개키, 키 생성)에서만 받아들임
                    let own = matches!(op, request::PUBLIC_KEY | request::KEYGEN);
                    let fine = r.status == 0
                        && r.mr[3] == 7
                        && match r.mr[0] {
                            status::OK => op == request::STATUS && len == 0,
                            status::DENIED | status::BAD_REQUEST => true,
                            status::UNSUPPORTED | status::NOT_READY | status::NO_ENTROPY => own,
                            _ => false,
                        };
                    wrong += u64::from(!fine);
                }
            }
        }
        // 예약 비트와 쓰지 않는 레지스터를 채운 요청
        let r = super::call(port::VAULT, super::ipc::timeout(5), [request::word(request::STATUS, 0) | 1 << 16, 0, 7, 0]);
        wrong += u64::from(r.status != 0 || r.mr[0] != status::BAD_REQUEST);
        let r = super::call(port::VAULT, super::ipc::timeout(5), [request::word(request::STATUS, 0), 0, 7, 9]);
        wrong += u64::from(r.status != 0 || r.mr[0] != status::BAD_REQUEST);
        if wrong != 0 {
            say("root: FAIL terminal wiring (vault)\n");
            exit(1)
        }
    }
    // 출력 필터: 입력 권한이 있는 Console이라 백스페이스 하나만 더 통과하고
    // 나머지 제어 바이트는 `?`가 됨
    say("root: terminal console filter [");
    for b in [0x1Bu8, 0x08, 0x0D, 0x00, 0x7F, 0x9B] {
        putc(CON, b);
    }
    say("]\n");
    say("root: terminal wiring holds\n");
    // 응답 프레임은 터미널에 읽기 전용이고, 볼트가 없는 이미지에서는 자기
    // 코드에 씀(실행 전용 페이지). 어느 쪽이든 쓰면 이 태스크가 종료됨
    #[cfg(feature = "vault")]
    let target = port::RESP;
    #[cfg(not(feature = "vault"))]
    let target = hostile_entry as usize as u64;
    // SAFETY: 의도된 위반
    unsafe { (target as *mut u64).write_volatile(0xBAD) };
    say("root: FAIL terminal wiring (write)\n");
    exit(1)
}

/// 주소 공간 하나와 그 안의 스택을 만드는 함수입니다. 주소 공간 슬롯을 줍니다.
fn make_space(ut: u64, b: &Boot) -> i64 {
    let (aspace, _) = make_aspace(ut, b);
    let mut va = STACK_BASE;
    while va < STACK_BASE + STACK_SIZE {
        let f = retype(ut, obj::FRAME);
        check("service stack", f > 0 && map_into(f, va, perm::RW, aspace) == 0);
        va += b.frame_size;
    }
    aspace
}

/// 태스크 하나를 만들어 실행 대기 상태로 만드는 함수입니다. TCB 슬롯을 줍니다.
///
/// # Arguments
/// `caps` - 태스크의 CNode 1번 슬롯부터 차례로 넣을 (원본, 남길 권한, 배지)
fn spawn(ut: u64, aspace: i64, entry: u64, arg: u64, caps: &[(i64, u64, u64)]) -> i64 {
    let cnode = retype(ut, obj::CNODE);
    check("service cnode", cnode > 0);
    for (i, &(src, mask, badge)) in caps.iter().enumerate() {
        check("service grant", cap_copy(src, cnode, 1 + i as u64, mask, badge) == 0);
    }
    let tcb = retype(ut, obj::TCB);
    check(
        "service task",
        tcb > 0
            && tcb_configure(tcb, entry, STACK_BASE + STACK_SIZE, aspace, cnode, arg) == 0
            && tcb_resume(tcb) == 0,
    );
    tcb
}

/// 서비스의 볼트를 띄우고 부트 시드를 넘기는 함수입니다. (엔드포인트, 관리자
/// 케이퍼빌리티, 시드를 넘긴 뒤의 볼트 상태 워드)를 줍니다.
///
/// 진짜 부트 시드가 쓰이는 곳은 여기 한 번뿐입니다. 넘긴 뒤에는 루트 쪽의
/// 사본과 관리자 요청 프레임을 지웁니다.
#[cfg(feature = "vault")]
fn start_vault(ut: u64, b: &Boot, term_space: i64) -> (i64, i64, u64) {
    use k0_vault::proto::{self, alg, request, status};
    use k0_vault::server::wipe;

    use super::{call, copy_here, ipc, oracle, take_boot_seed, TEST_VA};

    /// 루트가 서비스 볼트의 관리자 요청 프레임을 보는 VA. 루트의 시험용 VA
    /// 구역(페이지 테이블이 이미 깔린 2MiB) 안에서 다른 검증이 쓰지 않는 자리
    const ROOT_REQ: u64 = TEST_VA + 0x78 * 0x4000;

    let ep = retype(ut, obj::ENDPOINT);
    check("service endpoint", ep > 0);
    let vault_space = make_space(ut, b);
    oracle::link(ut, (0, ROOT_REQ), (vault_space, oracle::VAULT_REQ));
    oracle::link(
        ut,
        (vault_space, oracle::VAULT_RESP + proto::OPERATOR_FRAME * oracle::FRAME_STEP),
        (term_space, port::RESP),
    );
    spawn(ut, vault_space, oracle::vault_entry as usize as u64, 0, &[(ep, rights::R, 0)]);
    let admin = copy_here(ep, rights::W, proto::ADMIN_BADGE);
    check("service admin cap", admin > 0);

    let (mut seed, info) = take_boot_seed();
    for (i, byte) in seed.iter().chain(&info.to_le_bytes()).enumerate() {
        // SAFETY: 방금 이 VA에 관리자 요청 프레임을 쓰기 가능으로 매핑했음
        unsafe { (ROOT_REQ as *mut u8).add(i).write_volatile(*byte) };
    }
    wipe(&mut seed);
    let r = call(admin, ipc::timeout(5), [request::word(request::INIT, alg::NONE), proto::INIT_LEN as u64, 1, 0]);
    for i in 0..proto::INIT_LEN {
        // SAFETY: 위와 같음
        unsafe { (ROOT_REQ as *mut u8).add(i).write_volatile(0) };
    }
    check("service seed delivered", r.status == 0 && r.mr[3] == 1);
    put_str(match r.mr[0] {
        status::OK => "root: vault seeded\n",
        status::NO_ENTROPY => "root: vault has no seed (boot seed has no entropy)\n",
        status::UNSUPPORTED => "root: vault has no seed (no algorithm in this image)\n",
        _ => super::fail("service seed"),
    });
    let r = call(admin, ipc::timeout(5), [request::word(request::STATUS, alg::NONE), 0, 2, 0]);
    check("service vault status", r.status == 0 && r.mr[0] == status::OK && r.mr[3] == 2);
    (ep, admin, r.mr[1])
}

/// 서비스를 시작하는 함수입니다. 시작했으면 돌아오지 않습니다.
///
/// 콘솔 입력 경로가 없는 기계에서는 터미널을 띄울 까닭이 없으므로 한 줄을
/// 남기고 돌아갑니다(쓰지 않게 된 부트 시드는 지움).
pub fn start(b: &Boot) {
    if console_read(slot::CONSOLE, NONBLOCK).0 == err::NO_DEVICE {
        #[cfg(feature = "vault")]
        {
            let (mut seed, _) = super::take_boot_seed();
            k0_vault::server::wipe(&mut seed);
        }
        put_str("root: no console input, terminal not started\n");
        return;
    }
    let ut = super::late_untyped(b);
    let con = slot::CONSOLE as i64;
    let term_space = make_space(ut, b);

    #[cfg(feature = "vault")]
    let (ep, admin, before) = start_vault(ut, b, term_space);
    #[cfg(feature = "vault")]
    let caps = [(con, rights::R | rights::W, 0), (ep, rights::W, k0_vault::proto::OPERATOR_BADGE)];
    #[cfg(not(feature = "vault"))]
    let caps = [(con, rights::R | rights::W, 0)];

    // 터미널과 같은 주소 공간, 같은 케이퍼빌리티 목록으로 적대적 태스크를 돌림.
    // 둘 다 폴트로 죽어야 하고, 그동안 낸 줄과 폴트 수는 시험 하니스가 확인함
    for mode in [0, 1] {
        let tcb = spawn(ut, term_space, hostile_entry as usize as u64, mode, &caps);
        for _ in 0..6000 {
            yield_now();
        }
        check("hostile task delete", cap_delete(tcb) == 0);
    }
    #[cfg(feature = "vault")]
    {
        use k0_vault::proto::{alg, request, status};
        // 볼트는 살아 있고 상태가 그대로임. 관리자 권한은 여기서 내려놓음
        let r = super::call(admin, super::ipc::timeout(5), [request::word(request::STATUS, alg::NONE), 0, 3, 0]);
        check("vault survived the hostile task", r.status == 0 && r.mr[0] == status::OK && r.mr[1] == before);
        check("drop admin cap", cap_delete(admin) == 0);
    }

    // 터미널의 시작 줄이 이 줄의 한가운데에 끼어들지 않게 먼저 찍음
    put_str("root: service started\n");
    spawn(ut, term_space, term_entry as usize as u64, 0, &caps);
    // 루트의 일은 끝났음. 아무도 쥐지 않은 엔드포인트에서 기다려, 입력이 없는
    // 동안 커널이 다음 인터럽트까지 쉬게 함
    let quiet = retype(ut, obj::ENDPOINT);
    check("service idle endpoint", quiet > 0);
    loop {
        let _ = recv(quiet, 0);
    }
}
