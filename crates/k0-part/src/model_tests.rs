//! Lean 모형이 낸 결과와 진짜 Core를 맞대는 테스트 모듈입니다.
//!
//! `model_vectors.txt`의 한 줄은 사건 대본 하나와, Lean 모형 `K0Part.step`이 그
//! 대본에 낸 결과의 해시입니다(`proofs/lean/ModelVectors.lean`이 계산함). 이
//! 모듈은 같은 대본을 고치지 않은 `Core<u64>`에 넣고, 부팅 직후와 사건마다 상태를
//! 한 줄로 적어 해시를 견줍니다. CI에는 Lean이 없으므로 Rust 코드와 기록된 모형의
//! 결과가 갈라지면 이 테스트가 알립니다.
//!
//! 대본은 `;`로 나눈 칸 여덟 개입니다. 이름, 부팅 틱, 태스크 0번부터의 도메인,
//! 첫 태스크, 준비된 태스크, 대기자(`번호@기한` 또는 `번호@-`), 사건, 해시입니다.
//! 사건은 `tN` 틱, `pN` 대기 루프의 poll, `y` 양보, `b-`와 `bN` 블록(기한 없음과
//! 기한 N), `wT` 같은 도메인 태스크를 깨움, `xT` 다른 도메인 태스크를 깨움,
//! `sD:W,...` 표 바꾸기입니다. 블록과 깨움은 실행 중인 태스크의 행동입니다. 실행
//! 중인 태스크가 없을 때 `w`와 `x`는 아무 일도 하지 않고, `y`와 `b`는 Core를
//! 그대로 부릅니다. 모형은 그때 아무 일도 하지 않으므로 `b`는 두 쪽이 다르게
//! 다루는 하나뿐인 입력이고, 그때 도는 도메인의 큐가 비어 있어서 결과가 같습니다.
//!
//! 상태 한 줄은 `seen=N tbl=D:W,... ep=N pend=시작/D:W,... rd=D cur=T q=[..|..]
//! wl=[..|..] nx=B`입니다. 보류 표가 없으면 `pend=-`, 현재 태스크에 면제가 있으면
//! `cur=T*`, 실행 중인 태스크가 없으면 `cur=-`입니다. `q`와 `wl`은 도메인 0부터
//! 7까지의 준비 큐와 대기 목록을 `|`로 나눈 것이고 `nx`는
//! [Core::next_tick_switches]입니다.
//!
//! 해시는 FNV-1a 64비트입니다. 줄을 차례로, 줄마다 줄바꿈 바이트 하나를 붙여
//! 넣습니다. 14695981039346656037(0xcbf29ce484222325)에서 시작해 바이트마다 먼저
//! XOR하고 1099511628211(0x100000001b3)을 곱합니다(2^64로 나눈 나머지). 파일을
//! 작게 두려는 것일 뿐 보안 수단이 아닙니다.
//!
//! 이것은 테스트이지 증명이 아닙니다. 통과가 뜻하는 것은 "기록된 대본에서 Core와
//! 모형이 같은 줄을 냈다"까지입니다. [Host]는 목록으로 적은 Env이고 커널의 실제
//! 큐(TCB에 링크를 둔 목록)가 아닙니다. Core나 모형을 일부러 바꿨다면
//! `proofs/lean/README.md`의 절차대로 해시를 다시 만듭니다.

use std::collections::{BTreeMap, VecDeque};

use k0_slice::Verdict;

use super::*;

const VECTORS: &str = include_str!("model_vectors.txt");

/// FNV-1a 64비트의 시작값
const BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// 어느 갈래를 몇 번 지났는지 세는 표
type Cover = BTreeMap<String, u32>;

