//! K0 운영자 터미널의 줄 편집과 명령 해석을 맡는 크레이트입니다.
//!
//! # Features
//! 직렬 선에서 온 바이트를 하나씩 받아 줄을 만들고, 줄이 끝나면 명령으로 풀어
//! 실행한 뒤 결과를 글자로 내보냅니다. 시스템 콜은 쓰지 않고 바깥과는 두
//! 트레이트([Out], [Vault])로만 이어지므로, 모든 경로를 호스트에서 바이트를
//! 넣고 출력을 비교하는 방식으로 테스트합니다.
//!
//! 터미널의 코드는 루트 태스크 이미지에 들어가고 격리된 태스크에서 돌기
//! 때문에 할당도 가변 static도 쓰지 않습니다. `unsafe`도 없습니다.
//!
//! 화면 규칙은 다음과 같습니다. 프롬프트는 [PROMPT]이고, 값은 `이름=값` 줄로
//! 내며, 모든 명령은 `ok` 또는 `err <이유>` 한 줄로 끝납니다. 터미널이 되돌려
//! 보내는 글자는 언제나 프롬프트 뒤에 놓이므로 입력이 줄의 첫머리가 되는 일은
//! 없습니다. 화면에 보인 줄과 실행되는 줄은 항상 같습니다(버퍼에 넣은 글자만
//! 되돌려 보냄).
//!
//! 운영자가 할 수 있는 일은 상태 조회, 공개키 조회, 알고리즘을 골라 키 생성,
//! 키 소거뿐입니다. 서명을 받는 명령은 없습니다.
//!
//! 커널이 입력 유실을 알리면 그 줄을 버리고 다음 줄 끝까지 아무것도 받지
//! 않습니다. 빠진 줄의 꼬리가 다른 명령으로 읽히지 않게 하려는 것입니다. 한
//! 가지 한계가 있습니다. 운영자가 입력하는 도중에 커널의 줄이 끼어들면 화면의
//! 줄은 둘로 나뉘지만 버퍼는 이어져 있어서, 그때 줄 끝을 누르면 화면에 보이는
//! 조각이 아니라 버퍼의 전체 줄이 실행됩니다.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(feature = "vault")]
pub mod wire;

#[cfg(test)]
mod tests;

/// 한 줄의 최대 길이(줄 끝 문자 제외)
pub const LINE_MAX: usize = 64;
/// 프롬프트
pub const PROMPT: &str = "k0> ";
/// 시작할 때 한 번 내는 줄
pub const BANNER: &str = "K0 operator console, type 'help'\n";
/// 공개키를 받는 버퍼의 길이
pub const KEY_MAX: usize = 64;
/// 공개키 한 줄에 찍는 바이트 수
const KEY_ROW: usize = 16;
/// ESC 열에서 매개변수 바이트를 이만큼 넘게 보면 열을 버림
const ESC_MAX: u8 = 16;

/// 터미널이 바이트를 내보내는 통로입니다.
pub trait Out {
    /// 바이트 하나를 내보내는 함수입니다.
    ///
    /// # Arguments
    /// `b` - 내보낼 바이트(인쇄 가능한 ASCII, 줄바꿈, 백스페이스 중 하나)
    fn put(&mut self, b: u8);
}

/// 서명 알고리즘을 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    Ed25519,
}

impl Alg {
    /// 이 크레이트가 이름을 아는 알고리즘 전부
    pub const ALL: [Alg; 1] = [Alg::Ed25519];

    /// 명령과 출력에 쓰는 이름을 주는 함수입니다.
    pub const fn name(self) -> &'static str {
        match self {
            Alg::Ed25519 => "ed25519",
        }
    }

    /// 낱말을 알고리즘으로 푸는 함수입니다. 모르는 이름이면 `None`입니다.
    ///
    /// # Arguments
    /// `word` - 명령 줄의 낱말
    pub fn parse(word: &[u8]) -> Option<Alg> {
        Self::ALL.into_iter().find(|a| a.name().as_bytes() == word)
    }
}

