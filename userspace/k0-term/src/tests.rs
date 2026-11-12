//! 터미널의 줄 편집과 명령 해석을 검증하는 테스트 모듈입니다.

use super::*;

impl Out for Vec<u8> {
    fn put(&mut self, b: u8) {
        self.push(b);
    }
}

/// 부른 연산을 적어 두는 가짜 볼트입니다.
struct Fake {
    state: KeyState,
    alg: Option<Alg>,
    built: bool,
    fail: Option<Fail>,
    calls: Vec<&'static str>,
}

impl Fake {
    fn new() -> Self {
        Self {
            state: KeyState::Seeded,
            alg: None,
            built: true,
            fail: None,
            calls: Vec::new(),
        }
    }

    fn gate(&mut self, name: &'static str) -> Result<(), Fail> {
        self.calls.push(name);
        self.fail.map_or(Ok(()), Err)
    }
}

impl Vault for Fake {
    fn status(&mut self) -> Result<Status, Fail> {
        self.gate("status")?;
        Ok(Status {
            state: self.state,
            alg: self.alg,
            self_test: true,
            seed_rng: true,
            seed_rndr: false,
            built: [self.built],
        })
    }

    fn pubkey(&mut self, key: &mut [u8; KEY_MAX]) -> Result<usize, Fail> {
        self.gate("pubkey")?;
        if self.state != KeyState::Ready {
            return Err(Fail::NoKey);
        }
        for (i, b) in key.iter_mut().enumerate().take(32) {
            *b = i as u8 * 8 + 1;
        }
        Ok(32)
    }

    fn keygen(&mut self, alg: Alg) -> Result<(), Fail> {
        self.gate("keygen")?;
        if self.state == KeyState::Ready {
            return Err(Fail::KeyExists);
        }
        self.state = KeyState::Ready;
        self.alg = Some(alg);
        Ok(())
    }

    fn zeroize(&mut self) -> Result<(), Fail> {
        self.gate("zeroize")?;
        self.state = KeyState::Zeroized;
        self.alg = None;
        Ok(())
    }
}

/// 바이트 열을 넣고 출력을 글자열로 돌려주는 도우미입니다.
fn type_in(term: &mut Term, vault: &mut Fake, input: &[u8]) -> String {
    let mut out = Vec::new();
    for b in input {
        term.feed(*b, false, &mut out, Some(&mut *vault));
    }
    String::from_utf8(out).unwrap()
}

fn session(input: &[u8]) -> (String, Fake) {
    let mut vault = Fake::new();
    let out = type_in(&mut Term::new(), &mut vault, input);
    (out, vault)
}

fn bare(input: &[u8]) -> String {
    let mut term = Term::new();
    let mut out = Vec::new();
    for b in input {
        term.feed(*b, false, &mut out, None::<&mut NoVault>);
    }
    String::from_utf8(out).unwrap()
}

#[test]
fn the_banner_comes_once_and_ends_with_the_prompt() {
    let mut out = Vec::new();
    Term::new().start(&mut out);
    assert_eq!(out, b"K0 operator console, type 'help'\nk0> ");
}

#[test]
fn help_lists_the_commands_and_the_algorithms_in_the_image() {
    let (out, vault) = session(b"help\r");
    assert_eq!(
        out,
        "help\nhelp\nvault status\nvault pubkey\nvault keygen <alg>     alg: ed25519\nvault zeroize confirm\nok\nk0> "
    );
    assert_eq!(vault.calls, ["status"]);

    let mut none = Fake::new();
    none.built = false;
    assert!(type_in(&mut Term::new(), &mut none, b"help\r").contains("alg: none\n"));
    let mut dead = Fake::new();
    dead.fail = Some(Fail::VaultTimeout);
    let out = type_in(&mut Term::new(), &mut dead, b"help\r");
    assert!(out.contains("alg: unknown\n") && out.ends_with("ok\nk0> "));
}

