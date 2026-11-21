//! 진짜 Core를 이산 사건 모형 위에서 돌려 검증하는 테스트 모듈입니다.
//!
//! 모형은 커널이 하는 일을 틱 하나를 [STEPS]칸으로 나눈 시간 위에서 흉내
//! 냅니다. 틱이 오면 먼저 틱 사건을 처리하고, 칸마다 도는 태스크가 프로그램의
//! 연산 하나를 합니다. 실행 중인 태스크가 없으면(Idle) 그 칸은 비며, 입력이
//! 도착했을 때만 대기 루프가 poll을 부릅니다. 틱은 태스크가 도는 중이면 tick,
//! 커널이 기다리는 중이면 poll로 들어옵니다(실제 커널의 두 경로).
//!
//! 기준은 Core와 따로 적었습니다. [Plan]은 표의 효력 시점(설정한 틱 + 2)과
//! 한 바퀴 펼치기를 Core와 다른 방식으로 적은 기준이고, [Host]는 Env의 구현이면서
//! 호출이 올바른지 스스로 검사합니다(창 밖의 실행, 다른 도메인의 일, 경계에서
//! enter_domain 누락 등). 위반은 첫 하나만 기록하고 시뮬레이션이 끝난 뒤에
//! 보고합니다.
//!
//! 깨뜨리기 시험을 위해 Core의 사건 논리를 옮겨 적고 한 줄씩 틀리게 만든
//! [MutCore]를 둡니다. 실제 Core는 테스트에서 변형할 수 없으므로 이 방식을
//! 씁니다. 틀림 없는 [MutCore]가 실제 Core와 같은 결과를 내는지는 따로
//! 확인하므로(`copy_equals_core`), 옮겨 적은 쪽이 갈라지면 그 테스트가 알립니다.
//!
//! 모형이 다루지 않는 것은 다음과 같습니다. 시스템 콜에 걸리는 시간, 커널 경로의
//! 길이, 실제 준비 큐와 기한 목록의 구현, 캐시와 같은 마이크로 아키텍처 상태입니다.

use std::collections::VecDeque;

use k0_slice::{Entry, Slice, Verdict};

use super::*;

/// 틱 하나를 나눈 칸의 수
const STEPS: u8 = 3;

/// 공유 오브젝트(세마포어)의 수. 도메인 d는 2d와 2d+1번을 씀
const NOBJ: usize = 16;

/// 태스크가 칸 하나에 하는 연산입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// CPU를 내놓지 않고 돎
    Spin,
    Yield,
    /// 틱 n개짜리 기한 대기
    Wait(u64),
    /// 오브젝트를 기다림(Post가 있을 때까지)
    Pend(usize),
    /// 오브젝트에 신호를 보냄. 다른 도메인의 오브젝트면 명시적인 공유 채널임
    Post(usize),
    /// 입력을 기다림
    Read,
    Exit,
}

/// 태스크 하나의 프로그램과 도메인, 입력이 도착하는 (틱, 칸)입니다.
#[derive(Clone, Debug)]
struct Spec {
    domain: u8,
    prog: Vec<Op>,
    inputs: Vec<(u64, u8)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    Timeout,
    Input,
    Post,
}

/// 도메인의 흔적에 남는 사건입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    Ran { tick: u64, step: u8, task: usize },
    Woke { tick: u64, step: u8, task: usize, why: Why },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Ready,
    Running,
    Timed(u64),
    Pending(usize),
    Reading,
    Dead,
}

struct Task {
    spec: Spec,
    pc: usize,
    st: St,
    input: u32,
}

/// 표 변경의 효력 시점을 Core와 다르게 적은 기준입니다. 설정한 시점에 아직
/// 효력이 없던 표는 새 설정에 밀려 버려집니다.
struct Plan {
    segs: Vec<(u64, Table)>,
}

impl Plan {
    fn new() -> Self {
        Self { segs: vec![(0, Table::DEFAULT)] }
    }

    fn set(&mut self, now: u64, t: Table) {
        while self.segs.len() > 1 && self.segs.last().unwrap().0 > now {
            self.segs.pop();
        }
        self.segs.push((now + 2, t));
    }

    fn domain_at(&self, k: u64) -> Domain {
        let (s, t) = self.segs.iter().rev().find(|(s, _)| *s <= k).unwrap();
        let mut cycle = Vec::new();
        for i in 0..t.len() {
            let (d, w) = t.entry(i).unwrap();
            for _ in 0..w {
                cycle.push(d);
            }
        }
        cycle[((k - s) % cycle.len() as u64) as usize]
    }
}

/// Core가 Env에 한 호출입니다. 깨우는 쪽(커널의 enqueue)이 하는 일은 담지 않습니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Push(Domain, usize),
    PushFront(Domain, usize),
    Pop(Domain, Option<usize>),
    Run(usize),
    Enter(Domain),
    Serve(Domain, u64),
}

/// 검사가 어떤 사건을 얼마나 보았는지 세는 값입니다. 속성이 비어 있지 않음을 보입니다.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stats {
    events: u64,
    boundaries: u64,
    boundaries_running: u64,
    boundaries_rotated: u64,
    same_domain_ticks: u64,
    idle_entries: u64,
    polls_without_tick: u64,
    yields: u64,
    blocks: u64,
    repeats: u64,
    jumps: u64,
    partitioned_events: u64,
}

/// Env의 구현이자 호출을 검사하는 기준입니다.
struct Host {
    now: u64,
    step: u8,
    tasks: Vec<Task>,
    q: Vec<VecDeque<usize>>,
    current: Option<usize>,
    entered: Domain,
    plan: Plan,
    sems: Vec<u32>,
    waiters: Vec<VecDeque<usize>>,
    trace: Vec<(Domain, Ev)>,
    runs: Vec<(u64, usize)>,
    bad: Option<String>,
    calls: Vec<Call>,
    kcur: Option<usize>,
    seen: u64,
    stats: Stats,
}

impl Host {
    fn new(specs: &[Spec]) -> Self {
        Self {
            now: 0,
            step: 0,
            tasks: specs
                .iter()
                .map(|s| Task { spec: s.clone(), pc: 0, st: St::Ready, input: 0 })
                .collect(),
            q: (0..MAX_DOMAINS).map(|_| VecDeque::new()).collect(),
            current: None,
            entered: Domain::ROOT,
            plan: Plan::new(),
            sems: vec![0; NOBJ],
            waiters: (0..NOBJ).map(|_| VecDeque::new()).collect(),
            trace: Vec::new(),
            runs: Vec::new(),
            bad: None,
            calls: Vec::new(),
            kcur: None,
            seen: 0,
            stats: Stats::default(),
        }
    }

    fn fail(&mut self, msg: String) {
        if self.bad.is_none() {
            self.bad = Some(format!("tick {} step {}: {}", self.now, self.step, msg));
        }
    }

    fn dom(&self, t: usize) -> Domain {
        Domain::new(self.tasks[t].spec.domain).unwrap()
    }

    fn record(&mut self, t: usize, ev: Ev) {
        let d = self.dom(t);
        self.trace.push((d, ev));
    }

    /// 깨우는 쪽이 부르는 함수입니다. Core의 사건이 아니라 태스크 자신의 도메인
    /// 큐에 넣는 것이 전부입니다.
    fn wake(&mut self, t: usize, why: Why) {
        self.tasks[t].st = St::Ready;
        let d = self.dom(t);
        self.record(t, Ev::Woke { tick: self.now, step: self.step, task: t, why });
        self.enqueue_check(d, t);
        self.q[d.index()].push_back(t);
    }

    fn enqueue_check(&mut self, d: Domain, t: usize) {
        if d != self.dom(t) {
            self.fail(format!("task {t} pushed into the queue of domain {}", d.id()));
        }
        if self.q.iter().any(|q| q.contains(&t)) {
            self.fail(format!("task {t} queued twice"));
        }
        if self.current == Some(t) {
            self.current = None;
        }
        self.tasks[t].st = St::Ready;
    }

    fn check_next(&mut self, r: Next<usize>) {
        match r {
            Next::Run(x) => {
                if self.tasks[x].st != St::Running || self.current != Some(x) {
                    self.fail(format!("Next::Run({x}) but the task is not the running one"));
                }
            }
            Next::Idle => {
                if self.current.is_some() {
                    self.fail("Next::Idle while a task is current".into());
                }
            }
        }
    }
}

impl Env<usize> for Host {
    fn now(&self) -> u64 {
        self.now
    }

    fn domain(&self, t: usize) -> Domain {
        self.dom(t)
    }

    fn push(&mut self, d: Domain, t: usize) {
        self.calls.push(Call::Push(d, t));
        self.enqueue_check(d, t);
        self.q[d.index()].push_back(t);
    }

    fn push_front(&mut self, d: Domain, t: usize) {
        self.calls.push(Call::PushFront(d, t));
        self.enqueue_check(d, t);
        self.q[d.index()].push_front(t);
    }

    fn pop(&mut self, d: Domain) -> Option<usize> {
        let r = self.q[d.index()].pop_front();
        self.calls.push(Call::Pop(d, r));
        r
    }

    fn run(&mut self, t: usize) {
        let want = self.plan.domain_at(self.now);
        if self.dom(t) != want {
            self.fail(format!("run({t}) of domain {} in a window of domain {}", self.dom(t).id(), want.id()));
        }
        if self.entered != self.dom(t) {
            self.fail(format!("run({t}) before enter_domain({})", self.dom(t).id()));
        }
        self.calls.push(Call::Run(t));
        self.tasks[t].st = St::Running;
        self.current = Some(t);
        self.kcur = Some(t);
        self.runs.push((self.now, t));
    }

    fn enter_domain(&mut self, d: Domain) {
        self.calls.push(Call::Enter(d));
        if d != self.plan.domain_at(self.now) {
            self.fail(format!("enter_domain({}) outside its window", d.id()));
        }
        if d == self.entered {
            self.fail(format!("enter_domain({}) without a change", d.id()));
        }
        self.entered = d;
    }

    fn serve(&mut self, d: Domain, now: u64) {
        self.calls.push(Call::Serve(d, now));
        if d != self.plan.domain_at(self.now) || now != self.now {
            self.fail(format!("serve({}) outside its window", d.id()));
        }
        if d != self.entered {
            self.fail(format!("serve({}) while the kernel is still in domain {}", d.id(), self.entered.id()));
        }
        for t in 0..self.tasks.len() {
            if self.dom(t) == d && self.tasks[t].st == St::Reading && self.tasks[t].input > 0 {
                self.tasks[t].input -= 1;
                self.wake(t, Why::Input);
            }
        }
        for t in 0..self.tasks.len() {
            if self.dom(t) == d
                && let St::Timed(dl) = self.tasks[t].st
                && dl <= now
            {
                self.wake(t, Why::Timeout);
            }
        }
    }
}

/// 시뮬레이션이 부르는 스케줄러의 사건입니다. 실제 Core, 틀리게 만든 사본, 얼린
/// 옛 스케줄러가 같은 모양을 가집니다.
trait Sched {
    fn start(&mut self, h: &mut Host, first: usize);
    fn yield_now(&mut self, h: &mut Host);
    fn block(&mut self, h: &mut Host) -> Next<usize>;
    fn tick(&mut self, h: &mut Host) -> Next<usize>;
    fn poll(&mut self, h: &mut Host) -> Next<usize>;
    fn set_table(&mut self, t: Table);
    fn next_tick_switches(&self) -> bool;
    fn running_domain(&self) -> Domain;
    /// 스케줄러가 현재로 보는 태스크입니다.
    fn running(&self, h: &Host) -> Option<usize>;
    fn partitioned(&self) -> bool;
    /// 같은 틱 번호로 틱 사건을 되풀이해 부를 수 있는지(옛 스케줄러는 안 됨)
    fn idempotent_tick(&self) -> bool {
        true
    }
}

impl Sched for Core<usize> {
    fn start(&mut self, h: &mut Host, first: usize) {
        Core::start(self, h, first)
    }
    fn yield_now(&mut self, h: &mut Host) {
        Core::yield_now(self, h)
    }
    fn block(&mut self, h: &mut Host) -> Next<usize> {
        Core::block(self, h)
    }
    fn tick(&mut self, h: &mut Host) -> Next<usize> {
        Core::tick(self, h)
    }
    fn poll(&mut self, h: &mut Host) -> Next<usize> {
        Core::poll(self, h)
    }
    fn set_table(&mut self, t: Table) {
        Core::set_table(self, t)
    }
    fn next_tick_switches(&self) -> bool {
        Core::next_tick_switches(self)
    }
    fn running_domain(&self) -> Domain {
        Core::running_domain(self)
    }
    fn running(&self, _: &Host) -> Option<usize> {
        self.running
    }
    fn partitioned(&self) -> bool {
        Core::partitioned(self)
    }
}

/// 사건의 종류입니다. 사건마다 Env 호출의 순서가 정해져 있습니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Start(usize),
    Yield,
    Block,
    Tick,
    Poll,
    Set,
}

/// 사건 직전의 상태입니다.
struct Before {
    running: Option<usize>,
    rd: Domain,
    calls: usize,
    kcur: Option<usize>,
}

fn begin(h: &Host, sc: &dyn Sched) -> Before {
    Before { running: sc.running(h), rd: sc.running_domain(), calls: h.calls.len(), kcur: h.kcur }
}