/// 볼트의 키 상태를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// 시드도 키도 없음
    Fresh,
    /// 시드를 받았고 키는 아직 없음
    Seeded,
    /// 키가 있음
    Ready,
    /// 키와 시드가 지워졌음
    Zeroized,
    /// 자가 시험이 실패해 볼트가 오류 상태임
    Failed,
}

impl KeyState {
    const fn name(self) -> &'static str {
        match self {
            KeyState::Fresh => "fresh",
            KeyState::Seeded => "seeded",
            KeyState::Ready => "ready",
            KeyState::Zeroized => "zeroized",
            KeyState::Failed => "failed",
        }
    }
}

/// 볼트의 상태 조회 결과를 담는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: KeyState,
    /// 지금 있는 키의 알고리즘
    pub alg: Option<Alg>,
    /// 시작 시 자가 시험을 통과했는지
    pub self_test: bool,
    /// 시드에 `rng-seed` 32바이트 이상이 들어갔는지
    pub seed_rng: bool,
    /// 시드에 RNDR 난수 256비트가 들어갔는지
    pub seed_rndr: bool,
    /// 이미지에 들어 있는 알고리즘인지([Alg::ALL]과 같은 순서)
    pub built: [bool; Alg::ALL.len()],
}

/// 볼트 명령이 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fail {
    /// 볼트가 이 역할에 그 연산을 허용하지 않음
    Denied,
    /// 이 이미지에 볼트가 없거나 볼트 태스크가 사라졌음
    NoVault,
    /// 키가 아직 없음
    NoKey,
    /// 키가 이미 있음(부팅당 키 하나)
    KeyExists,
    /// 시드의 재료가 모자라 볼트가 시드를 받지 않았음
    NoEntropy,
    /// 그 알고리즘이 이 이미지에 없음
    Unsupported,
    /// 키가 지워졌음(재부팅해야 새 키를 만듦)
    Zeroized,
    /// 볼트가 오류 상태임
    VaultFailed,
    /// 요청이 기한 안에 볼트에 전달되지 않음(볼트는 요청을 보지 못했음)
    VaultTimeout,
    /// 요청은 전달됐지만 기한 안에 답이 오지 않음. 볼트가 그 요청을 처리했는지
    /// 알 수 없음
    VaultUnanswered,
}

impl Fail {
    /// `err` 뒤에 찍는 이름을 주는 함수입니다.
    pub const fn name(self) -> &'static str {
        match self {
            Fail::Denied => "denied",
            Fail::NoVault => "no-vault",
            Fail::NoKey => "no-key",
            Fail::KeyExists => "key-exists",
            Fail::NoEntropy => "no-entropy",
            Fail::Unsupported => "unsupported-algorithm",
            Fail::Zeroized => "zeroized",
            Fail::VaultFailed => "vault-failed",
            Fail::VaultTimeout | Fail::VaultUnanswered => "vault-timeout",
        }
    }
}

/// 터미널이 볼트에 말을 거는 통로입니다. 운영자 역할의 네 연산만 있습니다.
pub trait Vault {
    /// 볼트의 상태를 묻는 함수입니다.
    fn status(&mut self) -> Result<Status, Fail>;
    /// 공개키를 받아 그 길이를 돌려주는 함수입니다.
    ///
    /// # Arguments
    /// `key` - 공개키를 받을 버퍼
    fn pubkey(&mut self, key: &mut [u8; KEY_MAX]) -> Result<usize, Fail>;
    /// 고른 알고리즘으로 키를 만들게 하는 함수입니다.
    ///
    /// # Arguments
    /// `alg` - 키의 알고리즘
    fn keygen(&mut self, alg: Alg) -> Result<(), Fail>;
    /// 키를 지우게 하는 함수입니다.
    fn zeroize(&mut self) -> Result<(), Fail>;
}

/// 볼트가 없는 이미지에서 [Vault] 자리를 채우는 타입입니다. 값이 존재하지
/// 않으므로 그 이미지에는 볼트를 부르는 경로가 없습니다.
pub enum NoVault {}