/// 파일의 대본이 지나는 갈래의 전부입니다.
///
/// 앞쪽은 "사건, 직전에 태스크가 실행 중이었는지(run, idle): 그 사건에서 일어난
/// 일"입니다. 일어난 일은 Core가 Env에 한 호출의 순서이고 empty는 빈 큐에서
/// 꺼내려 한 것입니다. 깨움과 표 바꾸기에는 호출이 없으므로 결과를 적습니다.
/// 뒤쪽은 대본에 든 입력의 종류입니다.
const NEED: [&str; 47] = [
    // 이미 처리한 번호의 틱
    "tick again run:", "tick again idle:",
    // 같은 도메인의 틱: 면제로 유지, 교대, 혼자라 유지, 대기 중 진입, 계속 대기
    "tick run: serve", "tick run: serve pop push run", "tick run: serve empty",
    "tick idle: serve pop run", "tick idle: serve empty",
    // 창의 경계: 세워 두기(면제, 교대, 혼자, 태스크 없음)와 새 도메인 진입(있음, 없음)
    "tick run: front enter serve pop run", "tick run: front enter serve empty",
    "tick run: pop push front enter serve pop run", "tick run: pop push front enter serve empty",
    "tick run: empty front enter serve pop run", "tick run: empty front enter serve empty",
    "tick idle: enter serve pop run", "tick idle: enter serve empty",
    // 새 틱이 없는 poll, 양보, 블록
    "poll run: serve", "poll idle: serve pop run", "poll idle: serve empty",
    "yield run: pop push run", "yield run: empty", "yield idle:",
    "block run: pop run", "block run: empty", "block idle: empty",
    // 깨움: 기다리던 태스크, 기다리지 않는 태스크, 대상이 아니라 무시, 대기 중이라 무시
    "wake run: woke", "wake run: not waiting", "wake run:", "wake idle:",
    "wakeX run: woke", "wakeX run: not waiting", "wakeX run:", "wakeX idle:",
    // 표 바꾸기: 보류 표가 없을 때, 같은 틱에 한 번 더, 다음 틱에 한 번 더
    "set run: first", "set run: same tick", "set run: next tick",
    "set idle: first", "set idle: same tick", "set idle: next tick",
    // 입력의 종류
    "poll with a new tick", "jump", "table starts", "table starts late", "one-domain table",
    "next tick from the pending table", "two waiters at once", "deadline in the past", "ten idle events",
];

fn num(s: &str) -> u64 {
    s.parse().unwrap_or_else(|_| panic!("bad number '{s}'"))
}

fn domain(s: &str) -> Domain {
    u8::try_from(num(s)).ok().and_then(Domain::new).unwrap_or_else(|| panic!("bad domain '{s}'"))
}

fn deadline(s: &str) -> Option<u64> {
    (s != "-").then(|| num(s))
}

fn table(s: &str) -> Table {
    let entry = |e: &str| {
        let (d, w) = e.split_once(':').expect("table entry");
        (domain(d), u32::try_from(num(w)).expect("window"))
    };
    Table::from_entries(&s.split(',').map(entry).collect::<Vec<_>>()).expect("table")
}

fn count(cover: &mut Cover, on: bool, key: &str) {
    *cover.entry(key.to_string()).or_default() += u32::from(on);
}

fn join<T: ToString>(items: impl Iterator<Item = T>, sep: &str) -> String {
    items.map(|x| x.to_string()).collect::<Vec<_>>().join(sep)
}

/// Env를 목록으로 적은 것입니다. 준비 큐와 대기 목록이 도메인마다 하나씩입니다.
///
/// 대기자는 기다리기 시작한 순서대로 있고 기한은 틱 번호입니다(없으면 None).
/// `did`는 사건 하나에서 일어난 일을 차례로 적은 것이고 `woke`는 그 사건에서
/// 기한이 지나 깨어난 대기자의 수입니다. 둘은 갈래를 세는 데만 씁니다.
struct Host {
    now: u64,
    dom: Vec<Domain>,
    q: Vec<VecDeque<u64>>,
    wl: Vec<Vec<(u64, Option<u64>)>>,
    did: String,
    woke: usize,
}

impl Host {
    fn dom(&self, t: u64) -> usize {
        self.dom[t as usize].index()
    }

    /// 깨우는 쪽(커널의 enqueue)이 하는 일입니다. Core의 사건이 아니며, 기다리던
    /// 태스크를 자기 도메인 큐의 뒤에 넣는 것이 전부입니다.
    fn wake(&mut self, t: u64) {
        let d = self.dom(t);
        let waiting = self.wl[d].iter().any(|w| w.0 == t);
        if waiting {
            self.wl[d].retain(|w| w.0 != t);
            self.q[d].push_back(t);
        }
        self.did += if waiting { " woke" } else { " not waiting" };
    }
}

impl Env<u64> for Host {
    fn now(&self) -> u64 {
        self.now
    }

    fn domain(&self, t: u64) -> Domain {
        self.dom[t as usize]
    }

    fn push(&mut self, d: Domain, t: u64) {
        self.q[d.index()].push_back(t);
        self.did += " push";
    }

    fn push_front(&mut self, d: Domain, t: u64) {
        self.q[d.index()].push_front(t);
        self.did += " front";
    }

    fn pop(&mut self, d: Domain) -> Option<u64> {
        let x = self.q[d.index()].pop_front();
        self.did += if x.is_some() { " pop" } else { " empty" };
        x
    }

    fn run(&mut self, _: u64) {
        self.did += " run";
    }

    fn enter_domain(&mut self, _: Domain) {
        self.did += " enter";
    }