#[test]
fn without_a_vault_help_is_the_only_command() {
    assert_eq!(bare(b"help\r"), "help\nhelp\nok\nk0> ");
    assert_eq!(bare(b"vault status\r"), "vault status\nerr no-vault\nk0> ");
    assert_eq!(bare(b"vault zeroize confirm\r"), "vault zeroize confirm\nerr no-vault\nk0> ");
    assert_eq!(bare(b"status\r"), "status\nerr unknown-command\nk0> ");
}

#[test]
fn every_command_ends_with_exactly_one_ok_or_err_line() {
    let lines: [&[u8]; 16] = [
        b"help",
        b"help me",
        b"vault",
        b"vault status",
        b"vault status now",
        b"vault pubkey",
        b"vault keygen",
        b"vault keygen ed25519",
        b"vault keygen rsa",
        b"vault keygen ed25519 again",
        b"vault zeroize",
        b"vault zeroize confirm",
        b"vault zeroize confirm now",
        b"vault sign",
        b"sign",
        b"a b c d e",
    ];
    for line in lines {
        let mut input = line.to_vec();
        input.push(b'\r');
        let (out, _) = session(&input);
        let body = out.strip_prefix(core::str::from_utf8(line).unwrap()).unwrap();
        let body = body.strip_suffix("k0> ").unwrap();
        let ends = body.lines().filter(|l| *l == "ok" || l.starts_with("err ")).count();
        assert_eq!(ends, 1, "{out:?}");
        let last = body.lines().last().unwrap();
        assert!(last == "ok" || last.starts_with("err "), "{out:?}");
    }
}

#[test]
fn wrong_words_get_the_right_error() {
    let cases: [(&[u8], &str); 12] = [
        (b"sign", "unknown-command"),
        (b"HELP", "unknown-command"),
        (b"help me", "bad-argument"),
        (b"vault", "bad-argument"),
        (b"vault sign", "unknown-command"),
        (b"vault status now", "bad-argument"),
        (b"vault pubkey x", "bad-argument"),
        (b"vault keygen", "bad-argument"),
        (b"vault keygen rsa", "unsupported-algorithm"),
        (b"vault keygen ED25519", "unsupported-algorithm"),
        (b"vault zeroize CONFIRM", "bad-argument"),
        (b"vault zeroize confirm now", "bad-argument"),
    ];
    for (line, want) in cases {
        let mut input = line.to_vec();
        input.push(b'\n');
        let (out, vault) = session(&input);
        assert!(out.ends_with(&format!("\nerr {want}\nk0> ")), "{out:?}");
        assert!(!vault.calls.contains(&"zeroize") && !vault.calls.contains(&"keygen"), "{out:?}");
    }
}

#[test]
fn status_prints_every_field() {
    let (out, _) = session(b"vault status\r");
    assert_eq!(
        out,
        "vault status\nstate=seeded\nalg=none\nselftest=passed\nseed=rng-seed\nvalidated=no\nok\nk0> "
    );
    let mut out = Vec::new();
    for (state, name) in [
        (KeyState::Fresh, "fresh"),
        (KeyState::Seeded, "seeded"),
        (KeyState::Ready, "ready"),
        (KeyState::Zeroized, "zeroized"),
        (KeyState::Failed, "failed"),
    ] {
        for (rng, rndr, seed) in [
            (false, false, "none"),
            (true, false, "rng-seed"),
            (false, true, "rndr"),
            (true, true, "rng-seed+rndr"),
        ] {
            out.clear();
            let st = Status {
                state,
                alg: (state == KeyState::Ready).then_some(Alg::Ed25519),
                self_test: state != KeyState::Failed,
                seed_rng: rng,
                seed_rndr: rndr,
                built: [true],
            };
            status(&mut out, &st);
            let alg = if state == KeyState::Ready { "ed25519" } else { "none" };
            let test = if state == KeyState::Failed { "failed" } else { "passed" };
            assert_eq!(
                String::from_utf8(out.clone()).unwrap(),
                format!("state={name}\nalg={alg}\nselftest={test}\nseed={seed}\nvalidated=no\n")
            );
        }
    }
}