impl Vault for NoVault {
    fn status(&mut self) -> Result<Status, Fail> {
        match *self {}
    }

    fn pubkey(&mut self, _key: &mut [u8; KEY_MAX]) -> Result<usize, Fail> {
        match *self {}
    }

    fn keygen(&mut self, _alg: Alg) -> Result<(), Fail> {
        match *self {}
    }

    fn zeroize(&mut self) -> Result<(), Fail> {
        match *self {}
    }
}

/// ESC로 시작하는 열(화살표 키 등)을 건너뛰는 상태입니다.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Esc {
    Idle,
    /// ESC를 방금 봄
    Start,
    /// `ESC [` 뒤, 지금까지 본 매개변수 바이트 수
    Csi(u8),
    /// `ESC O` 뒤, 한 바이트를 더 버림
    One,
}

/// 줄 끝까지 입력을 버리는 이유입니다.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Skip {
    No,
    /// 줄이 [LINE_MAX]를 넘음
    TooLong,
    /// 커널이 입력을 잃었다고 알림
    Lost,
}

/// 터미널의 상태를 담는 구조체입니다.
///
/// 줄 버퍼와 그 길이, 건너뛰기 상태뿐이라 태스크의 스택에 둡니다.
pub struct Term {
    line: [u8; LINE_MAX],
    len: usize,
    skip: Skip,
    esc: Esc,
    /// 바로 앞 바이트가 CR이었는지(CRLF의 LF를 삼키는 데 씀)
    cr: bool,
}

impl Default for Term {
    fn default() -> Self {
        Self::new()
    }
}

fn text(out: &mut impl Out, s: &str) {
    for b in s.bytes() {
        out.put(b);
    }
}

fn hex(out: &mut impl Out, bytes: &[u8]) {
    for b in bytes {
        for nib in [b >> 4, b & 0xF] {
            out.put(if nib < 10 { b'0' + nib } else { b'a' + nib - 10 });
        }
    }
}

fn ok(out: &mut impl Out) {
    text(out, "ok\n");
}

fn err(out: &mut impl Out, reason: &str) {
    text(out, "err ");
    text(out, reason);
    out.put(b'\n');
}

impl Term {
    /// 빈 터미널 상태를 만드는 함수입니다.
    pub const fn new() -> Self {
        Self {
            line: [0; LINE_MAX],
            len: 0,
            skip: Skip::No,
            esc: Esc::Idle,
            cr: false,
        }
    }

    /// 시작 줄과 첫 프롬프트를 내는 함수입니다.
    ///
    /// # Arguments
    /// `out` - 출력 통로
    pub fn start(&mut self, out: &mut impl Out) {
        text(out, BANNER);
        text(out, PROMPT);
    }

    /// 입력 바이트 하나를 처리하는 함수입니다.
    ///
    /// # Arguments
    /// `b` - 받은 바이트
    /// `lost` - 이 바이트 앞에서 입력이 유실됐다고 커널이 알렸는지
    /// `out` - 출력 통로
    /// `vault` - 볼트로 가는 통로(볼트가 없는 이미지에서는 `None`)
    pub fn feed<O: Out, V: Vault>(&mut self, b: u8, lost: bool, out: &mut O, mut vault: Option<&mut V>) {
        if lost && self.skip != Skip::Lost {
            // 어디가 빠졌는지 알 수 없으므로 지금 줄을 버리고 다음 줄 끝까지
            // 아무것도 받지 않음. 빠진 줄의 꼬리가 다른 명령으로 읽히지 않게 함
            out.put(b'\n');
            err(out, "input-lost");
            self.len = 0;
            self.skip = Skip::Lost;
            self.esc = Esc::Idle;
        }
        let after_cr = self.cr;
        self.cr = b == b'\r';
        if b == b'\n' && after_cr {
            return;
        }
        if self.skip != Skip::No {
            self.skipping(b, out);
            return;
        }
        if !self.escape(b) {
            return;
        }
        match b {
            b'\r' | b'\n' => {
                out.put(b'\n');
                let len = core::mem::take(&mut self.len);
                run(&self.line[..len], out, vault.as_deref_mut());
                text(out, PROMPT);
            }
            0x08 | 0x7F => {
                if self.len > 0 {
                    self.len -= 1;
                    for e in [0x08, b' ', 0x08] {
                        out.put(e);
                    }
                }
            }
            0x03 | 0x15 => self.cancel(b, out),
            0x1B => self.esc = Esc::Start,
            0x20..=0x7E => {
                if self.len < LINE_MAX {
                    self.line[self.len] = b;
                    self.len += 1;
                    out.put(b);
                } else {
                    self.skip = Skip::TooLong;
                }
            }
            // 그 밖의 제어 바이트와 0x80 이상은 버림
            _ => {}
        }
    }