    fn serve(&mut self, d: Domain, now: u64) {
        let due = |w: &(u64, Option<u64>)| w.1.is_some_and(|k| k <= now);
        let (q, wl) = (&mut self.q[d.index()], &mut self.wl[d.index()]);
        self.woke += wl.iter().filter(|w| due(w)).count();
        // 커널의 순서: 늦게 기다리기 시작한 대기자부터 큐에 들어감
        // (k0_sched::arm_timeout은 목록의 머리에 걸고 k0_sched::expired는 머리부터 봄)
        q.extend(wl.iter().rev().filter(|w| due(w)).map(|w| w.0));
        wl.retain(|w| !due(w));
        self.did += " serve";
    }
}

/// 상태 한 줄을 적는 함수입니다. `ModelVectors.lean`의 `showSt`와 같은 글을 냅니다.
fn line(core: &Core<u64>, h: &Host) -> String {
    let table = |t: &Table| join((0..t.len()).map(|i| format!("{}:{}", t.dom[i], t.ticks[i])), ",");
    let pend = core.pending.map_or("-".to_string(), |(t, s)| format!("{s}/{}", table(&t)));
    // 면제는 k0-slice 안의 값이라 사본에 틱을 한 번 물어서 읽음
    let mut probe = core.slice;
    let cur = match core.running {
        Some(c) if probe.on_tick() == Verdict::Keep => format!("{c}*"),
        Some(c) => c.to_string(),
        None => "-".to_string(),
    };
    let waiter = |&(t, d): &(u64, Option<u64>)| d.map_or(format!("{t}@-"), |k| format!("{t}@{k}"));
    let q = join(h.q.iter().map(|q| join(q.iter(), " ")), "|");
    let wl = join(h.wl.iter().map(|l| join(l.iter().map(waiter), " ")), "|");
    let (seen, tbl, ep, rd) = (core.seen, table(&core.table), core.epoch, core.rd.id());
    let nx = u8::from(core.next_tick_switches());
    format!("seen={seen} tbl={tbl} ep={ep} pend={pend} rd={rd} cur={cur} q=[{q}] wl=[{wl}] nx={nx}")
}

/// 사건 하나(`tok`, 모듈 설명의 표기)를 Core에 넣고 지난 갈래를 세는 함수입니다.
fn step(core: &mut Core<u64>, h: &mut Host, tok: &str, cover: &mut Cover) {
    let (seen, running) = (core.seen, core.running);
    let pending = core.pending.map(|(_, start)| start);
    let rd = core.rd;
    h.did.clear();
    h.woke = 0;
    let (kind, arg) = tok.split_at(1);
    let name = match kind {
        "t" | "p" => {
            let n = num(arg);
            h.now = n;
            let r = if kind == "p" { core.poll(h) } else { core.tick(h) };
            assert_eq!(r, core.status(), "the returned value must name the running task");
            count(cover, n > seen && kind == "p", "poll with a new tick");
            count(cover, n > seen + 1, "jump");
            count(cover, pending == Some(n), "table starts");
            count(cover, pending.is_some_and(|s| s < n), "table starts late");
            match kind {
                _ if n > seen => "tick",
                "p" => "poll",
                _ => "tick again",
            }
        }
        "y" if arg.is_empty() => {
            core.yield_now(h);
            "yield"
        }
        "b" => {
            // 떠나는 태스크는 Core를 부르기 전에 대기 목록에 걸려 있어야 함
            if let Some(c) = running {
                let (d, dl) = (h.dom(c), deadline(arg));
                h.wl[d].push((c, dl));
                count(cover, dl.is_some_and(|k| k <= seen), "deadline in the past");
            }
            let r = core.block(h);
            assert_eq!(r, core.status(), "the returned value must name the running task");
            "block"
        }
        "w" | "x" => {
            // 깨움은 실행 중인 태스크의 행동이고 w는 같은 도메인, x는 다른 도메인만 깨움
            let t = num(arg);
            let own = h.dom(t) == core.rd.index();
            if running.is_some() && own == (kind == "w") {
                h.wake(t);
            }
            if kind == "w" { "wake" } else { "wakeX" }
        }
        "s" => {
            let t = table(arg);
            h.did += match pending {
                None => " first",
                Some(s) if s == seen + 2 => " same tick",
                Some(_) => " next tick",
            };
            count(cover, (0..t.len()).all(|i| t.dom[i] == t.dom[0]), "one-domain table");
            core.set_table(t);
            "set"
        }
        _ => panic!("bad event '{tok}'"),
    };
    count(cover, h.woke >= 2, "two waiters at once");
    count(cover, core.pending.is_some_and(|(_, s)| s == core.seen + 1), "next tick from the pending table");
    // 해시는 상태 줄만 덮으므로, 도메인이 바뀔 때만 그리고 바뀔 때마다 환경에
    // 알렸는지는 여기서 따로 봄
    assert_eq!(h.did.contains(" enter"), core.rd != rd, "the environment hears of every domain change and of nothing else: {tok}");
    let was = if running.is_some() { "run" } else { "idle" };
    count(cover, true, &format!("{name} {was}:{}", h.did));
}