#[test]
fn key_generation_then_public_key_then_zeroize() {
    let (out, vault) = session(b"vault pubkey\rvault keygen ed25519\rvault keygen ed25519\rvault pubkey\rvault zeroize\rvault zeroize confirm\r");
    let want = "vault pubkey\nerr no-key\nk0> \
vault keygen ed25519\nstate=ready\nalg=ed25519\nok\nk0> \
vault keygen ed25519\nerr key-exists\nk0> \
vault pubkey\npubkey=0109111921293139414951596169717981899199a1a9b1b9c1c9d1d9e1e9f1f9\nok\nk0> ";
    // 공개키는 16바이트씩 두 줄이고 둘째 줄은 7칸 들여 씀
    let want = want.replace(
        "pubkey=0109111921293139414951596169717981899199a1a9b1b9c1c9d1d9e1e9f1f9",
        "pubkey=01091119212931394149515961697179\n       81899199a1a9b1b9c1c9d1d9e1e9f1f9",
    );
    let rest = "vault zeroize\nhint=type 'vault zeroize confirm' to erase the key\nerr confirm-required\nk0> \
vault zeroize confirm\nstate=zeroized\nok\nk0> ";
    assert_eq!(out, format!("{want}{rest}"));
    assert_eq!(vault.calls, ["pubkey", "keygen", "keygen", "pubkey", "zeroize"]);
}

#[test]
fn every_failure_has_its_own_name_from_the_agreed_list() {
    let all = [
        (Fail::Denied, "denied"),
        (Fail::NoVault, "no-vault"),
        (Fail::NoKey, "no-key"),
        (Fail::KeyExists, "key-exists"),
        (Fail::NoEntropy, "no-entropy"),
        (Fail::Unsupported, "unsupported-algorithm"),
        (Fail::Zeroized, "zeroized"),
        (Fail::VaultFailed, "vault-failed"),
        (Fail::VaultTimeout, "vault-timeout"),
        (Fail::VaultUnanswered, "vault-timeout"),
    ];
    for (fail, name) in all {
        assert_eq!(fail.name(), name);
        let mut vault = Fake::new();
        vault.fail = Some(fail);
        for line in [&b"vault status\r"[..], b"vault pubkey\r", b"vault keygen ed25519\r", b"vault zeroize confirm\r"] {
            let out = type_in(&mut Term::new(), &mut vault, line);
            assert!(out.ends_with(&format!("\nerr {name}\nk0> ")), "{out:?}");
        }
    }
}

#[test]
fn a_change_whose_outcome_is_unknown_says_how_to_find_out() {
    let hint = "hint=run 'vault status' to see whether it took effect\nerr vault-timeout\nk0> ";
    for fail in [Fail::VaultUnanswered, Fail::VaultTimeout, Fail::NoVault, Fail::Denied] {
        let mut vault = Fake::new();
        vault.fail = Some(fail);
        for (line, changes) in [
            (&b"vault keygen ed25519\r"[..], true),
            (b"vault zeroize confirm\r", true),
            (b"vault status\r", false),
            (b"vault pubkey\r", false),
        ] {
            let out = type_in(&mut Term::new(), &mut vault, line);
            // 전달됐지만 답이 없었던, 상태를 바꾸는 명령에만 붙음
            assert_eq!(out.ends_with(hint), changes && fail == Fail::VaultUnanswered, "{fail:?} {out:?}");
        }
    }
}