/// 틱 사건(또는 틱이 든 poll)이 낸 호출이 설계가 정한 순서인지 봅니다.
///
/// 경계에서는 `(도는 태스크에 틱 규칙을 적용해 세워 둠) Enter(새) Serve(새) Pop(새) (Run)`
/// 이고 같은 도메인에서는 `Serve(옛)` 뒤에 규칙의 호출입니다.
fn tick_grammar(b: &Before, calls: &[Call], n: u64, new: Domain, r: Next<usize>) -> Result<(), String> {
    let old = b.rd;
    let done = |i: usize, got: Option<usize>| -> Result<(), String> {
        let want_len = i + usize::from(got.is_some());
        if calls.len() != want_len || got.is_some_and(|x| calls[i] != Call::Run(x)) {
            return Err(format!("calls after the pop: {:?}", &calls[i.min(calls.len())..]));
        }
        let want = got.map_or(Next::Idle, Next::Run);
        if r != want { Err(format!("result {r:?} want {want:?}")) } else { Ok(()) }
    };
    if new != old {
        let i = match (b.running, calls) {
            (None, _) => 0,
            (Some(c), [Call::PushFront(d, x), ..]) if *d == old && *x == c => 1,
            (Some(c), [Call::Pop(d, None), Call::PushFront(e, x), ..]) if *d == old && *e == old && *x == c => 2,
            (Some(c), [Call::Pop(d, Some(x)), Call::Push(e, y), Call::PushFront(f, z), ..])
                if *d == old && *e == old && *f == old && *y == c && z == x =>
            {
                3
            }
            _ => return Err(format!("the rule at the boundary did not park the running task first: {calls:?}")),
        };
        let rest = &calls[i..];
        let [Call::Enter(e), Call::Serve(s, m), Call::Pop(p, got), ..] = rest else {
            return Err(format!("expected enter_domain, serve, pop after the rule: {rest:?}"));
        };
        if *e != new || *s != new || *m != n || *p != new {
            return Err(format!("wrong arguments at the boundary: {rest:?} (new {} tick {n})", new.id()));
        }
        return done(i + 3, *got);
    }
    let [Call::Serve(s, m), ..] = calls else {
        return Err(format!("a tick in the same domain must start with serve: {calls:?}"));
    };
    if *s != old || *m != n {
        return Err(format!("serve({}, {m}) but the domain {} runs at tick {n}", s.id(), old.id()));
    }
    match (b.running, &calls[1..]) {
        (Some(c), []) => {
            if r != Next::Run(c) {
                return Err(format!("result {r:?} want the running task {c}"));
            }
            Ok(())
        }
        (Some(c), [Call::Pop(d, None)]) if *d == old => {
            if r != Next::Run(c) { Err(format!("result {r:?} want the running task {c}")) } else { Ok(()) }
        }
        (Some(c), [Call::Pop(d, Some(x)), Call::Push(e, y), Call::Run(z)]) if *d == old && *e == old && *y == c && z == x => {
            if r != Next::Run(*x) { Err(format!("result {r:?} want {x}")) } else { Ok(()) }
        }
        (None, [Call::Pop(d, got), ..]) if *d == old => done(2, *got),
        (_, other) => Err(format!("unexpected calls after serve: {other:?}")),
    }
}

/// 사건이 낸 호출과 사건 뒤의 상태를 검사합니다. 위반은 [Host::fail]로 기록합니다.
///
/// 상태 규칙은 설계의 I1(Running인 TCB는 스케줄러의 현재 태스크이고 그것뿐), I2(돌리는
/// 태스크가 없으면 Running인 TCB가 없음), I3(커널의 current는 `Env::run`에서만 바뀜)이고,
/// 도는 도메인과 `next_tick_switches`는 기준 [Plan]과 견줍니다.
fn finish(h: &mut Host, sc: &dyn Sched, kind: Kind, b: Before, r: Option<Next<usize>>) {
    let calls: Vec<Call> = h.calls[b.calls..].to_vec();
    let n = h.now;
    let seen0 = h.seen;
    let new = h.plan.domain_at(n);
    h.stats.events += 1;
    let res: Result<(), String> = (|| {
        match kind {
            Kind::Start(first) => {
                if calls != [Call::Run(first)] {
                    return Err(format!("start must only run the first task: {calls:?}"));
                }
            }
            Kind::Set => {
                if !calls.is_empty() {
                    return Err(format!("set_table must not touch the environment: {calls:?}"));
                }
            }
            Kind::Yield => match (b.running, calls.as_slice()) {
                (None, []) => {}
                (Some(_), [Call::Pop(d, None)]) if *d == b.rd => {}
                (Some(c), [Call::Pop(d, Some(x)), Call::Push(e, y), Call::Run(z)])
                    if *d == b.rd && *e == b.rd && *y == c && z == x => {}
                _ => return Err(format!("yield: {calls:?}")),
            },
            Kind::Block => {
                let r = r.unwrap();
                match calls.as_slice() {
                    [Call::Pop(d, None)] if *d == b.rd && r == Next::Idle => {}
                    [Call::Pop(d, Some(x)), Call::Run(y)] if *d == b.rd && x == y && r == Next::Run(*x) => {}
                    _ => return Err(format!("block of the domain {}: {calls:?} result {r:?}", b.rd.id())),
                }
            }
            Kind::Tick | Kind::Poll => {
                let r = r.unwrap();
                if n > seen0 {
                    h.stats.jumps += u64::from(n > seen0 + 1);
                    if new != b.rd {
                        h.stats.boundaries += 1;
                        h.stats.boundaries_running += u64::from(b.running.is_some());
                        h.stats.boundaries_rotated +=
                            u64::from(calls.iter().any(|c| matches!(c, Call::Push(..))) && b.running.is_some());
                    } else {
                        h.stats.same_domain_ticks += 1;
                    }
                    if b.running.is_none() && matches!(r, Next::Run(_)) {
                        h.stats.idle_entries += 1;
                    }
                    tick_grammar(&b, &calls, n, new, r)?;
                } else if kind == Kind::Tick {
                    h.stats.repeats += 1;
                    if !calls.is_empty() {
                        return Err(format!("a tick event that brings no new tick must do nothing: {calls:?}"));
                    }
                    let want = b.running.map_or(Next::Idle, Next::Run);
                    if r != want {
                        return Err(format!("result {r:?} of a repeated tick, want {want:?}"));
                    }
                } else {
                    h.stats.polls_without_tick += 1;
                    let [Call::Serve(s, m), rest @ ..] = calls.as_slice() else {
                        return Err(format!("poll without a tick must start with serve: {calls:?}"));
                    };
                    if *s != b.rd || *m != n {
                        return Err(format!("poll serve({}, {m})", s.id()));
                    }
                    match (b.running, rest) {
                        (Some(c), []) if r == Next::Run(c) => {}
                        (None, [Call::Pop(d, None)]) if *d == b.rd && r == Next::Idle => {}
                        (None, [Call::Pop(d, Some(x)), Call::Run(y)]) if *d == b.rd && x == y && r == Next::Run(*x) => {}
                        _ => return Err(format!("poll without a tick: {rest:?} result {r:?}")),
                    }
                }
            }
        }
        if matches!(kind, Kind::Tick | Kind::Poll) && n > seen0 {
            h.seen = n;
        }
        if kind == Kind::Yield {
            h.stats.yields += 1;
        }
        if kind == Kind::Block {
            h.stats.blocks += 1;
        }
        // I1, I2
        let running = sc.running(h);
        let on: Vec<usize> = (0..h.tasks.len()).filter(|&t| h.tasks[t].st == St::Running).collect();
        match running {
            Some(c) => {
                if on != [c] {
                    return Err(format!("I1: the core runs {c} but the Running tasks are {on:?}"));
                }
                if h.kcur != Some(c) {
                    return Err(format!("I1: the core runs {c} but the kernel's current is {:?}", h.kcur));
                }
            }
            None => {
                if !on.is_empty() {
                    return Err(format!("I2: nothing runs but {on:?} are Running"));
                }
            }
        }
        // I3
        let last_run = calls.iter().rev().find_map(|c| if let Call::Run(x) = c { Some(*x) } else { None });
        match last_run {
            Some(x) if h.kcur != Some(x) => return Err(format!("I3: current {:?} after run({x})", h.kcur)),
            None if h.kcur != b.kcur => {
                return Err(format!("I3: current changed from {:?} to {:?} without run", b.kcur, h.kcur));
            }
            _ => {}
        }
        // 도는 도메인과 도메인 변경 예고
        let rd = sc.running_domain();
        let want = h.plan.domain_at(h.seen);
        if rd != want {
            return Err(format!("running_domain {} but the plan says {} at tick {}", rd.id(), want.id(), h.seen));
        }
        if h.now == h.seen && sc.next_tick_switches() != (h.plan.domain_at(h.seen + 1) != rd) {
            return Err(format!("next_tick_switches disagrees with the plan after tick {}", h.seen));
        }
        if sc.partitioned() {
            h.stats.partitioned_events += 1;
        }
        Ok(())
    })();
    if let Err(e) = res {
        h.fail(format!("{kind:?}: {e}"));
    }
}

fn ev_start(h: &mut Host, sc: &mut dyn Sched, first: usize) {
    let b = begin(h, sc);
    sc.start(h, first);
    finish(h, sc, Kind::Start(first), b, None);
}

fn ev_yield(h: &mut Host, sc: &mut dyn Sched) {
    let b = begin(h, sc);
    sc.yield_now(h);
    finish(h, sc, Kind::Yield, b, None);
}

fn ev_block(h: &mut Host, sc: &mut dyn Sched) -> Next<usize> {
    let b = begin(h, sc);
    let r = sc.block(h);
    finish(h, sc, Kind::Block, b, Some(r));
    r
}

fn ev_tick(h: &mut Host, sc: &mut dyn Sched) -> Next<usize> {
    let b = begin(h, sc);
    let r = sc.tick(h);
    finish(h, sc, Kind::Tick, b, Some(r));
    r
}

fn ev_poll(h: &mut Host, sc: &mut dyn Sched) -> Next<usize> {
    let b = begin(h, sc);
    let r = sc.poll(h);
    finish(h, sc, Kind::Poll, b, Some(r));
    r
}

/// 표를 정하는 사건입니다. 기준 Plan에도 같이 적습니다.
///
/// 시스템 콜은 센 틱을 전부 처리한 뒤에만 도므로 이 사건은 처리하지 않은 틱이
/// 남아 있으면 쓸 수 없습니다. 틱을 먼저 전하는 것은 부르는 쪽의 몫입니다.
fn ev_set(h: &mut Host, sc: &mut dyn Sched, t: Table) {
    if h.now != h.seen {
        h.fail(format!("set_table with the tick {} outstanding (processed {})", h.now, h.seen));
    }
    let b = begin(h, sc);
    sc.set_table(t);
    h.plan.set(h.seen, t);
    finish(h, sc, Kind::Set, b, None);
}

/// 얼린 옛 스케줄러입니다. crates/k0-sched/src/lib.rs의 이 브랜치(커밋
/// 6324648)의 `switch_to`, `rotate_as`, `rotate`, `on_tick`, `switch_next`,
/// `block_and_switch`, `resume_from_idle`, `handoff`를 준비 큐와 TCB만 Host로
/// 바꿔 옮겼고 경위 인자는 그대로입니다. 커널 쪽 연결은 crates/k0-slice/src/tests.rs의
/// `Wiring::Kernel`과 kernel/src/syscall.rs의 `k0_preempt`(콘솔 깨움과 기한
/// 처리를 한 뒤 on_tick), `idle_until_ready`(기다림 뒤 깨움과 기한 처리, 그다음
/// resume_from_idle)를 따랐습니다. 이 사본은 일부러 고치지 않습니다.
struct Legacy {
    slice: Slice,
    idle_before: u64,
}

impl Legacy {
    fn new() -> Self {
        Self { slice: Slice::new(), idle_before: 0 }
    }

    fn switch_to(&mut self, h: &mut Host, next: usize, how: Entry) {
        self.slice.entered(how);
        h.run(next);
    }

    fn rotate_as(&mut self, h: &mut Host, how: Entry) -> bool {
        let Some(next) = h.pop(Domain::ROOT) else { return false };
        let cur = h.current.unwrap();
        h.push(Domain::ROOT, cur);
        self.switch_to(h, next, how);
        true
    }

    fn on_tick(&mut self, h: &mut Host) {
        if self.slice.on_tick() == Verdict::Keep {
            return;
        }
        let _ = self.rotate_as(h, Entry::Tick);
    }

    fn switch_next(&mut self, h: &mut Host, how: Entry) -> bool {
        let Some(next) = h.pop(Domain::ROOT) else { return false };
        self.switch_to(h, next, how);
        true
    }

    fn status(h: &Host) -> Next<usize> {
        h.current.map_or(Next::Idle, Next::Run)
    }
}

impl Sched for Legacy {
    fn start(&mut self, h: &mut Host, first: usize) {
        self.slice.entered(Entry::Between);
        h.run(first);
    }
    fn yield_now(&mut self, h: &mut Host) {
        let _ = self.rotate_as(h, Entry::Between);
    }
    fn block(&mut self, h: &mut Host) -> Next<usize> {
        self.idle_before = h.now;
        self.switch_next(h, Entry::Between);
        Self::status(h)
    }
    fn tick(&mut self, h: &mut Host) -> Next<usize> {
        h.serve(Domain::ROOT, h.now);
        self.on_tick(h);
        Self::status(h)
    }
    fn poll(&mut self, h: &mut Host) -> Next<usize> {
        let tick_seen = h.now != self.idle_before;
        self.idle_before = h.now;
        h.serve(Domain::ROOT, h.now);
        let _ = self.switch_next(h, Entry::after_idle(tick_seen));
        Self::status(h)
    }
    fn set_table(&mut self, _t: Table) {}
    fn next_tick_switches(&self) -> bool {
        false
    }
    fn running_domain(&self) -> Domain {
        Domain::ROOT
    }
    fn running(&self, h: &Host) -> Option<usize> {
        h.current
    }
    fn partitioned(&self) -> bool {
        false
    }
    fn idempotent_tick(&self) -> bool {
        false
    }
}

/// [MutCore]에 한 번에 하나만 넣는 틀린 줄입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutant {
    None,
    YieldAsTick,
    BlockAsTick,
    PollInputAsTick,
    EnterAsBetween,
    StartAsTick,
    RotateAsBetween,
    BoundaryKeepToBack,
    BoundaryRotateToBack,
    ServeOldAtBoundary,
    PopOldDomain,
    EffectNextTick,
    NoRuleAtBoundary,
    NoEnterDomain,
    SlackDonation,
    PollIgnoresTick,
    SwitchesNeverTrue,
    SecondSetIgnored,
    CatchUpOneByOne,
    ServeBeforeEnter,
    ServeOldInstead,
    ReplaceStartsNextTick,
    SpuriousAsTick,
    BlockKeepsRunning,
    StartsInThree,
    SwitchesIgnoresPending,
}