    /// 줄을 버리고 새 프롬프트를 내는 함수입니다. 누른 키를 `^C`나 `^U`로 보여
    /// 줍니다.
    fn cancel(&mut self, key: u8, out: &mut impl Out) {
        self.len = 0;
        self.skip = Skip::No;
        text(out, if key == 0x03 { "^C\n" } else { "^U\n" });
        text(out, PROMPT);
    }

    /// 줄 끝까지 버리는 동안의 바이트 하나를 처리하는 함수입니다.
    fn skipping(&mut self, b: u8, out: &mut impl Out) {
        match b {
            b'\r' | b'\n' => {
                if self.skip == Skip::TooLong {
                    out.put(b'\n');
                    err(out, "line-too-long");
                }
                self.len = 0;
                self.skip = Skip::No;
                text(out, PROMPT);
            }
            0x03 | 0x15 => self.cancel(b, out),
            _ => {}
        }
    }

    /// ESC 열을 건너뛰는 함수입니다. 이 바이트를 평소대로 처리해야 하면
    /// true를 돌려줍니다.
    fn escape(&mut self, b: u8) -> bool {
        match self.esc {
            Esc::Idle => true,
            // 제어 바이트(줄 끝, 취소 등)는 열을 끊고 평소대로 처리함
            _ if b < 0x20 || b == 0x7F => {
                self.esc = Esc::Idle;
                true
            }
            Esc::Start => {
                self.esc = match b {
                    b'[' => Esc::Csi(0),
                    b'O' => Esc::One,
                    _ => Esc::Idle,
                };
                false
            }
            Esc::One => {
                self.esc = Esc::Idle;
                false
            }
            // 매개변수 바이트가 이어지는 동안은 열 안이고, 그 밖의 바이트(끝
            // 바이트나 0x80 이상)에서 열이 끝남. 어느 쪽이든 이 바이트는 버림
            Esc::Csi(n) => {
                self.esc = match b {
                    0x20..=0x3F if n < ESC_MAX => Esc::Csi(n + 1),
                    _ => Esc::Idle,
                };
                false
            }
        }
    }
}

/// 줄을 낱말로 나눠 `words`에 담고 낱말 수를 돌려주는 함수입니다. `words`보다
/// 낱말이 많으면 `words.len() + 1`을 돌려줍니다.
fn split<'a>(line: &'a [u8], words: &mut [&'a [u8]]) -> usize {
    let mut n = 0;
    for w in line.split(|b| *b == b' ').filter(|w| !w.is_empty()) {
        if n == words.len() {
            return n + 1;
        }
        words[n] = w;
        n += 1;
    }
    n
}

/// 완성된 줄 하나를 실행하는 함수입니다.
fn run<O: Out, V: Vault>(line: &[u8], out: &mut O, vault: Option<&mut V>) {
    let mut words: [&[u8]; 3] = [&[]; 3];
    let n = split(line, &mut words);
    if n == 0 {
        return;
    }
    let args = &words[1..n.min(3)];
    match words[0] {
        b"help" if n == 1 => help(out, vault),
        b"help" => err(out, "bad-argument"),
        b"vault" => match vault {
            None => err(out, "no-vault"),
            // 낱말이 셋을 넘는 줄은 어느 명령에도 맞지 않음
            Some(_) if n > 3 => err(out, "bad-argument"),
            Some(v) => vault_command(args, out, v),
        },
        _ => err(out, "unknown-command"),
    }
}