#[test]
fn the_terminal_never_claims_validation() {
    // 터미널이 낼 수 있는 글자열을 전부 모아, 인증이나 승인을 뜻하는 말이
    // 없는지 확인함. 구현은 어떤 인증도 받지 않았고 화면은 그것을 그대로 말함
    let mut all = String::new();
    for state in [KeyState::Fresh, KeyState::Seeded, KeyState::Ready, KeyState::Zeroized, KeyState::Failed] {
        for fail in [None, Some(Fail::VaultUnanswered), Some(Fail::VaultFailed)] {
            let mut vault = Fake::new();
            vault.state = state;
            vault.alg = (state == KeyState::Ready).then_some(Alg::Ed25519);
            vault.fail = fail;
            let lines = b"help\rvault status\rvault pubkey\rvault keygen ed25519\rvault zeroize\rvault zeroize confirm\rx\r";
            all += &type_in(&mut Term::new(), &mut vault, lines);
        }
    }
    let mut out = Vec::new();
    Term::new().start(&mut out);
    all += core::str::from_utf8(&out).unwrap();
    let lower = all.to_lowercase();
    for word in ["fips", "approved", "compliant", "certified", "validated=yes"] {
        assert!(!lower.contains(word), "the terminal must not say {word:?}");
    }
    assert!(all.contains("validated=no\n"));
}

#[test]
fn backspace_and_delete_erase_what_was_echoed() {
    let (out, vault) = session(b"vault stauts\x08\x08\x7ftus\r");
    assert!(out.starts_with("vault stauts\x08 \x08\x08 \x08\x08 \x08tus\nstate=seeded\n"), "{out:?}");
    assert_eq!(vault.calls, ["status"]);
    // 빈 줄에서는 아무것도 내보내지 않아 프롬프트를 지우지 못함
    assert_eq!(bare(b"\x08\x7f\x08"), "");
    assert_eq!(bare(b"a\x08\x08\x08"), "a\x08 \x08");
}

#[test]
fn cancel_keys_drop_the_line() {
    for (key, shown) in [(0x03u8, "^C"), (0x15, "^U")] {
        let mut input = b"vault zeroize confirm".to_vec();
        input.push(key);
        input.extend_from_slice(b"\rhelp\r");
        let (out, vault) = session(&input);
        assert!(out.starts_with(&format!("vault zeroize confirm{shown}\nk0> \nk0> help\n")), "{out:?}");
        assert_eq!(vault.calls, ["status"]);
    }
}

#[test]
fn line_ends_are_cr_lf_or_both() {
    assert_eq!(bare(b"help\r\n"), "help\nhelp\nok\nk0> ");
    assert_eq!(bare(b"help\n"), "help\nhelp\nok\nk0> ");
    assert_eq!(bare(b"help\r\rhelp\n\n"), "help\nhelp\nok\nk0> \nk0> help\nhelp\nok\nk0> \nk0> ");
    // CR 다음이 아닌 LF는 그 자체로 줄 끝임
    assert_eq!(bare(b"\rx\n"), "\nk0> x\nerr unknown-command\nk0> ");
    assert_eq!(bare(b"\r\n\r\n"), "\nk0> \nk0> ");
}

#[test]
fn an_empty_line_gives_only_a_prompt() {
    assert_eq!(bare(b"\r"), "\nk0> ");
    assert_eq!(bare(b"   \r"), "   \nk0> ");
}

#[test]
fn spaces_around_words_do_not_matter() {
    let (out, vault) = session(b"  vault   zeroize  confirm  \r");
    assert!(out.ends_with("\nstate=zeroized\nok\nk0> "), "{out:?}");
    assert_eq!(vault.calls, ["zeroize"]);
}