/// 대본 한 줄(해시를 뺀 일곱 칸)을 Core에 넣고 부팅 직후와 사건마다의 상태 줄을
/// (사건의 글, 줄)로 돌려주는 함수입니다.
fn run<'a>(f: &[&'a str], cover: &mut Cover) -> Vec<(&'a str, String)> {
    let waiter = |w: &str| {
        let (t, d) = w.split_once('@').expect("waiter");
        (num(t), deadline(d))
    };
    let dom: Vec<Domain> = f[2].split_whitespace().map(domain).collect();
    let (q, wl) = (vec![VecDeque::new(); MAX_DOMAINS], vec![Vec::new(); MAX_DOMAINS]);
    let mut h = Host { now: num(f[1]), dom, q, wl, did: String::new(), woke: 0 };
    for t in f[4].split_whitespace().map(num) {
        let d = h.dom(t);
        h.q[d].push_back(t);
    }
    for w in f[5].split_whitespace().map(waiter) {
        let d = h.dom(w.0);
        h.wl[d].push(w);
    }
    let mut core: Core<u64> = Core::new();
    core.start(&mut h, num(f[3]));
    let mut out = vec![("boot", line(&core, &h))];
    let mut idle = 0;
    for tok in f[6].split_whitespace() {
        step(&mut core, &mut h, tok, cover);
        out.push((tok, line(&core, &h)));
        idle = if core.running.is_none() { idle + 1 } else { 0 };
        count(cover, idle == 10, "ten idle events");
    }
    out
}

/// FNV-1a 64비트를 바이트열만큼 진행하는 함수입니다.
fn fnv(h: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(h, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// 글에 든 대본을 모두 돌리는 함수입니다. 대본의 수와, 해시가 모형과 다른
/// 대본마다 대본과 다시 계산한 줄을 적은 보고를 돌려줍니다.
fn check(text: &str, cover: &mut Cover) -> (usize, Vec<String>) {
    let mut bad = Vec::new();
    let scripts: Vec<&str> = text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    for l in &scripts {
        let f: Vec<&str> = l.split(';').collect();
        assert_eq!(f.len(), 8, "a script has eight fields: {l}");
        let lines = run(&f, cover);
        let got = lines.iter().fold(BASIS, |h, (_, l)| fnv(fnv(h, l.as_bytes()), b"\n"));
        if format!("{got:016x}") != f[7] {
            let shown = join(lines.iter().map(|(tok, l)| format!("  {tok} -> {l}\n")), "");
            bad.push(format!("the model gives {}, Core gives {got:016x} on the script\n{l}\n{shown}", f[7]));
        }
    }
    (scripts.len(), bad)
}

#[test]
fn core_gives_the_lines_of_the_lean_model_on_every_recorded_script() {
    let mut cover = Cover::new();
    let (n, bad) = check(VECTORS, &mut cover);
    let first = bad[..bad.len().min(3)].join("\n");
    assert!(bad.is_empty(), "{} of {n} scripts differ from the Lean model, the first ones:\n{first}", bad.len());
    println!("{n} scripts, {cover:#?}");
    // 줄이 빠진 파일을 통과시키지 않음. 대본을 다시 만들어 수가 달라지면 여기도 고침
    assert_eq!(n, 1600, "the file must hold every recorded script");
    for key in NEED {
        let hits = cover.get(key).copied().unwrap_or(0);
        assert!(hits >= 100, "the scripts must reach '{key}' many times: {hits}");
    }
    for key in cover.keys() {
        assert!(NEED.contains(&key.as_str()), "a path that the list does not name: '{key}'");
    }
}

#[test]
fn a_wrong_hash_and_a_changed_script_are_reported() {
    let line = VECTORS.lines().find(|l| !l.is_empty() && !l.starts_with('#')).unwrap();
    let (body, hash) = line.rsplit_once(';').unwrap();
    let mut cover = Cover::new();
    assert_eq!(check(line, &mut cover).1.len(), 0);
    // 해시의 마지막 자리를 바꿈
    let digit = if hash.ends_with('0') { '1' } else { '0' };
    let (n, bad) = check(&format!("{body};{}{digit}", &hash[..15]), &mut cover);
    assert_eq!((n, bad.len()), (1, 1));
    assert!(bad[0].contains(body) && bad[0].contains("  boot -> seen="), "the report must show the script and its lines");
    // 마지막 사건을 뺌
    let shorter = body.rsplit_once(' ').unwrap().0;
    assert_eq!(check(&format!("{shorter};{hash}"), &mut cover).1.len(), 1);
}

#[test]
fn the_hash_is_fnv_1a_64() {
    assert_eq!(fnv(BASIS, b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv(BASIS, b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv(BASIS, b"foobar"), 0x8594_4171_f739_67e8);
}