const ALL_MUTANTS: [Mutant; 25] = [
    Mutant::YieldAsTick,
    Mutant::BlockAsTick,
    Mutant::PollInputAsTick,
    Mutant::EnterAsBetween,
    Mutant::StartAsTick,
    Mutant::RotateAsBetween,
    Mutant::BoundaryKeepToBack,
    Mutant::BoundaryRotateToBack,
    Mutant::ServeOldAtBoundary,
    Mutant::PopOldDomain,
    Mutant::EffectNextTick,
    Mutant::NoRuleAtBoundary,
    Mutant::NoEnterDomain,
    Mutant::SlackDonation,
    Mutant::PollIgnoresTick,
    Mutant::SwitchesNeverTrue,
    Mutant::SecondSetIgnored,
    Mutant::CatchUpOneByOne,
    Mutant::ServeBeforeEnter,
    Mutant::ServeOldInstead,
    Mutant::ReplaceStartsNextTick,
    Mutant::SpuriousAsTick,
    Mutant::BlockKeepsRunning,
    Mutant::StartsInThree,
    Mutant::SwitchesIgnoresPending,
];

/// lib.rs의 Core를 옮겨 적고 `m`이 가리키는 줄 하나만 틀리게 한 사본입니다.
struct MutCore {
    m: Mutant,
    table: Table,
    epoch: u64,
    pending: Option<(Table, u64)>,
    seen: u64,
    rd: Domain,
    running: Option<usize>,
    slice: Slice,
}

impl MutCore {
    fn new(m: Mutant) -> Self {
        Self {
            m,
            table: Table::DEFAULT,
            epoch: 0,
            pending: None,
            seen: 0,
            rd: Domain::ROOT,
            running: None,
            slice: Slice::new(),
        }
    }

    fn plan_at(&self, k: u64) -> Domain {
        if let Some((t, s)) = &self.pending
            && k >= *s
        {
            return t.domain_at(k - *s);
        }
        self.table.domain_at(k.saturating_sub(self.epoch))
    }

    fn advance(&mut self, n: u64) {
        if let Some((t, s)) = self.pending
            && s <= n
        {
            self.table = t;
            self.epoch = s;
            self.pending = None;
        }
    }

    fn status(&self) -> Next<usize> {
        self.running.map_or(Next::Idle, Next::Run)
    }

    fn switch(&mut self, h: &mut Host, x: usize, how: Entry) {
        self.slice.entered(how);
        self.running = Some(x);
        h.run(x);
    }

    fn enter(&mut self, h: &mut Host, d: Domain) -> Next<usize> {
        let how = if self.m == Mutant::EnterAsBetween { Entry::Between } else { Entry::Tick };
        let mut got = h.pop(d);
        if got.is_none() && self.m == Mutant::SlackDonation {
            for i in 0..MAX_DOMAINS {
                if got.is_none() {
                    got = h.pop(Domain::new(i as u8).unwrap());
                }
            }
        }
        match got {
            Some(x) => {
                self.switch(h, x, how);
                Next::Run(x)
            }
            None => Next::Idle,
        }
    }
}

impl Sched for MutCore {
    fn start(&mut self, h: &mut Host, first: usize) {
        let m = self.m;
        *self = MutCore::new(m);
        self.seen = h.now;
        self.epoch = self.seen;
        let how = if m == Mutant::StartAsTick { Entry::Tick } else { Entry::Between };
        self.switch(h, first, how);
    }

    fn yield_now(&mut self, h: &mut Host) {
        let Some(c) = self.running else { return };
        if let Some(x) = h.pop(self.rd) {
            h.push(self.rd, c);
            let how = if self.m == Mutant::YieldAsTick { Entry::Tick } else { Entry::Between };
            self.switch(h, x, how);
        }
    }

    fn block(&mut self, h: &mut Host) -> Next<usize> {
        if self.m != Mutant::BlockKeepsRunning {
            self.running = None;
        }
        match h.pop(self.rd) {
            Some(x) => {
                let how = if self.m == Mutant::BlockAsTick { Entry::Tick } else { Entry::Between };
                self.switch(h, x, how);
                Next::Run(x)
            }
            None => Next::Idle,
        }
    }

    fn tick(&mut self, h: &mut Host) -> Next<usize> {
        let n = h.now;
        if n <= self.seen && self.m != Mutant::SpuriousAsTick {
            return self.status();
        }
        self.advance(n);
        let old = self.rd;
        let new = if self.m == Mutant::CatchUpOneByOne {
            self.table.domain_at(self.seen + 1 - self.epoch.min(self.seen + 1))
        } else {
            self.table.domain_at(n - self.epoch)
        };
        self.seen = n;
        if new == old {
            h.serve(old, n);
            return match self.running {
                Some(c) => match self.slice.on_tick() {
                    Verdict::Keep => Next::Run(c),
                    Verdict::Rotate => match h.pop(old) {
                        Some(x) => {
                            h.push(old, c);
                            let how =
                                if self.m == Mutant::RotateAsBetween { Entry::Between } else { Entry::Tick };
                            self.switch(h, x, how);
                            Next::Run(x)
                        }
                        None => Next::Run(c),
                    },
                },
                None => self.enter(h, old),
            };
        }
        if let Some(c) = self.running.take() {
            let verdict = if self.m == Mutant::NoRuleAtBoundary { Verdict::Keep } else { self.slice.on_tick() };
            match verdict {
                Verdict::Keep => {
                    if self.m == Mutant::BoundaryKeepToBack {
                        h.push(old, c)
                    } else {
                        h.push_front(old, c)
                    }
                }
                Verdict::Rotate => match h.pop(old) {
                    Some(x) => {
                        h.push(old, c);
                        if self.m == Mutant::BoundaryRotateToBack {
                            h.push(old, x)
                        } else {
                            h.push_front(old, x)
                        }
                    }
                    None => h.push_front(old, c),
                },
            }
        }
        self.rd = new;
        match self.m {
            Mutant::NoEnterDomain => h.serve(new, n),
            Mutant::ServeBeforeEnter => {
                h.serve(new, n);
                h.enter_domain(new);
            }
            Mutant::ServeOldInstead => {
                h.enter_domain(new);
                h.serve(old, n);
            }
            _ => {
                h.enter_domain(new);
                h.serve(new, n);
            }
        }
        if self.m == Mutant::ServeOldAtBoundary {
            h.serve(old, n);
        }
        if self.m == Mutant::PopOldDomain { self.enter(h, old) } else { self.enter(h, new) }
    }

    fn poll(&mut self, h: &mut Host) -> Next<usize> {
        let n = h.now;
        if n > self.seen && self.m != Mutant::PollIgnoresTick {
            return self.tick(h);
        }
        h.serve(self.rd, n);
        if self.running.is_some() {
            return self.status();
        }
        match h.pop(self.rd) {
            Some(x) => {
                let how = if self.m == Mutant::PollInputAsTick { Entry::Tick } else { Entry::Between };
                self.switch(h, x, how);
                Next::Run(x)
            }
            None => Next::Idle,
        }
    }

    fn set_table(&mut self, t: Table) {
        if self.m == Mutant::SecondSetIgnored && self.pending.is_some() {
            return;
        }
        let replacing = self.pending.is_some();
        let now = self.seen;
        let at = match self.m {
            Mutant::EffectNextTick => now + 1,
            Mutant::ReplaceStartsNextTick if replacing => now + 1,
            Mutant::StartsInThree => now + 3,
            _ => now + 2,
        };
        self.pending = Some((t, at));
    }

    fn next_tick_switches(&self) -> bool {
        if self.m == Mutant::SwitchesNeverTrue {
            return false;
        }
        if self.m == Mutant::SwitchesIgnoresPending {
            return self.table.domain_at((self.seen + 1).saturating_sub(self.epoch)) != self.rd;
        }
        self.plan_at(self.seen + 1) != self.rd
    }

    fn running_domain(&self) -> Domain {
        self.rd
    }

    fn running(&self, _: &Host) -> Option<usize> {
        self.running
    }