#[test]
fn escape_sequences_are_swallowed() {
    // 화살표, 수정 키가 붙은 화살표, 기능 키, Alt 글자
    for seq in [&b"\x1b[A"[..], b"\x1b[1;5C", b"\x1bOP", b"\x1bx", b"\x1b[200~", b"\x1b\x1b[B"] {
        let mut input = b"he".to_vec();
        input.extend_from_slice(seq);
        input.extend_from_slice(b"lp\r");
        assert_eq!(bare(&input), "help\nhelp\nok\nk0> ", "{seq:?}");
    }
    // 끝나지 않은 열은 줄 끝에서 끊기고, 줄 끝은 평소대로 처리됨
    assert_eq!(bare(b"help\x1b[12\r"), "help\nhelp\nok\nk0> ");
    assert_eq!(bare(b"help\x1b\r"), "help\nhelp\nok\nk0> ");
    assert_eq!(bare(b"xy\x1b[\x03help\r"), "xy^C\nk0> help\nhelp\nok\nk0> ");
    // 열 안의 글자는 화면에도 버퍼에도 들어가지 않음
    assert_eq!(bare(b"\x1b[vault\r"), "ault\nerr unknown-command\nk0> ");
}

#[test]
fn an_escape_sequence_that_never_ends_stops_swallowing() {
    let mut input = b"\x1b[".to_vec();
    input.extend_from_slice(&[b'1'; 16]);
    // 열일곱 번째 매개변수 바이트에서 열을 버리고, 그 뒤는 평소 글자임
    input.extend_from_slice(b"1help\r");
    assert_eq!(bare(&input), "help\nhelp\nok\nk0> ");
}

#[test]
fn other_control_bytes_and_high_bytes_are_ignored() {
    let mut input = Vec::new();
    for b in (0u8..=0xFF).filter(|b| !matches!(b, 0x03 | 0x08 | 0x0A | 0x0D | 0x15 | 0x1B | 0x20..=0x7F)) {
        input.push(b);
    }
    input.extend_from_slice(b"h\x00e\x80l\xffp\x07\r");
    assert_eq!(bare(&input), "help\nhelp\nok\nk0> ");
}

#[test]
fn a_line_that_is_too_long_is_refused_whole() {
    let mut input = vec![b'a'; LINE_MAX];
    input.push(b'\r');
    let out = bare(&input);
    assert_eq!(out, format!("{}\nerr unknown-command\nk0> ", "a".repeat(LINE_MAX)));

    // 한 글자가 넘치면 그 뒤로는 되돌려 보내지 않고, 지우기도 듣지 않으며,
    // 줄 끝에서 줄 전체를 거부함
    let mut vault = Fake::new();
    let mut input = b"vault zeroize confirm".to_vec();
    input.resize(LINE_MAX, b' ');
    input.extend_from_slice(b"x\x08\x08\r");
    let out = type_in(&mut Term::new(), &mut vault, &input);
    assert_eq!(out, format!("{:<64}\nerr line-too-long\nk0> ", "vault zeroize confirm"));
    assert!(vault.calls.is_empty());

    // 넘친 줄도 취소 키로 버릴 수 있음
    let mut input = vec![b'a'; LINE_MAX + 5];
    input.extend_from_slice(b"\x03help\r");
    assert!(bare(&input).ends_with("^C\nk0> help\nhelp\nok\nk0> "));
}

#[test]
fn lost_input_drops_everything_up_to_the_next_line_end() {
    let mut term = Term::new();
    let mut vault = Fake::new();
    let mut out = Vec::new();
    for b in b"vault zero" {
        term.feed(*b, false, &mut out, Some(&mut vault));
    }
    // 유실 알림과 함께 온 바이트부터 줄 끝까지는 완전한 명령이라도 실행하지 않음
    let mut lost = true;
    for b in b"vault zeroize confirm\r" {
        term.feed(*b, lost, &mut out, Some(&mut vault));
        lost = false;
    }
    assert_eq!(String::from_utf8(out.clone()).unwrap(), "vault zero\nerr input-lost\nk0> ");
    assert!(vault.calls.is_empty());

    // 버리는 동안 유실이 또 알려져도 오류 줄은 한 번만 나감
    out.clear();
    term.feed(b'x', true, &mut out, Some(&mut vault));
    term.feed(b'y', true, &mut out, Some(&mut vault));
    term.feed(b'\n', true, &mut out, Some(&mut vault));
    assert_eq!(String::from_utf8(out.clone()).unwrap(), "\nerr input-lost\nk0> ");

    // 그 뒤의 줄은 평소대로 실행됨
    out.clear();
    for b in b"vault status\r" {
        term.feed(*b, false, &mut out, Some(&mut vault));
    }
    assert!(String::from_utf8(out).unwrap().ends_with("validated=no\nok\nk0> "));
    assert_eq!(vault.calls, ["status"]);
}