fn help<O: Out, V: Vault>(out: &mut O, vault: Option<&mut V>) {
    text(out, "help\n");
    if let Some(v) = vault {
        text(out, "vault status\nvault pubkey\nvault keygen <alg>     alg:");
        match v.status() {
            Ok(st) if st.built.iter().any(|b| *b) => {
                for (a, _) in Alg::ALL.iter().zip(st.built).filter(|(_, built)| *built) {
                    out.put(b' ');
                    text(out, a.name());
                }
            }
            Ok(_) => text(out, " none"),
            Err(_) => text(out, " unknown"),
        }
        text(out, "\nvault zeroize confirm\n");
    }
    ok(out);
}

fn vault_command<O: Out, V: Vault>(args: &[&[u8]], out: &mut O, v: &mut V) {
    match args {
        [b"status"] => match v.status() {
            Ok(st) => {
                status(out, &st);
                ok(out);
            }
            Err(f) => err(out, f.name()),
        },
        [b"pubkey"] => {
            let mut key = [0u8; KEY_MAX];
            match v.pubkey(&mut key) {
                Ok(len) => {
                    for (i, row) in key[..len.min(KEY_MAX)].chunks(KEY_ROW).enumerate() {
                        text(out, if i == 0 { "pubkey=" } else { "       " });
                        hex(out, row);
                        out.put(b'\n');
                    }
                    ok(out);
                }
                Err(f) => err(out, f.name()),
            }
        }
        [b"keygen", name] => match Alg::parse(name) {
            None => err(out, Fail::Unsupported.name()),
            Some(alg) => match v.keygen(alg) {
                Ok(()) => {
                    text(out, "state=ready\nalg=");
                    text(out, alg.name());
                    out.put(b'\n');
                    ok(out);
                }
                Err(f) => changed(out, f),
            },
        },
        [b"zeroize", b"confirm"] => match v.zeroize() {
            Ok(()) => {
                text(out, "state=zeroized\n");
                ok(out);
            }
            Err(f) => changed(out, f),
        },
        [b"zeroize"] => {
            text(out, "hint=type 'vault zeroize confirm' to erase the key\n");
            err(out, "confirm-required");
        }
        [] | [b"status" | b"pubkey" | b"keygen" | b"zeroize", ..] => err(out, "bad-argument"),
        _ => err(out, "unknown-command"),
    }
}

/// 상태를 바꾸는 명령이 실패했을 때의 끝줄을 내는 함수입니다. 요청이 전달된 뒤
/// 답이 오지 않았으면 볼트가 그 일을 했는지 알 수 없으므로, 확인하는 방법을 한
/// 줄 덧붙입니다.
fn changed(out: &mut impl Out, f: Fail) {
    if f == Fail::VaultUnanswered {
        text(out, "hint=run 'vault status' to see whether it took effect\n");
    }
    err(out, f.name());
}

fn status(out: &mut impl Out, st: &Status) {
    text(out, "state=");
    text(out, st.state.name());
    text(out, "\nalg=");
    text(out, st.alg.map_or("none", Alg::name));
    text(out, "\nselftest=");
    text(out, if st.self_test { "passed" } else { "failed" });
    text(out, "\nseed=");
    text(
        out,
        match (st.seed_rng, st.seed_rndr) {
            (false, false) => "none",
            (true, false) => "rng-seed",
            (false, true) => "rndr",
            (true, true) => "rng-seed+rndr",
        },
    );
    // 구현이 어떤 인증도 받지 않았음을 운영자에게 항상 보여 줌
    text(out, "\nvalidated=no\n");
}