    fn partitioned(&self) -> bool {
        !self.table.is_default() || self.pending.is_some_and(|(t, _)| !t.is_default())
    }
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// 한 번의 시뮬레이션이 받는 입력입니다.
#[derive(Clone, Debug)]
struct World {
    specs: Vec<Spec>,
    changes: Vec<(u64, Table)>,
    jumps: Vec<(u64, u64)>,
    horizon: u64,
}

/// 세계를 `sc`로 돌리고 Host를 돌려주는 함수입니다.
///
/// 틱 번호 t에서 틱 사건을 처리하고 표 변경을 적용한 뒤 칸마다 한 연산씩
/// 돕니다. `jumps`의 (at, extra)는 틱 at 다음에 extra개의 틱이 사건 없이
/// 건너뛰어지는 것(커널이 오래 멈춘 경우)입니다.
fn simulate(w: &World, sc: &mut dyn Sched) -> Host {
    let mut h = Host::new(&w.specs);
    for t in 1..w.specs.len() {
        let d = h.dom(t);
        h.push(d, t);
    }
    ev_start(&mut h, sc, 0);
    let mut changes = w.changes.iter().peekable();
    while let Some(&&(at, tbl)) = changes.peek() {
        if at > 0 {
            break;
        }
        ev_set(&mut h, sc, tbl);
        changes.next();
    }
    let mut arrivals: Vec<(u64, u8, usize)> = Vec::new();
    for (t, task) in w.specs.iter().enumerate() {
        for &(k, s) in &task.inputs {
            arrivals.push((k, s, t));
        }
    }
    arrivals.sort();
    let mut next_arrival = 0;
    let mut t = 1;
    while t <= w.horizon && h.bad.is_none() {
        h.now = t;
        h.step = 0;
        let r = if h.current.is_some() { ev_tick(&mut h, sc) } else { ev_poll(&mut h, sc) };
        h.check_next(r);
        let want = h.plan.domain_at(t);
        if sc.running_domain() != want {
            h.fail(format!("running_domain {} but the plan says {}", sc.running_domain().id(), want.id()));
        }
        // 비교값보다 이른 인터럽트(Spurious)는 커널이 사건으로 올리지 않지만, 같은 번호로
        // 다시 불려도 Core는 아무 일도 하지 않아야 한다
        if sc.idempotent_tick() && t % 2 == 0 && h.bad.is_none() {
            let again = ev_tick(&mut h, sc);
            h.check_next(again);
        }
        while let Some(&&(at, tbl)) = changes.peek() {
            if at > t {
                break;
            }
            ev_set(&mut h, sc, tbl);
            changes.next();
        }
        if sc.next_tick_switches() != (h.plan.domain_at(t + 1) != want) {
            h.fail("next_tick_switches disagrees with the plan".into());
        }
        for step in 0..STEPS {
            h.step = step;
            while next_arrival < arrivals.len() && (arrivals[next_arrival].0, arrivals[next_arrival].1) <= (t, step)
            {
                let task = arrivals[next_arrival].2;
                next_arrival += 1;
                h.tasks[task].input += 1;
                // 입력 인터럽트: 도는 도메인의 읽는 태스크만 바로 깨움
                if h.tasks[task].st == St::Reading && h.dom(task) == sc.running_domain() {
                    h.tasks[task].input -= 1;
                    h.wake(task, Why::Input);
                }
                if h.current.is_none() {
                    let r = ev_poll(&mut h, sc);
                    h.check_next(r);
                }
            }
            match h.current {
                Some(c) => exec(&mut h, sc, c),
                None => {
                    let d = h.plan.domain_at(t);
                    if !h.q[d.index()].is_empty() {
                        h.fail(format!("idle although domain {} has a ready task", d.id()));
                    }
                }
            }
            if h.bad.is_some() {
                break;
            }
        }
        t += 1 + w.jumps.iter().find(|j| j.0 == t).map_or(0, |j| j.1);
    }
    h
}

fn exec(h: &mut Host, sc: &mut dyn Sched, t: usize) {
    let want = h.plan.domain_at(h.now);
    if h.dom(t) != want {
        h.fail(format!("task {t} of domain {} executes in a window of domain {}", h.dom(t).id(), want.id()));
        return;
    }
    let len = h.tasks[t].spec.prog.len();
    let op = h.tasks[t].spec.prog[h.tasks[t].pc];
    h.tasks[t].pc = (h.tasks[t].pc + 1) % len;
    h.record(t, Ev::Ran { tick: h.now, step: h.step, task: t });
    let leave = |h: &mut Host, sc: &mut dyn Sched, st: St| {
        h.tasks[t].st = st;
        h.current = None;
        let r = ev_block(h, sc);
        h.check_next(r);
    };
    match op {
        Op::Spin => {}
        Op::Yield => ev_yield(h, sc),
        Op::Wait(n) => leave(h, sc, St::Timed(h.now + n)),
        Op::Pend(o) => {
            if h.sems[o] > 0 {
                h.sems[o] -= 1;
            } else {
                h.waiters[o].push_back(t);
                leave(h, sc, St::Pending(o));
            }
        }
        Op::Post(o) => match h.waiters[o].pop_front() {
            Some(w) => h.wake(w, Why::Post),
            None => h.sems[o] += 1,
        },
        Op::Read => {
            if h.tasks[t].input > 0 {
                h.tasks[t].input -= 1;
            } else {
                leave(h, sc, St::Reading);
            }
        }
        Op::Exit => leave(h, sc, St::Dead),
    }
}

fn trace_of(h: &Host, d: u8) -> Vec<Ev> {
    h.trace.iter().filter(|(x, _)| x.id() == d).map(|(_, e)| *e).collect()
}

fn rand_op(r: &mut Rng, dom: u8) -> Op {
    let obj = dom as usize * 2 + r.below(2) as usize;
    match r.below(18) {
        0..=2 => Op::Spin,
        3..=6 => Op::Yield,
        7..=9 => Op::Wait(1 + r.below(3)),
        10..=11 => Op::Read,
        12 => Op::Exit,
        13 => Op::Pend(obj),
        _ => Op::Post(obj),
    }
}

fn rand_spec(r: &mut Rng, dom: u8, horizon: u64) -> Spec {
    let prog: Vec<Op> = (0..1 + r.below(4)).map(|_| rand_op(r, dom)).collect();
    let mut inputs = Vec::new();
    if prog.contains(&Op::Read) {
        for _ in 0..r.below(5) {
            inputs.push((1 + r.below(horizon), r.below(STEPS as u64) as u8));
        }
    }
    Spec { domain: dom, prog, inputs }
}

fn rand_table(r: &mut Rng, ndom: u64, must: u8) -> Table {
    let n = 1 + r.below(5) as usize;
    let mut e: Vec<(Domain, u32)> = (0..n)
        .map(|_| (Domain::new(r.below(ndom) as u8).unwrap(), 1 + r.below(3) as u32))
        .collect();
    if !e.iter().any(|x| x.0.id() == must) {
        let i = r.below(n as u64) as usize;
        e[i].0 = Domain::new(must).unwrap();
    }
    Table::from_entries(&e).unwrap()
}

fn rand_changes(r: &mut Rng, ndom: u64, must: u8, horizon: u64) -> Vec<(u64, Table)> {
    let mut v: Vec<(u64, Table)> = (0..r.below(4))
        .map(|_| (r.below(horizon.saturating_sub(3)), rand_table(r, ndom, must)))
        .collect();
    v.sort_by_key(|x| x.0);
    v
}

fn rand_jumps(r: &mut Rng, horizon: u64) -> Vec<(u64, u64)> {
    (0..r.below(3)).map(|_| (1 + r.below(horizon), 1 + r.below(4))).collect()
}

/// 모든 도메인이 무작위인 세계입니다. 0번 태스크는 도메인 0입니다.
fn random_world(seed: u64, horizon: u64) -> World {
    let mut r = Rng::new(seed);
    let ndom = 1 + r.below(8);
    let n = 2 + r.below(6);
    let mut specs = vec![rand_spec(&mut r, 0, horizon)];
    for _ in 1..n {
        let d = r.below(ndom) as u8;
        specs.push(rand_spec(&mut r, d, horizon));
    }
    let changes = rand_changes(&mut r, ndom, 0, horizon);
    let jumps = if r.below(2) == 0 { rand_jumps(&mut r, horizon) } else { Vec::new() };
    World { specs, changes, jumps, horizon }
}

/// 같은 표와 같은 시간 흐름을 쓰고 도메인 `d`의 태스크는 같으며 나머지만 다른
/// 세계 한 쌍입니다. `shared`가 true면 다른 도메인의 태스크가 `d`의 오브젝트에
/// 신호를 보냄(명시적 공유 채널)으로써 흔적이 갈리는 경우를 만듭니다.
fn two_worlds(seed: u64, d: u8, horizon: u64) -> (World, World) {
    let mut r = Rng::new(seed);
    let ndom = 3;
    let mine: Vec<Spec> = (0..1 + r.below(3)).map(|_| rand_spec(&mut r, d, horizon)).collect();
    let changes = rand_changes(&mut r, ndom, d, horizon);
    let jumps = if r.below(2) == 0 { rand_jumps(&mut r, horizon) } else { Vec::new() };
    let build = |s: u64| {
        let mut o = Rng::new(s ^ 0xABCD_1234);
        let mut specs = Vec::new();
        if d != 0 {
            specs.push(rand_spec(&mut o, 0, horizon));
        }
        specs.extend(mine.iter().cloned());
        for _ in 0..o.below(5) {
            let mut x = o.below(ndom) as u8;
            if x == d {
                x = (x + 1) % ndom as u8;
            }
            specs.push(rand_spec(&mut o, x, horizon));
        }
        World { specs, changes: changes.clone(), jumps: jumps.clone(), horizon }
    };
    (build(seed.wrapping_add(1)), build(seed.wrapping_add(2)))
}

type Mk<'a> = &'a dyn Fn() -> Box<dyn Sched>;

fn real() -> Box<dyn Sched> {
    Box::new(Core::<usize>::new())
}

fn mutant(m: Mutant) -> Box<dyn Sched> {
    Box::new(MutCore::new(m))
}

fn check(h: &Host, what: &str) -> Result<(), String> {
    match &h.bad {
        Some(b) => Err(format!("{what}: {b}")),
        None => Ok(()),
    }
}

/// 시험 규모입니다. 깨뜨리기 시험은 같은 속성을 더 작게 돌립니다.
#[derive(Clone, Copy)]
struct Scale {
    random: u64,
    exhaustive: bool,
}

const FULL: Scale = Scale { random: 4000, exhaustive: true };
const QUICK: Scale = Scale { random: 400, exhaustive: false };

/// 모든 표(항목 1~2개, 도메인 0~1, 창 1~2틱)입니다.
fn small_tables() -> Vec<Table> {
    let one: Vec<(Domain, u32)> = (0..2)
        .flat_map(|d| (1..=2).map(move |w| (Domain::new(d).unwrap(), w)))
        .collect();
    let mut v = Vec::new();
    for a in &one {
        v.push(Table::from_entries(&[*a]).unwrap());
        for b in &one {
            v.push(Table::from_entries(&[*a, *b]).unwrap());
        }
    }
    v
}

/// 길이 2 이하의 모든 프로그램입니다.
fn small_progs() -> Vec<Vec<Op>> {
    let ops = [Op::Spin, Op::Yield, Op::Wait(1), Op::Exit];
    let mut v: Vec<Vec<Op>> = ops.iter().map(|&a| vec![a]).collect();
    for &a in &ops {
        for &b in &ops {
            v.push(vec![a, b]);
        }
    }
    v
}

fn spec(domain: u8, prog: Vec<Op>) -> Spec {
    Spec { domain, prog, inputs: Vec::new() }
}

/// 속성 2: 창이 정확하고 남는 시간을 양도하지 않습니다.
fn p_window_exact(mk: Mk, sc: Scale) -> Result<(), String> {
    for seed in 0..sc.random {
        let w = random_world(seed, 40);
        let h = simulate(&w, &mut *mk());
        check(&h, &format!("random world {seed}"))?;
    }
    if sc.exhaustive {
        let progs = small_progs();
        for tbl in small_tables() {
            for a in &progs {
                for b in &progs {
                    let w = World {
                        specs: vec![spec(0, a.clone()), spec(0, vec![Op::Spin]), spec(1, b.clone())],
                        changes: vec![(0, tbl)],
                        jumps: Vec::new(),
                        horizon: 10,
                    };
                    let h = simulate(&w, &mut *mk());
                    check(&h, &format!("exhaustive {:?} {:?} {:?}", tbl, a, b))?;
                }
            }
        }
    }
    Ok(())
}

/// 속성 1: 한 도메인의 흔적은 다른 도메인이 무엇을 하든 같습니다. 두 세계의
/// 다른 도메인 흔적이 실제로 갈렸는지(속성이 비어 있지 않은지)도 세어 돌려줍니다.
fn p_two_run(mk: Mk, sc: Scale) -> Result<u64, String> {
    let mut differing = 0;
    for seed in 0..sc.random {
        let d = (seed % 3) as u8;
        let (a, b) = two_worlds(seed, d, 40);
        let (ha, hb) = (simulate(&a, &mut *mk()), simulate(&b, &mut *mk()));
        check(&ha, &format!("world A seed {seed}"))?;
        check(&hb, &format!("world B seed {seed}"))?;
        if trace_of(&ha, d) != trace_of(&hb, d) {
            return Err(format!("domain {d} trace differs, seed {seed}"));
        }
        if (0..3).filter(|&x| x != d).any(|x| trace_of(&ha, x) != trace_of(&hb, x)) {
            differing += 1;
        }
    }
    if sc.exhaustive {
        let progs = small_progs();
        for tbl in small_tables() {
            if !tbl.contains(Domain::new(1).unwrap()) {
                continue;
            }
            for mine in &progs {
                let mut base: Option<Vec<Ev>> = None;
                for a in &progs {
                    for b in &progs {
                        let w = World {
                            specs: vec![spec(0, a.clone()), spec(1, mine.clone()), spec(0, b.clone())],
                            changes: vec![(0, tbl)],
                            jumps: Vec::new(),
                            horizon: 10,
                        };
                        let h = simulate(&w, &mut *mk());
                        check(&h, &format!("exhaustive two-run {:?} {:?}", tbl, mine))?;
                        let tr = trace_of(&h, 1);
                        match &base {
                            None => base = Some(tr),
                            Some(x) if *x != tr => {
                                return Err(format!("exhaustive: domain 1 trace differs, {:?} {:?} {:?} {:?}", tbl, mine, a, b));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    Ok(differing)
}

/// 속성 3: 기본 표(와 루트만 든 표)에서 Core의 결정은 얼린 옛 스케줄러와 같습니다.
fn p_default_equal(mk: Mk, sc: Scale) -> Result<(), String> {
    let all_root = |v: &[(u32,)]| {
        Table::from_entries(&v.iter().map(|x| (Domain::ROOT, x.0)).collect::<Vec<_>>()).unwrap()
    };
    let tables = [Table::DEFAULT, all_root(&[(3,)]), all_root(&[(1,), (2,)])];
    let run = |w: &World| -> Result<(), String> {
        let (mut a, mut b) = (mk(), Legacy::new());
        let (ha, hb) = (simulate(w, &mut *a), simulate(w, &mut b));
        check(&ha, "core")?;
        check(&hb, "legacy")?;
        if ha.runs != hb.runs || ha.trace != hb.trace || ha.calls != hb.calls {
            return Err(format!("decisions differ from the frozen scheduler: {:?}", w.specs));
        }
        Ok(())
    };
    for seed in 0..sc.random {
        let mut w = random_world(seed ^ 0x5555, 40);
        for s in &mut w.specs {
            s.domain = 0;
        }
        let mut r = Rng::new(seed);
        w.changes = (0..r.below(3))
            .map(|_| (r.below(30), tables[r.below(3) as usize]))
            .collect();
        w.changes.sort_by_key(|x| x.0);
        run(&w)?;
    }
    if sc.exhaustive {
        let progs = small_progs();
        for a in &progs {
            for b in &progs {
                for c in &progs {
                    let w = World {
                        specs: vec![spec(0, a.clone()), spec(0, b.clone()), spec(0, c.clone())],
                        changes: Vec::new(),
                        jumps: Vec::new(),
                        horizon: 10,
                    };
                    run(&w)?;
                }
            }
        }
    }
    Ok(())
}

/// 속성 4: 도메인 안에서 계속 도는 태스크의 몫은 도메인의 창 시간을 기준으로
/// 1/N 아래로 한계 이상 내려가지 않습니다.
///
/// 한계는 k0-slice의 한계 "(N-1)주기 + 한 주기"를 틱 단위로 옮긴 N틱입니다.
/// 도메인 안의 흐름은 창에 속한 틱만 이어 붙인 시간 위에서 k0-slice와 같다고
/// 보고 한계를 그대로 가져왔으며, 증명이 아니라 시험의 기준입니다.
fn share_lag(n: u64) -> u64 {
    n * STEPS as u64
}

fn share_world(seed: u64) -> (World, usize, u8, u64) {
    let mut r = Rng::new(seed);
    let d = r.below(2) as u8;
    let n = 2 + r.below(3);
    let mut specs: Vec<Spec> = Vec::new();
    if d == 1 {
        specs.push(rand_spec(&mut r, 0, 40));
    }
    let first = specs.len();
    let spinner = first + r.below(n) as usize;
    for i in 0..n as usize {
        let mut s = rand_spec(&mut r, d, 40);
        if first + i == spinner {
            s.prog = vec![Op::Spin];
            s.inputs.clear();
        }
        specs.push(s);
    }
    for _ in 0..r.below(3) {
        specs.push(rand_spec(&mut r, 1 - d, 40));
    }
    let changes = vec![(r.below(4), rand_table(&mut r, 2, d))];
    let jumps = if r.below(3) == 0 { rand_jumps(&mut r, 40) } else { Vec::new() };
    (World { specs, changes, jumps, horizon: 60 }, spinner, d, n)
}

/// `share_world`를 돌리며 매 칸마다 몫을 보고 가장 크게 뒤처진 값을 돌려줍니다.
fn worst_lag(mk: Mk, w: &World, spinner: usize, d: u8, n: u64) -> Result<i64, String> {
    let mut sc = mk();
    // 칸마다 다시 돌리지 않고 흔적에서 계산
    let h = simulate(w, &mut *sc);
    check(&h, "share")?;
    let mut window_steps = 0u64;
    let mut mine = 0u64;
    let mut worst = i64::MIN;
    let mut ran: std::collections::BTreeMap<(u64, u8), usize> = std::collections::BTreeMap::new();
    for (_, ev) in &h.trace {
        if let Ev::Ran { tick, step, task } = ev {
            ran.insert((*tick, *step), *task);
        }
    }
    let plan = &h.plan;
    let mut t = 1;
    while t <= w.horizon {
        if plan.domain_at(t).id() == d {
            for s in 0..STEPS {
                window_steps += 1;
                if ran.get(&(t, s)) == Some(&spinner) {
                    mine += 1;
                }
                worst = worst.max(window_steps as i64 - (mine * n) as i64);
            }
        }
        t += 1 + w.jumps.iter().find(|j| j.0 == t).map_or(0, |j| j.1);
    }
    Ok(worst)
}

fn p_share(mk: Mk, sc: Scale) -> Result<(), String> {
    for seed in 0..sc.random {
        let (w, spinner, d, n) = share_world(seed);
        let worst = worst_lag(mk, &w, spinner, d, n)?;
        if worst > share_lag(n) as i64 {
            return Err(format!("share bound broken, seed {seed}: lag {worst} > {}", share_lag(n)));
        }
    }
    if sc.exhaustive {
        let progs = small_progs();
        for tbl in small_tables() {
            if !tbl.contains(Domain::ROOT) {
                continue;
            }
            for a in &progs {
                for b in &progs {
                    let w = World {
                        specs: vec![spec(0, vec![Op::Spin]), spec(0, a.clone()), spec(0, b.clone()), spec(1, vec![Op::Spin])],
                        changes: vec![(0, tbl)],
                        jumps: Vec::new(),
                        horizon: 14,
                    };
                    let worst = worst_lag(mk, &w, 0, 0, 3)?;
                    if worst > share_lag(3) as i64 {
                        return Err(format!("exhaustive share bound broken: {:?} {:?} {:?} lag {worst}", tbl, a, b));
                    }
                }
            }
        }
    }
    Ok(())
}

/// 틱마다 어느 태스크가 도는지 모은 것입니다(모든 도메인).
fn ran_by_tick(h: &Host) -> Vec<(u64, Vec<usize>)> {
    let mut v: Vec<(u64, Vec<usize>)> = Vec::new();
    for (_, ev) in &h.trace {
        if let Ev::Ran { tick, task, .. } = ev {
            match v.last_mut() {
                Some((t, l)) if t == tick => l.push(*task),
                _ => v.push((*tick, vec![*task])),
            }
        }
    }
    v
}

fn scenario(specs: Vec<Spec>, table: Option<Table>, horizon: u64, sc: &mut dyn Sched) -> Host {
    let w = World {
        specs,
        changes: table.map(|t| vec![(0, t)]).unwrap_or_default(),
        jumps: Vec::new(),
        horizon,
    };
    simulate(&w, sc)
}

fn tbl(e: &[(u8, u32)]) -> Table {
    Table::from_entries(&e.iter().map(|&(d, w)| (Domain::new(d).unwrap(), w)).collect::<Vec<_>>()).unwrap()
}

/// 속성 6: 경계에서의 틱 규칙입니다.
fn p_tick_rule(mk: Mk) -> Result<(), String> {
    let flat = |v: &[(u64, &[usize])]| -> Vec<(u64, Vec<usize>)> { v.iter().map(|&(t, l)| (t, l.to_vec())).collect() };
    let same = |got: Vec<(u64, Vec<usize>)>, want: Vec<(u64, Vec<usize>)>, what: &str| {
        if got == want { Ok(()) } else { Err(format!("{what}: got {got:?}, want {want:?}")) }
    };
    // 경계에서 세워 둔 태스크는 자기 도메인의 다음 창에서 맨 앞이고, 한 틱짜리
    // 창은 스피너 둘 사이를 돌린다. 표는 틱 2부터 [(0,1),(1,1)]
    let two = vec![spec(0, vec![Op::Spin]), spec(0, vec![Op::Spin]), spec(1, vec![Op::Spin])];
    let h = scenario(two, Some(tbl(&[(0, 1), (1, 1)])), 9, &mut *mk());
    check(&h, "rotation")?;
    same(
        ran_by_tick(&h),
        flat(&[
            (1, &[0, 0, 0]),
            (2, &[1, 1, 1]),
            (3, &[2, 2, 2]),
            (4, &[0, 0, 0]),
            (5, &[2, 2, 2]),
            (6, &[1, 1, 1]),
            (7, &[2, 2, 2]),
            (8, &[0, 0, 0]),
            (9, &[2, 2, 2]),
        ]),
        "one-tick windows rotate between two spinners",
    )?;
    // 틱과 틱 사이에 들어온 태스크는 경계의 틱에서 끊기지 않고 다음 창에서도
    // 맨 앞이다. 0번이 양보해서 1번이 틱 1 직후가 아니라 칸 0에 들어옴
    let between = vec![spec(0, vec![Op::Yield, Op::Spin]), spec(0, vec![Op::Spin]), spec(1, vec![Op::Spin])];
    let h = scenario(between, Some(tbl(&[(1, 1), (0, 1)])), 6, &mut *mk());
    check(&h, "between")?;
    same(
        ran_by_tick(&h),
        flat(&[
            (1, &[0, 1, 1]),
            (2, &[2, 2, 2]),
            (3, &[1, 1, 1]),
            (4, &[2, 2, 2]),
            (5, &[0, 0, 1]),
            (6, &[2, 2, 2]),
        ]),
        "a task that entered between ticks is not cut at the boundary",
    )?;
    // 기본 표: 양보로 들어온 태스크는 처음 맞는 틱을 넘기고 그다음 틱에서 끊긴다
    let dflt = vec![spec(0, vec![Op::Yield, Op::Spin]), spec(0, vec![Op::Spin])];
    let h = scenario(dflt, None, 4, &mut *mk());
    check(&h, "default")?;
    same(
        ran_by_tick(&h),
        flat(&[(1, &[0, 1, 1]), (2, &[1, 1, 1]), (3, &[0, 0, 1]), (4, &[1, 1, 1])]),
        "default table keeps the first tick of a task that entered between ticks",
    )?;
    // 한 틱짜리 기본 표에서 스피너 둘은 틱마다 번갈아 돈다(틱 1은 시작 면제)
    let spin2 = vec![spec(0, vec![Op::Spin]), spec(0, vec![Op::Spin])];
    let h = scenario(spin2, None, 6, &mut *mk());
    check(&h, "spin2")?;
    same(
        ran_by_tick(&h),
        flat(&[
            (1, &[0, 0, 0]),
            (2, &[1, 1, 1]),
            (3, &[0, 0, 0]),
            (4, &[1, 1, 1]),
            (5, &[0, 0, 0]),
            (6, &[1, 1, 1]),
        ]),
        "default table alternates two spinners",
    )?;
    // 비어 있는 도메인으로 경계가 오면 CPU는 놀고 남는 시간은 양도되지 않는다
    let donate = vec![spec(0, vec![Op::Spin]), spec(1, vec![Op::Wait(1), Op::Spin])];
    let h = scenario(donate, Some(tbl(&[(1, 1), (0, 1)])), 6, &mut *mk());
    check(&h, "idle window")?;
    Ok(())
}

/// 속성 5: 표를 틱 n에 정하면 n + 2의 0번 항목부터 효력이 생기고, 새 설정이 앞의
/// 보류 표를 밀어내며, 틱이 건너뛰어져도 맞는 항목에 닿고, 도메인 변경 예고가 맞습니다.
fn p_table(mk: Mk, sc: Scale) -> Result<(), String> {
    let dom = |sc: &dyn Sched| sc.running_domain().id();
    let mut h = Host::new(&[]);
    let mut s = mk();
    // 틱 5에 [(1,2),(0,1)]을 정함: 틱 6은 그대로, 7부터 1,1,0,1,1,0
    for t in 1..=5 {
        h.now = t;
        s.tick(&mut h);
    }
    s.set_table(tbl(&[(1, 2), (0, 1)]));
    let mut got = Vec::new();
    for t in 6..=12 {
        h.now = t;
        s.tick(&mut h);
        got.push(dom(&*s));
    }
    if got != [0, 1, 1, 0, 1, 1, 0] {
        return Err(format!("table set at 5: domains of ticks 6..=12 are {got:?}"));
    }
    // 보류 중에 다시 정하면 앞의 표는 효력이 생기지 않는다
    let mut h = Host::new(&[]);
    let mut s = mk();
    for t in 1..=5 {
        h.now = t;
        s.tick(&mut h);
    }
    s.set_table(tbl(&[(1, 1)]));
    h.now = 6;
    s.tick(&mut h);
    s.set_table(tbl(&[(2, 1)]));
    let mut got = Vec::new();
    for t in 7..=9 {
        h.now = t;
        s.tick(&mut h);
        got.push(dom(&*s));
    }
    if got != [0, 2, 2] {
        return Err(format!("second set must replace the first: {got:?}"));
    }
    // 효력이 생긴 시점(틱 7)에 다시 정하면 앞의 표는 그 시점부터 자리를 지킨다
    let mut h = Host::new(&[]);
    let mut s = mk();
    for t in 1..=5 {
        h.now = t;
        s.tick(&mut h);
    }
    s.set_table(tbl(&[(1, 1)]));
    let mut got = Vec::new();
    for t in 6..=9 {
        h.now = t;
        s.tick(&mut h);
        if t == 7 {
            s.set_table(tbl(&[(2, 1)]));
        }
        got.push(dom(&*s));
    }
    if got != [0, 1, 1, 2] {
        return Err(format!("a set at the tick the first one starts: {got:?}"));
    }
    // 건너뜀: [(0,2),(1,3),(2,1)]은 틱 2부터 한 바퀴 6틱. 3에서 8로 뛰면 0번 항목
    let mut h = Host::new(&[]);
    let mut s = mk();
    s.set_table(tbl(&[(0, 2), (1, 3), (2, 1)]));
    for (t, want) in [(1u64, 0u8), (3, 0), (8, 0), (11, 1), (13, 2), (14, 0), (20, 0), (22, 1)] {
        h.now = t;
        s.tick(&mut h);
        if dom(&*s) != want {
            return Err(format!("jump to tick {t}: domain {} want {want}", dom(&*s)));
        }
    }
    // 무작위: 설정, 건너뜀, 예고를 기준(Plan)과 견줌
    for seed in 0..sc.random {
        let mut r = Rng::new(seed ^ 0x77);
        let mut h = Host::new(&[]);
        let mut s = mk();
        let mut t = 0;
        for _ in 0..30 {
            t += 1 + if r.below(5) == 0 { r.below(6) } else { 0 };
            h.now = t;
            s.tick(&mut h);
            let want = h.plan.domain_at(t);
            if s.running_domain() != want {
                return Err(format!("seed {seed} tick {t}: domain {} want {}", s.running_domain().id(), want.id()));
            }
            if r.below(3) == 0 {
                let tb = rand_table(&mut r, 4, 0);
                s.set_table(tb);
                h.plan.set(t, tb);
            }
            if s.next_tick_switches() != (h.plan.domain_at(t + 1) != want) {
                return Err(format!("seed {seed} tick {t}: next_tick_switches is wrong"));
            }
        }
    }
    Ok(())
}

fn replace_specs() -> Vec<Spec> {
    vec![
        spec(0, vec![Op::Spin]),
        spec(0, vec![Op::Yield, Op::Spin]),
        spec(1, vec![Op::Spin]),
        spec(1, vec![Op::Wait(1), Op::Spin]),
    ]
}

/// 태스크가 든 Host를 만들고 0번을 시작시킵니다. 나머지는 자기 도메인 큐에 순서대로 섭니다.
fn script_host(specs: Vec<Spec>, sc: &mut dyn Sched) -> Host {
    let mut h = Host::new(&specs);
    for t in 1..specs.len() {
        let d = h.dom(t);
        h.push(d, t);
    }
    h.calls.clear();
    ev_start(&mut h, sc, 0);
    h
}

/// 틱 `t`가 오고 커널이 그것을 처리하는 한 걸음입니다(도는 태스크가 없으면 poll).
fn step_to(h: &mut Host, sc: &mut dyn Sched, t: u64) {
    h.now = t;
    let r = if h.current.is_some() { ev_tick(h, sc) } else { ev_poll(h, sc) };
    h.check_next(r);
}

/// 속성 7: 보류 중인 표를 갈아 끼웁니다. 효력이 생기기 전에 다시 정하면 앞의 표는
/// 버려지고 새 표가 정한 틱 + 2에서 시작하며, 앞 표의 시작 틱이 지난 뒤에 다시 정하면
/// 앞 표는 그 틱을 처리할 때 이미 올라가 있습니다. 사건마다 `next_tick_switches`를
/// 기준 Plan과 견주는 일은 [finish]가 합니다.
fn p_replace(mk: Mk, sc: Scale) -> Result<(), String> {
    let all = small_tables();
    let tables: Vec<Table> = all.iter().copied().step_by(if sc.exhaustive { 1 } else { 3 }).collect();
    for &a in &tables {
        for &b in &tables {
            for s1 in 0..4u64 {
                for gap in 0..=3u64 {
                    let w = World {
                        specs: replace_specs(),
                        changes: vec![(s1, a), (s1 + gap, b)],
                        jumps: Vec::new(),
                        horizon: 14,
                    };
                    let h = simulate(&w, &mut *mk());
                    check(&h, &format!("replace {:?} at {s1} by {:?} at {}", a, b, s1 + gap))?;
                }
            }
        }
    }
    // 앞 표의 시작 틱이 지난 뒤에 다시 정하는 경우. 틱 처리가 멈춰 있었다면 그
    // 틱을 먼저 전해야 시스템 콜이 돌 수 있으므로, 틱을 하나씩 전하는 쪽과 한
    // 번에 건너뛰어 전하는 쪽을 둘 다 본다. 앞 표는 전해지는 사이에 올라가고
    // 새 표는 그 뒤의 틱 + 2에서 시작한다
    for &a in &tables {
        for &b in &tables {
            for s1 in 0..3u64 {
                for stall in 0..=3u64 {
                    for jump in [false, true] {
                        let mut s = mk();
                        let mut h = script_host(replace_specs(), &mut *s);
                        for t in 1..=s1 {
                            step_to(&mut h, &mut *s, t);
                        }
                        ev_set(&mut h, &mut *s, a);
                        let now = s1 + 2 + stall;
                        if jump {
                            step_to(&mut h, &mut *s, now);
                        } else {
                            for t in s1 + 1..=now {
                                step_to(&mut h, &mut *s, t);
                            }
                        }
                        ev_set(&mut h, &mut *s, b);
                        for t in now + 1..=now + 12 {
                            step_to(&mut h, &mut *s, t);
                        }
                        check(&h, &format!("late set {:?} at {s1}, {:?} at {now}, jump {jump}", a, b))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// 틱을 전하고 표를 정하는 사건의 열에서 한 걸음입니다.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// 지금까지 처리한 틱에서 이만큼 뒤의 틱을 전함(1이면 다음 틱, 더 크면 건너뜀)
    Tick(u64),
    Set(Table),
    /// 같은 번호의 틱 사건을 되풀이함(이른 인터럽트 뒤의 같은 번호)
    Again,
}

fn random_script(r: &mut Rng, len: u64) -> Vec<Step> {
    (0..len)
        .map(|_| match r.below(10) {
            0..=3 => Step::Tick(1),
            4 => Step::Tick(2 + r.below(4)),
            5 => Step::Again,
            _ => Step::Set(rand_table(r, 4, 0)),
        })
        .collect()
}

fn guard_specs() -> Vec<Spec> {
    let mut v = replace_specs();
    v.push(spec(2, vec![Op::Spin]));
    v
}

/// 열을 처음부터 돌려 스케줄러와 Host를 돌려주는 함수입니다. `extra`가 있으면 열의
/// 끝에서 그 표를 한 번 더 정합니다.
fn replay(mk: Mk, script: &[Step], extra: Option<Table>) -> (Box<dyn Sched>, Host) {
    let mut s = mk();
    let mut h = script_host(guard_specs(), &mut *s);
    for st in script {
        match *st {
            Step::Tick(d) => {
                let t = h.now + d;
                step_to(&mut h, &mut *s, t);
            }
            Step::Set(t) => ev_set(&mut h, &mut *s, t),
            Step::Again => {
                let r = ev_tick(&mut h, &mut *s);
                h.check_next(r);
            }
        }
    }
    if let Some(t) = extra {
        ev_set(&mut h, &mut *s, t);
    }
    (s, h)
}

/// 속성 11: 틱, 건너뜀, 되풀이, 표 설정이 섞인 어떤 열에서도 `next_tick_switches()`는
/// 사건마다(표를 정한 바로 뒤 포함) 기준 Plan의 틱 seen + 1의 답과 같습니다. 기준은
/// Core와 따로 적은 [Plan]이고 표를 정하는 것은 처리한 틱 seen에서뿐입니다.
fn p_guard_answer(mk: Mk, sc: Scale) -> Result<u64, String> {
    let mut checked = 0;
    for seed in 0..sc.random {
        let mut r = Rng::new(seed ^ 0x6A6A);
        let len = 2 + r.below(14);
        let script = random_script(&mut r, len);
        let mut s = mk();
        let mut h = script_host(guard_specs(), &mut *s);
        for (i, st) in script.iter().enumerate() {
            match *st {
                Step::Tick(d) => {
                    let t = h.now + d;
                    step_to(&mut h, &mut *s, t);
                }
                Step::Set(t) => ev_set(&mut h, &mut *s, t),
                Step::Again => {
                    let r = ev_tick(&mut h, &mut *s);
                    h.check_next(r);
                }
            }
            check(&h, &format!("guard world {seed} step {i}"))?;
            let want = h.plan.domain_at(h.seen + 1) != s.running_domain();
            if s.next_tick_switches() != want {
                return Err(format!(
                    "seed {seed} after step {i} {st:?} (seen {}): next_tick_switches {} but the plan says {want}: {script:?}",
                    h.seen,
                    s.next_tick_switches()
                ));
            }
            checked += 1;
        }
    }
    Ok(checked)
}

/// 속성 12: 처리한 틱 seen에서 표를 정해도 틱 seen + 1의 도메인은 바뀌지 않습니다. 다만
/// seen + 1에서 시작하기로 한 보류 표를 갈아 끼울 때는 예외이고, 그때는 도메인이 바뀌는
/// 만큼 `next_tick_switches()`가 바로 달라집니다(Lean의 `guard_sound`와 그 가정이 빠진
/// 반례). 같은 열을 표를 더 정한 쪽과 안 정한 쪽으로 두 번 돌려 실제 Core의 도메인을
/// 견주고, 기준 Plan의 답과도 견줍니다. 센 값은 (예외가 아닌 경우, 예외인 경우, 예외에서
/// 도메인이 실제로 달라진 경우, 예외에서 예고가 달라진 경우)입니다.
fn p_guard_sound(mk: Mk, sc: Scale) -> Result<(u64, u64, u64, u64), String> {
    let (mut plain, mut replacing, mut domain_changed, mut guard_changed) = (0, 0, 0, 0);
    for seed in 0..sc.random {
        let mut r = Rng::new(seed ^ 0x5151);
        let len = 1 + r.below(10);
        let script = random_script(&mut r, len);
        let table = rand_table(&mut r, 4, 0);
        let (mut a, mut ha) = replay(mk, &script, Some(table));
        let (mut b, mut hb) = replay(mk, &script, None);
        check(&ha, &format!("guard sound A {seed}"))?;
        check(&hb, &format!("guard sound B {seed}"))?;
        let seen = hb.seen;
        if ha.seen != seen {
            return Err(format!("seed {seed}: the set_table changed the processed tick"));
        }
        let starts_next = hb.plan.segs.iter().any(|(st, _)| *st == seen + 1);
        let (before, after) = (b.next_tick_switches(), a.next_tick_switches());
        let rd = b.running_domain();
        if a.running_domain() != rd {
            return Err(format!("seed {seed}: set_table changed the running domain"));
        }
        let (want_b, want_a) = (hb.plan.domain_at(seen + 1), ha.plan.domain_at(seen + 1));
        if before != (want_b != rd) || after != (want_a != rd) {
            return Err(format!("seed {seed}: next_tick_switches {before}/{after} vs the plan {want_b:?}/{want_a:?}"));
        }
        step_to(&mut ha, &mut *a, seen + 1);
        step_to(&mut hb, &mut *b, seen + 1);
        check(&ha, &format!("guard sound A tick {seed}"))?;
        check(&hb, &format!("guard sound B tick {seed}"))?;
        let (da, db) = (a.running_domain(), b.running_domain());
        if da != want_a || db != want_b {
            return Err(format!("seed {seed}: the domains of tick {} are {da:?}/{db:?}, the plan says {want_a:?}/{want_b:?}", seen + 1));
        }
        if !starts_next {
            plain += 1;
            if da != db || after != before {
                return Err(format!("seed {seed}: set_table changed tick {} or its guard without a pending table starting there: {script:?} {table:?}", seen + 1));
            }
        } else {
            replacing += 1;
            domain_changed += u64::from(da != db);
            guard_changed += u64::from(after != before);
        }
    }
    Ok((plain, replacing, domain_changed, guard_changed))
}

/// 속성 8: 틱이 한 바퀴 또는 여러 바퀴를 건너뛰어도 사건은 한 번이고 도메인은 도착한
/// 번호가 정합니다(건너뛴 틱의 도메인은 일하지 않음). 사건마다의 호출은 [finish]가 봅니다.
fn p_jumps(mk: Mk, sc: Scale) -> Result<(), String> {
    let mut tables = small_tables();
    tables.push(tbl(&[(0, 2), (1, 3), (2, 1)]));
    tables.push(tbl(&[(2, 1), (0, 1), (1, 1)]));
    tables.push(tbl(&[(1, 1), (1, 2), (0, 1), (1, 1)]));
    let specs = || {
        vec![
            spec(0, vec![Op::Spin]),
            spec(0, vec![Op::Yield, Op::Spin]),
            spec(1, vec![Op::Spin]),
            spec(1, vec![Op::Wait(1), Op::Spin]),
            spec(2, vec![Op::Spin]),
        ]
    };
    for (i, &t) in tables.iter().enumerate() {
        if !sc.exhaustive && i % 3 != 0 {
            continue;
        }
        let c = t.cycle() as u64;
        let mut extras = vec![0, 1, c, c + 1, 2 * c, 3 * c + 1, 5 * c + 2];
        if c > 1 {
            extras.push(c - 1);
        }
        for at in 1..=c + 2 {
            for &extra in &extras {
                for set_at in [0u64, 1, 2] {
                    let w = World {
                        specs: specs(),
                        changes: vec![(set_at, t)],
                        jumps: vec![(at, extra)],
                        horizon: 6 * c + 8,
                    };
                    let h = simulate(&w, &mut *mk());
                    check(&h, &format!("jump of {extra} after tick {at}, table {:?} set at {set_at}", t))?;
                    // 기준으로 센 도메인 변경 횟수와 enter_domain의 횟수
                    let mut k = 1;
                    let mut prev = Domain::ROOT;
                    let mut changes = 0;
                    while k <= w.horizon {
                        let d = h.plan.domain_at(k);
                        changes += usize::from(d != prev);
                        prev = d;
                        k += 1 + w.jumps.iter().find(|j| j.0 == k).map_or(0, |j| j.1);
                    }
                    let enters = h.calls.iter().filter(|c| matches!(c, Call::Enter(_))).count();
                    if enters != changes {
                        return Err(format!("{enters} enter_domain calls but {changes} domain changes: {:?} jump {at}+{extra}", t));
                    }
                }
            }
        }
    }
    Ok(())
}

fn root_tables() -> Vec<Table> {
    vec![
        tbl(&[(0, 3)]),
        tbl(&[(0, 1), (0, 2)]),
        tbl(&[(0, 2), (0, 2)]),
        tbl(&[(0, 255)]),
        tbl(&[(0, 1); 16]),
    ]
}

/// 속성 9: 루트만 든 표는 나뉜 표가 아니고 기본 표와 호출 하나하나까지 같게 움직입니다.
fn p_root_only(mk: Mk, sc: Scale) -> Result<(), String> {
    let tabs = root_tables();
    for t in &tabs {
        if !t.is_default() || !t.contains(Domain::ROOT) {
            return Err(format!("{t:?} must be a default table"));
        }
    }
    let same = |w: &World, base: &World, what: &str| -> Result<(), String> {
        let (ha, hb) = (simulate(w, &mut *mk()), simulate(base, &mut *mk()));
        check(&ha, what)?;
        check(&hb, what)?;
        if ha.stats.partitioned_events != 0 {
            return Err(format!("{what}: a root-only table was reported as partitioned"));
        }
        if ha.calls != hb.calls || ha.runs != hb.runs || ha.trace != hb.trace {
            return Err(format!("{what}: the calls differ from those of the default table"));
        }
        Ok(())
    };
    for seed in 0..sc.random {
        let mut w = random_world(seed ^ 0xA5A5, 40);
        for s in &mut w.specs {
            s.domain = 0;
        }
        let mut base = w.clone();
        base.changes = Vec::new();
        let mut r = Rng::new(seed);
        w.changes = (0..1 + r.below(3)).map(|_| (r.below(30), tabs[r.below(tabs.len() as u64) as usize])).collect();
        w.changes.sort_by_key(|x| x.0);
        same(&w, &base, &format!("root-only world {seed}"))?;
    }
    if sc.exhaustive {
        let progs = small_progs();
        for tab in &tabs[..3] {
            for a in &progs {
                for b in &progs {
                    let specs = vec![spec(0, a.clone()), spec(0, b.clone()), spec(0, vec![Op::Yield, Op::Spin])];
                    let base = World { specs: specs.clone(), changes: Vec::new(), jumps: vec![(3, 2)], horizon: 10 };
                    let w = World { changes: vec![(0, *tab)], ..base.clone() };
                    same(&w, &base, &format!("exhaustive root-only {:?} {:?} {:?}", tab, a, b))?;
                }
            }
        }
    }
    // 보류 표도 루트만 든 표면 나뉜 것이 아니다. 나뉜 표 위에 정하면 올라가기 전까지는 나뉜 것이다
    let mut c = mk();
    let mut h = Host::new(&[]);
    c.set_table(tabs[0]);
    if c.partitioned() {
        return Err("a pending root-only table must not count as partitioned".into());
    }
    c.set_table(tbl(&[(0, 1), (1, 1)]));
    if !c.partitioned() {
        return Err("a pending partitioned table counts as partitioned".into());
    }
    c.set_table(tabs[1]);
    if c.partitioned() {
        return Err("replacing the pending table by a root-only one clears partitioned".into());
    }
    c.set_table(tbl(&[(1, 1)]));
    for t in 1..=3 {
        h.now = t;
        c.tick(&mut h);
    }
    c.set_table(tabs[2]);
    if !c.partitioned() {
        return Err("a partitioned table in force stays partitioned until the root-only one starts".into());
    }
    for t in 4..=5 {
        h.now = t;
        c.tick(&mut h);
    }
    if c.partitioned() {
        return Err("partitioned must clear when the root-only table starts".into());
    }
    Ok(())
}

/// 속성 10: 사건마다 호출 순서와 상태 규칙(I1 I2 I3)과 도는 도메인이 맞는지는 [finish]가
/// 봅니다. 여기서는 되풀이한 틱과 건너뜀이 섞인 무작위 세계를 돌리고 본 사건의 수를
/// 합쳐 돌려줍니다.
fn p_events(mk: Mk, sc: Scale) -> Result<Stats, String> {
    let mut sum = Stats::default();
    for seed in 0..sc.random {
        let mut w = random_world(seed ^ 0xE7E7, 40);
        if w.jumps.is_empty() {
            let mut r = Rng::new(seed);
            w.jumps = rand_jumps(&mut r, 40);
        }
        let h = simulate(&w, &mut *mk());
        check(&h, &format!("event world {seed}"))?;
        let s = h.stats;
        sum.events += s.events;
        sum.boundaries += s.boundaries;
        sum.boundaries_running += s.boundaries_running;
        sum.boundaries_rotated += s.boundaries_rotated;
        sum.same_domain_ticks += s.same_domain_ticks;
        sum.idle_entries += s.idle_entries;
        sum.polls_without_tick += s.polls_without_tick;
        sum.yields += s.yields;
        sum.blocks += s.blocks;
        sum.repeats += s.repeats;
        sum.jumps += s.jumps;
        sum.partitioned_events += s.partitioned_events;
    }
    Ok(sum)
}

/// 모든 속성을 돌려 어느 것이 실패했는지 모아 돌려줍니다.
fn check_all(mk: Mk, sc: Scale) -> Vec<(&'static str, String)> {
    let mut bad = Vec::new();
    let mut note = |name: &'static str, r: Result<(), String>| {
        if let Err(e) = r {
            bad.push((name, e));
        }
    };
    note("window exactness", p_window_exact(mk, sc));
    note("two-run equality", p_two_run(mk, sc).map(|_| ()));
    note("default table equivalence", p_default_equal(mk, sc));
    note("share bound", p_share(mk, sc));
    note("table", p_table(mk, sc));
    note("tick rule at a boundary", p_tick_rule(mk));
    note("pending replacement", p_replace(mk, sc));
    note("guard answer after every event", p_guard_answer(mk, sc).map(|_| ()));
    note("guard soundness of set_table", p_guard_sound(mk, sc).map(|_| ()));
    note("jumps", p_jumps(mk, sc));
    note("root-only tables", p_root_only(mk, sc));
    note("call order and invariants", p_events(mk, sc).map(|_| ()));
    bad
}

#[test]
fn window_exactness_and_no_slack_donation() {
    p_window_exact(&real, FULL).unwrap();
}

#[test]
fn two_run_equality() {
    let differing = p_two_run(&real, FULL).unwrap();
    assert!(differing > FULL.random / 4, "workloads of the other domains must really differ: {differing}");
}

#[test]
fn two_run_equality_is_not_vacuous_with_a_shared_object() {
    // 도메인 1의 읽는 태스크가 오브젝트 2를 기다리고, 도메인 0의 태스크가 얼마
    // 뒤에 신호를 보냄. 신호를 보내는 쪽의 일정이 달라지면 도메인 1의 흔적도 달라짐
    let build = |wait: u64| World {
        specs: vec![
            spec(0, vec![Op::Wait(wait), Op::Post(2), Op::Spin]),
            spec(1, vec![Op::Pend(2), Op::Spin]),
        ],
        changes: vec![(0, tbl(&[(0, 1), (1, 1)]))],
        jumps: Vec::new(),
        horizon: 14,
    };
    let (a, b) = (simulate(&build(1), &mut *real()), simulate(&build(4), &mut *real()));
    check(&a, "a").unwrap();
    check(&b, "b").unwrap();
    assert_ne!(trace_of(&a, 1), trace_of(&b, 1), "a shared object must leak, else the property tests nothing");
    // 같은 두 세계에서 신호가 없으면(도메인 0이 오브젝트를 안 건드림) 흔적이 같다
    let quiet = |wait: u64| World {
        specs: vec![spec(0, vec![Op::Wait(wait), Op::Spin]), spec(1, vec![Op::Pend(2), Op::Spin])],
        changes: vec![(0, tbl(&[(0, 1), (1, 1)]))],
        jumps: Vec::new(),
        horizon: 14,
    };
    let (a, b) = (simulate(&quiet(1), &mut *real()), simulate(&quiet(4), &mut *real()));
    assert_eq!(trace_of(&a, 1), trace_of(&b, 1));
}

#[test]
fn default_table_equals_the_frozen_scheduler() {
    p_default_equal(&real, FULL).unwrap();
}

#[test]
fn share_bound_inside_a_domain() {
    p_share(&real, FULL).unwrap();
}

#[test]
fn table_timing_and_jumps() {
    p_table(&real, FULL).unwrap();
}

#[test]
fn tick_rule_at_a_boundary() {
    p_tick_rule(&real).unwrap();
}

#[test]
fn copy_equals_core() {
    // 틀림 없는 사본이 실제 Core와 같은 결정을 내리는지
    for seed in 0..FULL.random {
        let w = random_world(seed ^ 0x99, 40);
        let (a, b) = (simulate(&w, &mut *real()), simulate(&w, &mut *mutant(Mutant::None)));
        assert_eq!(a.runs, b.runs, "seed {seed}");
        assert_eq!(a.trace, b.trace, "seed {seed}");
        assert_eq!(a.bad, b.bad, "seed {seed}");
    }
    let bad = check_all(&|| mutant(Mutant::None), QUICK);
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn deliberate_breaks_are_caught() {
    for m in ALL_MUTANTS {
        let bad = check_all(&|| mutant(m), QUICK);
        println!("{:?}: caught by {:?}", m, bad.iter().map(|b| b.0).collect::<Vec<_>>());
        assert!(!bad.is_empty(), "{m:?} was not caught by any property");
    }
    // 새 연결 실수는 새 속성에 걸려야 한다
    let new_props = ["pending replacement", "jumps", "root-only tables", "call order and invariants"];
    for m in [
        Mutant::NoEnterDomain,
        Mutant::ServeBeforeEnter,
        Mutant::ServeOldInstead,
        Mutant::ReplaceStartsNextTick,
        Mutant::SpuriousAsTick,
        Mutant::BlockKeepsRunning,
        Mutant::StartsInThree,
        Mutant::SwitchesIgnoresPending,
    ] {
        let bad = check_all(&|| mutant(m), QUICK);
        assert!(bad.iter().any(|b| new_props.contains(&b.0)), "{m:?} was not caught by a new property: {bad:?}");
    }
    // 시계 산술의 연결 실수도 같은 자리에서 확인한다
    for v in crate::clock_tests::ALL_BREAKS {
        assert!(crate::clock_tests::caught(v), "{v:?} was not caught by the clock properties");
    }
}

/// 표의 읽기 쓰기를 한 단계씩 확인하는 기준입니다. Table::decode와 따로 적었습니다.
fn oracle(count: u64, regs: [u64; 4]) -> bool {
    if !(1..=16).contains(&count) {
        return false;
    }
    (0..16).all(|i| {
        let v = (regs[i / 4] >> (16 * (i % 4))) & 0xffff;
        if (i as u64) < count { (v & 0xff) < 8 && (v >> 8) >= 1 } else { v == 0 }
    })
}

#[test]
fn table_decode_accepts_exactly_the_well_formed() {
    // 항목 0의 16비트 전부 x 항목 수 0..=3 (나머지 칸은 0)
    for count in 0..=3u64 {
        for v in 0..=0xffffu64 {
            let regs = [v, 0, 0, 0];
            assert_eq!(Table::decode(count, regs).is_ok(), oracle(count, regs), "count {count} v {v:#x}");
        }
    }
    // 항목 0, 1은 대표값으로 격자, 둘째 칸은 전부
    let firsts = [0u64, 1, 0x0100, 0x0101, 0x0107, 0x0108, 0x01ff, 0xff07, 0x0007, 0x8001];
    for count in [1u64, 2, 3] {
        for &a in &firsts {
            for b in 0..=0xffffu64 {
                let regs = [a | (b << 16), 0, 0, 0];
                assert_eq!(Table::decode(count, regs).is_ok(), oracle(count, regs));
            }
        }
    }
    // 항목 수 경계
    for count in [0u64, 1, 15, 16, 17, 18, 255, u64::MAX] {
        let full = [0x0101_0101_0101_0101u64; 4];
        assert_eq!(Table::decode(count, full).is_ok(), oracle(count, full), "count {count}");
    }
    // 무작위: 성긴 비트와 올바른 인코딩에 비트 하나를 뒤집은 것
    let mut r = Rng::new(42);
    for _ in 0..400_000 {
        let count = r.below(20);
        let mut regs = [0u64; 4];
        for i in 0..count.min(16) as usize {
            let v = ((1 + r.below(255)) << 8) | r.below(8);
            regs[i / 4] |= v << (16 * (i % 4));
        }
        match r.below(3) {
            0 => {}
            1 => regs[r.below(4) as usize] ^= 1 << r.below(64),
            _ => regs[r.below(4) as usize] = r.next() & r.next() & r.next(),
        }
        assert_eq!(Table::decode(count, regs).is_ok(), oracle(count, regs), "count {count} regs {regs:x?}");
    }
}

#[test]
fn table_decode_reads_the_abi_packing_and_reports_the_first_fault() {
    let regs = [0x0201_0105_0301_0200u64, 0, 0, 0];
    let t = Table::decode(4, regs).unwrap();
    let got: Vec<(u8, u32)> = (0..t.len()).map(|i| (t.entry(i).unwrap().0.id(), t.entry(i).unwrap().1)).collect();
    // 항목 0은 하위 16비트 0x0200(틱 2, 도메인 0), 항목 1은 0x0301(틱 3, 도메인 1)
    assert_eq!(got, [(0, 2), (1, 3), (5, 1), (1, 2)]);
    assert_eq!(t.cycle(), 8);
    assert_eq!(Table::decode(0, [0; 4]), Err(TableError::Count));
    assert_eq!(Table::decode(17, [0; 4]), Err(TableError::Count));
    assert_eq!(Table::decode(1, [0x0108, 0, 0, 0]), Err(TableError::Domain));
    assert_eq!(Table::decode(1, [0x0001, 0x00, 0, 0]), Err(TableError::Window));
    assert_eq!(Table::decode(1, [0x0100 | (1 << 16), 0, 0, 0]), Err(TableError::Reserved));
    assert_eq!(Table::decode(1, [0x0100, 0, 0, 1 << 63]), Err(TableError::Reserved));
    assert!(Table::decode(1, [0x0100, 0, 0, 0]).unwrap().is_default());
    assert_eq!(Table::decode(1, [0x0100, 0, 0, 0]).unwrap(), Table::DEFAULT);
    let max = [0xff07_ff07_ff07_ff07u64; 4];
    assert_eq!(Table::decode(16, max).unwrap().cycle(), 16 * 255);
}

#[test]
fn table_helpers() {
    assert!(Table::DEFAULT.is_default() && Table::DEFAULT.contains(Domain::ROOT));
    assert!(!tbl(&[(0, 1), (1, 1)]).is_default());
    assert!(tbl(&[(0, 2), (0, 1)]).is_default());
    assert!(!tbl(&[(1, 1)]).contains(Domain::ROOT));
    assert_eq!(Table::from_entries(&[]), Err(TableError::Count));
    assert_eq!(Table::from_entries(&[(Domain::ROOT, 0)]), Err(TableError::Window));
    assert_eq!(Table::from_entries(&[(Domain::ROOT, 256)]), Err(TableError::Window));
    assert_eq!(Domain::new(8), None);
    let mut c = Core::<usize>::new();
    assert!(!c.partitioned());
    c.set_table(tbl(&[(0, 1), (1, 1)]));
    assert!(c.partitioned());
    c.set_table(Table::DEFAULT);
    assert!(!c.partitioned());
}

#[test]
fn share_lag_is_not_sloppy() {
    // 한계가 너무 느슨해서 속성이 비는 일이 없도록, 실제로 관측한 가장 큰 뒤처짐을 본다
    let mut worst = [i64::MIN; 6];
    for seed in 0..FULL.random {
        let (w, spinner, d, n) = share_world(seed);
        let l = worst_lag(&real, &w, spinner, d, n).unwrap();
        worst[n as usize] = worst[n as usize].max(l);
    }
    println!("observed worst lag by N: {:?}", &worst[2..5]);
    for (n, &w) in worst.iter().enumerate().skip(2).take(3) {
        assert!(w <= share_lag(n as u64) as i64);
        assert!(w > 0, "the workloads must really take share from the spinner");
    }
}

#[test]
fn a_pending_table_can_be_replaced_before_it_starts() {
    p_replace(&real, FULL).unwrap();
}

#[test]
fn replacement_hand_cases_with_next_tick_switches() {
    // 틱 5에 [(1,1)]을 정하면 틱 7에서 시작한다 틱 6에 [(2,1)]로 바꾸면 틱 8에서 시작한다
    let mut s = real();
    let mut h = script_host(replace_specs(), &mut *s);
    for t in 1..=5 {
        step_to(&mut h, &mut *s, t);
    }
    ev_set(&mut h, &mut *s, tbl(&[(1, 1)]));
    assert!(!s.next_tick_switches(), "tick 6 is still the root domain");
    step_to(&mut h, &mut *s, 6);
    assert!(s.next_tick_switches(), "tick 7 starts the pending table");
    ev_set(&mut h, &mut *s, tbl(&[(2, 1)]));
    assert!(!s.next_tick_switches(), "the replaced table no longer starts at tick 7");
    step_to(&mut h, &mut *s, 7);
    assert_eq!(s.running_domain(), Domain::ROOT);
    assert!(s.next_tick_switches(), "tick 8 starts the replacement");
    step_to(&mut h, &mut *s, 8);
    assert_eq!(s.running_domain().id(), 2);
    assert!(!s.next_tick_switches());
    h.bad.map_or(Ok(()), Err).unwrap();

    // 틱 5에 정한 표(시작 7)가 있는데 틱 처리가 멈춰 있다가 틱 8로 건너뛴 뒤에 다시 정한다
    let mut s = real();
    let mut h = script_host(replace_specs(), &mut *s);
    for t in 1..=5 {
        step_to(&mut h, &mut *s, t);
    }
    ev_set(&mut h, &mut *s, tbl(&[(1, 1)]));
    // 건너뛰어 도착한 틱 8에서 앞 표가 올라가 있고(도메인 1), 새 표는 틱 10에서 시작한다
    step_to(&mut h, &mut *s, 8);
    assert_eq!(s.running_domain().id(), 1);
    assert!(!s.next_tick_switches());
    ev_set(&mut h, &mut *s, tbl(&[(0, 1), (2, 1)]));
    assert!(!s.next_tick_switches(), "tick 9 is still the first table");
    step_to(&mut h, &mut *s, 9);
    assert_eq!(s.running_domain().id(), 1);
    assert!(s.next_tick_switches(), "tick 10 starts the second table at its entry 0");
    step_to(&mut h, &mut *s, 10);
    assert_eq!(s.running_domain().id(), 0);
    step_to(&mut h, &mut *s, 11);
    assert_eq!(s.running_domain().id(), 2);
    step_to(&mut h, &mut *s, 12);
    assert_eq!(s.running_domain().id(), 0);
    h.bad.map_or(Ok(()), Err).unwrap();
}

#[test]
fn next_tick_switches_equals_the_plan_after_every_event() {
    let n = p_guard_answer(&real, FULL).unwrap();
    assert!(n > 20_000, "the property must see many events: {n}");
}

#[test]
fn set_table_keeps_the_next_tick_unless_it_replaces_a_table_starting_there() {
    let (plain, replacing, domain_changed, guard_changed) = p_guard_sound(&real, FULL).unwrap();
    println!("plain {plain} replacing {replacing} domain changed {domain_changed} guard changed {guard_changed}");
    assert!(plain > 1000 && replacing > 100, "both sides must be exercised: {plain} {replacing}");
    assert!(domain_changed > 20, "the replacing side must really change a domain: {domain_changed}");
    assert!(guard_changed > 20, "the guard must follow the change at once: {guard_changed}");
}

#[test]
fn guard_soundness_by_hand_both_sides() {
    // 쪽 하나: 표를 정해도 다음 틱의 도메인과 예고는 그대로다(앞의 보류 표가 없음)
    let mut s = real();
    let mut h = script_host(guard_specs(), &mut *s);
    for t in 1..=3 {
        step_to(&mut h, &mut *s, t);
    }
    assert!(!s.next_tick_switches());
    ev_set(&mut h, &mut *s, tbl(&[(1, 1)]));
    assert!(!s.next_tick_switches(), "tick 4 is still the root domain, the table starts at 5");
    step_to(&mut h, &mut *s, 4);
    assert_eq!(s.running_domain(), Domain::ROOT);
    assert!(s.next_tick_switches(), "tick 5 starts the table");
    // 쪽 둘: Lean의 반례와 같은 모양: 틱 1에 정하면 3에서 시작하고, 틱 2에 다시 정하면 그 표는 버려진다
    let mut s = real();
    let mut h = script_host(guard_specs(), &mut *s);
    step_to(&mut h, &mut *s, 1);
    ev_set(&mut h, &mut *s, tbl(&[(1, 1)]));
    step_to(&mut h, &mut *s, 2);
    assert_eq!(s.running_domain(), Domain::ROOT);
    assert!(s.next_tick_switches(), "tick 3 starts the first table");
    ev_set(&mut h, &mut *s, tbl(&[(2, 1)]));
    assert!(!s.next_tick_switches(), "the guard follows the replacement at once: tick 3 stays root");
    step_to(&mut h, &mut *s, 3);
    assert_eq!(s.running_domain(), Domain::ROOT);
    assert!(s.next_tick_switches(), "tick 4 starts the replacement");
    step_to(&mut h, &mut *s, 4);
    assert_eq!(s.running_domain().id(), 2);
    h.bad.map_or(Ok(()), Err).unwrap();
    // 같은 열에서 둘째 설정이 없었다면 틱 3은 도메인 1이었다(예외가 실제로 도메인을 바꿈)
    let mut s = real();
    let mut h = script_host(guard_specs(), &mut *s);
    step_to(&mut h, &mut *s, 1);
    ev_set(&mut h, &mut *s, tbl(&[(1, 1)]));
    step_to(&mut h, &mut *s, 2);
    step_to(&mut h, &mut *s, 3);
    assert_eq!(s.running_domain().id(), 1);
    h.bad.map_or(Ok(()), Err).unwrap();
}

#[test]
fn jumps_over_one_cycle_and_over_several() {
    p_jumps(&real, FULL).unwrap();
    // 한 바퀴 6틱인 표에서 틱 2부터 틱 14로, 다시 틱 40으로 뛴다
    let w = World {
        specs: replace_specs(),
        changes: vec![(0, tbl(&[(0, 2), (1, 3), (0, 1)]))],
        jumps: vec![(3, 10), (15, 24)],
        horizon: 45,
    };
    let h = simulate(&w, &mut *real());
    check(&h, "jump").unwrap();
    let ticks: Vec<u64> = ran_by_tick(&h).iter().map(|x| x.0).collect();
    assert!(ticks.contains(&14) && ticks.contains(&40), "the events land on the arrival ticks: {ticks:?}");
    assert!(!ticks.contains(&5) && !ticks.contains(&20), "skipped ticks have no event: {ticks:?}");
    // 건너뛴 뒤의 도메인은 도착한 번호가 정한다: 틱 2부터 0,0,1,1,1,0 이므로 틱 14는 오프셋 12에서 0
    let mut s = real();
    let mut h = script_host(replace_specs(), &mut *s);
    ev_set(&mut h, &mut *s, tbl(&[(0, 2), (1, 3), (0, 1)]));
    step_to(&mut h, &mut *s, 1);
    step_to(&mut h, &mut *s, 2);
    for (t, d) in [(14u64, 0u8), (17, 1), (26, 0), (28, 1), (60, 1), (63, 0), (1000, 1)] {
        let c0 = h.calls.len();
        step_to(&mut h, &mut *s, t);
        assert_eq!(s.running_domain().id(), d, "tick {t}");
        let serves = h.calls[c0..].iter().filter(|c| matches!(c, Call::Serve(..))).count();
        assert_eq!(serves, 1, "one serve for the whole jump to {t}");
        let enters: Vec<&Call> = h.calls[c0..].iter().filter(|c| matches!(c, Call::Enter(_))).collect();
        assert!(enters.len() <= 1, "at most one domain entry for the jump to {t}");
    }
    h.bad.map_or(Ok(()), Err).unwrap();
}

#[test]
fn consecutive_entries_of_one_domain_are_not_a_boundary() {
    let pairs = [
        (tbl(&[(1, 1), (1, 2), (0, 1), (1, 1)]), tbl(&[(1, 3), (0, 1), (1, 1)])),
        (tbl(&[(0, 1), (1, 1), (1, 1)]), tbl(&[(0, 1), (1, 2)])),
        (tbl(&[(1, 1), (0, 1), (0, 1), (1, 1)]), tbl(&[(1, 1), (0, 2), (1, 1)])),
        (tbl(&[(1, 2), (1, 2)]), tbl(&[(1, 4)])),
    ];
    for (a, b) in pairs {
        let mk = |t: Table| World {
            specs: vec![
                spec(0, vec![Op::Spin]),
                spec(0, vec![Op::Yield, Op::Spin]),
                spec(1, vec![Op::Spin]),
                spec(1, vec![Op::Wait(1), Op::Spin]),
                spec(1, vec![Op::Spin]),
            ],
            changes: vec![(0, t)],
            jumps: Vec::new(),
            horizon: 24,
        };
        let (ha, hb) = (simulate(&mk(a), &mut *real()), simulate(&mk(b), &mut *real()));
        check(&ha, "a").unwrap();
        check(&hb, "b").unwrap();
        assert_eq!(ha.calls, hb.calls, "{a:?} and {b:?} describe the same plan");
        assert_eq!(ha.trace, hb.trace);
        // 기준 Plan으로 센 도메인 변경 횟수만큼만 들어간다(틱 1은 루트 그대로)
        let mut prev = Domain::ROOT;
        let mut changes = 0;
        for t in 1..=24 {
            let d = ha.plan.domain_at(t);
            changes += usize::from(d != prev);
            prev = d;
        }
        let enters = ha.calls.iter().filter(|c| matches!(c, Call::Enter(_))).count();
        assert_eq!(enters, changes, "{a:?}");
        // 세워 두는 일(맨 앞으로 넣기)은 도는 태스크가 있는 경계에서만 한 번씩 있다
        let fronts = ha.calls.iter().filter(|c| matches!(c, Call::PushFront(..))).count() as u64;
        assert_eq!(fronts, ha.stats.boundaries_running, "{a:?}");
        assert_eq!(ha.stats.boundaries as usize, changes);
    }
}

#[test]
fn state_invariants_and_call_order_hold_after_every_event_and_are_exercised() {
    let s = p_events(&real, FULL).unwrap();
    println!("{s:?}");
    assert!(s.events > 100_000);
    assert!(s.boundaries > 1000 && s.boundaries_running > 500 && s.boundaries_rotated > 50);
    assert!(s.same_domain_ticks > 1000 && s.idle_entries > 100 && s.polls_without_tick > 100);
    assert!(s.yields > 1000 && s.blocks > 1000 && s.repeats > 1000 && s.jumps > 100);
    assert!(s.partitioned_events > 1000);
}

#[test]
fn root_only_tables_are_not_partitioned_and_equal_the_default() {
    p_root_only(&real, FULL).unwrap();
}

#[test]
fn boundary_calls_come_in_the_designed_order() {
    use Call::*;
    let (d0, d1, d2) = (Domain::ROOT, Domain::new(1).unwrap(), Domain::new(2).unwrap());
    let run = |specs: Vec<Spec>, table: Table, f: &mut dyn FnMut(&mut Host, &mut dyn Sched)| {
        let mut s = real();
        let mut h = script_host(specs, &mut *s);
        ev_set(&mut h, &mut *s, table);
        f(&mut h, &mut *s);
        h.bad.clone().map_or(Ok(()), Err).unwrap();
        h
    };
    let at = |h: &mut Host, s: &mut dyn Sched, t: u64| -> Vec<Call> {
        let c0 = h.calls.len();
        step_to(h, s, t);
        h.calls[c0..].to_vec()
    };
    // 규칙이 Rotate이고 준비된 태스크가 없음: 도는 태스크를 세우고, 들어가고, 깨우고, 꺼내서 실행
    run(vec![spec(0, vec![Op::Spin]), spec(1, vec![Op::Spin])], tbl(&[(1, 1), (0, 1)]), &mut |h, s| {
        assert_eq!(at(h, s, 1), [Serve(d0, 1)]);
        assert_eq!(
            at(h, s, 2),
            [Pop(d0, None), PushFront(d0, 0), Enter(d1), Serve(d1, 2), Pop(d1, Some(1)), Run(1)]
        );
        assert_eq!(
            at(h, s, 3),
            [Pop(d1, None), PushFront(d1, 1), Enter(d0), Serve(d0, 3), Pop(d0, Some(0)), Run(0)]
        );
    });
    // 틱 사이에 들어온 태스크는 경계에서 Keep이라 그대로 맨 앞에 선다
    run(
        vec![spec(0, vec![Op::Spin]), spec(1, vec![Op::Spin]), spec(0, vec![Op::Spin])],
        tbl(&[(1, 1), (0, 1)]),
        &mut |h, s| {
            let c0 = h.calls.len();
            ev_yield(h, s);
            assert_eq!(h.calls[c0..], [Pop(d0, Some(2)), Push(d0, 0), Run(2)]);
            assert_eq!(
                at(h, s, 2),
                [PushFront(d0, 2), Enter(d1), Serve(d1, 2), Pop(d1, Some(1)), Run(1)]
            );
            // 틱 3에서 돌아오면 맨 앞은 2, 그다음 0
            assert_eq!(
                at(h, s, 3),
                [Pop(d1, None), PushFront(d1, 1), Enter(d0), Serve(d0, 3), Pop(d0, Some(2)), Run(2)]
            );
        },
    );
    // Rotate이고 준비된 태스크가 있음: 큐 앞을 꺼내 도는 태스크를 뒤에 넣고 꺼낸 것을 다시 맨 앞에 둔다
    run(
        vec![spec(0, vec![Op::Spin]), spec(1, vec![Op::Spin]), spec(0, vec![Op::Spin]), spec(0, vec![Op::Spin])],
        tbl(&[(1, 1), (0, 1)]),
        &mut |h, s| {
            assert_eq!(at(h, s, 1), [Serve(d0, 1)]);
            assert_eq!(
                at(h, s, 2),
                [
                    Pop(d0, Some(2)),
                    Push(d0, 0),
                    PushFront(d0, 2),
                    Enter(d1),
                    Serve(d1, 2),
                    Pop(d1, Some(1)),
                    Run(1)
                ]
            );
            assert_eq!(h.q[0], [2, 3, 0]);
        },
    );
    // 들어갈 도메인이 비어 있음: 일할 것이 없으면 Idle이고 다른 도메인의 큐는 건드리지 않는다
    run(vec![spec(0, vec![Op::Spin]), spec(1, vec![Op::Spin])], tbl(&[(2, 1), (0, 1)]), &mut |h, s| {
        assert_eq!(at(h, s, 1), [Serve(d0, 1)]);
        assert_eq!(at(h, s, 2), [Pop(d0, None), PushFront(d0, 0), Enter(d2), Serve(d2, 2), Pop(d2, None)]);
        assert!(h.current.is_none() && h.q[1] == [1]);
        // 같은 틱에 입력으로 깨어난 poll은 도는 도메인만 일한다
        let c0 = h.calls.len();
        let r = ev_poll(h, s);
        assert_eq!(r, Next::Idle);
        assert_eq!(h.calls[c0..], [Serve(d2, 2), Pop(d2, None)]);
        assert_eq!(at(h, s, 3), [Enter(d0), Serve(d0, 3), Pop(d0, Some(0)), Run(0)]);
    });
}

// 빈 채로 기다리던 도메인에 창 안쪽의 틱에서 들어오는 태스크: 도메인 1은 두 세계에서
// 같고 도메인 0만 태스크 하나가 다름. 다른 도메인의 태스크가 남긴 틱 면제가 넘어오면
// 도메인 1의 순서가 도메인 0의 태스크 수에 따라 달라짐
#[test]
fn a_task_entering_an_idle_domain_at_an_interior_tick_owes_nothing_to_another_domain() {
    let world = |extra: bool| {
        let late = || {
            let mut p = vec![Op::Wait(5)];
            p.extend(std::iter::repeat_n(Op::Spin, 60));
            p
        };
        let mut specs = vec![spec(0, vec![Op::Wait(3)]), spec(1, late()), spec(1, late())];
        if extra {
            specs.push(spec(0, vec![Op::Wait(3)]));
        }
        World { specs, changes: vec![(0, tbl(&[(0, 1), (1, 3)]))], jumps: Vec::new(), horizon: 12 }
    };
    let (ha, hb) = (simulate(&world(true), &mut *real()), simulate(&world(false), &mut *real()));
    check(&ha, "world A").unwrap();
    check(&hb, "world B").unwrap();
    // 보려는 경우에 실제로 닿음: 도메인 1의 두 태스크가 빈 창 안쪽의 틱 8에서 깨어남
    let woke8 = trace_of(&ha, 1).iter().filter(|e| matches!(e, Ev::Woke { tick: 8, .. })).count();
    assert_eq!(woke8, 2, "the pair must reach the idle entry at an interior tick");
    let at9 = |h: &Host| -> Vec<usize> {
        trace_of(h, 1).iter().filter_map(|e| if let Ev::Ran { tick: 9, task, .. } = e { Some(*task) } else { None }).collect()
    };
    assert_eq!(trace_of(&ha, 1), trace_of(&hb, 1), "domain 1 differs: tick 9 ran {:?} / {:?}", at9(&ha), at9(&hb));
    assert_eq!(at9(&ha), vec![2, 2, 2], "the task that entered at tick 8 must be rotated at tick 9");
}