/// 터미널이 낸 바이트를 화면처럼 해석해 줄들을 돌려주는 도우미입니다. 커서가
/// 줄의 첫머리보다 왼쪽으로 가려 하면 패닉합니다.
fn screen(out: &[u8]) -> Vec<String> {
    let mut rows = vec![Vec::new()];
    let mut col = 0usize;
    for b in out {
        let row = rows.last_mut().unwrap();
        match b {
            b'\n' => {
                rows.push(Vec::new());
                col = 0;
            }
            0x08 => {
                assert!(col > 0, "backspace at the start of a line");
                col -= 1;
            }
            0x20..=0x7E => {
                if col == row.len() {
                    row.push(*b);
                } else {
                    row[col] = *b;
                }
                col += 1;
            }
            other => panic!("byte {other:#x} must never be sent"),
        }
    }
    rows.into_iter().map(|r| String::from_utf8(r).unwrap().trim_end().to_string()).collect()
}

/// 고정된 씨앗의 의사 난수입니다(xorshift64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_input_never_forges_a_line_and_runs_only_what_the_screen_shows() {
    let pool: &[&[u8]] = &[
        b"vault zeroize confirm",
        b"vault zeroize",
        b"vault keygen ed25519",
        b"vault status",
        b"vault pubkey",
        b"help",
        b"k0: FATAL",
        b"root: FAIL",
        b"\r",
        b"\n",
        b"\r\n",
        b"\x08",
        b"\x7f",
        b"\x03",
        b"\x15",
        b"\x1b",
        b"\x1b[",
        b"\x1bO",
        b" ",
        b"confirm",
        b"zeroize",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for round in 0..2000 {
        let mut term = Term::new();
        let mut vault = Fake::new();
        let mut out = Vec::new();
        term.start(&mut out);
        let mut zeroized_at = None;
        for _ in 0..(rng.next() % 40) {
            let lost = rng.next() % 50 == 0;
            let r = rng.next();
            let piece: Vec<u8> = if r % 4 == 0 {
                vec![(r >> 8) as u8]
            } else {
                pool[(r >> 8) as usize % pool.len()].to_vec()
            };
            for (i, b) in piece.iter().enumerate() {
                term.feed(*b, lost && i == 0, &mut out, Some(&mut vault));
                if zeroized_at.is_none() && vault.calls.contains(&"zeroize") {
                    zeroized_at = Some(out.len());
                }
            }
        }
        let rows = screen(&out);
        for row in &rows[1..] {
            // 입력이 닿는 줄은 언제나 프롬프트로 시작하므로 커널이나 루트
            // 태스크의 줄처럼 보이는 줄은 나올 수 없음
            assert!(!row.starts_with("k0:") && !row.starts_with("root:"), "round {round}: {row:?}");
        }
        if let Some(at) = zeroized_at {
            // 소거가 실행됐다면 화면에서 그 직전의 프롬프트 줄이 정확히 그
            // 명령이어야 함
            let before = screen(&out[..at]);
            let shown = before.iter().rev().find(|r| r.starts_with("k0> ")).unwrap();
            let words: Vec<&str> = shown["k0> ".len()..].split(' ').filter(|w| !w.is_empty()).collect();
            assert_eq!(words, ["vault", "zeroize", "confirm"], "round {round}: {shown:?}");
        }
    }
}
