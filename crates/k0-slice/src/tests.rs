//! 규칙을 가짜 준비 큐 위에서 검증하는 테스트 모듈입니다.
//!
//! 모형은 커널의 스케줄러가 하는 일을 한 주기가 100칸인 시간 위에서 흉내
//! 냅니다. 준비 큐는 선입선출이고 깨어난 태스크는 큐의 끝에 섭니다. 틱은 먼저
//! 기다리던 태스크를 깨운 다음에 현재 태스크를 판정합니다. 틱 n개짜리 기한
//! 대기는 그때의 틱 수에 n + 1을 더한 틱에서 끝납니다. 양보는 준비된 태스크가
//! 있을 때만 전환합니다.
//!
//! 규칙이 옳은지는 규칙과 따로 적은 기준으로 판정합니다. 모형은 전환이 틱의
//! 판정에서 일어났는지 틱 사이에서 일어났는지를 스스로 알고 있으므로, 틱마다
//! "틱 사이에 들어와 아직 틱을 맞지 않은 태스크만 넘어가고, 나머지는 준비된
//! 태스크가 있으면 반드시 끊긴다"와 실제로 일어난 일을 비교합니다. 커널이
//! 규칙에 경위를 잘못 알려 주는 경우([Wiring]의 나머지 값)도 같은 기준에
//! 걸리는지 봅니다. 이 기준은 규칙을 다른 말로 다시 적은 것이라서, 결과로
//! 나타나는 성질(끊긴 구간의 길이, 몫, 기다린 시간)을 따로 확인합니다.
//!
//! `Wiring::Kernel`은 커널의 연결을 손으로 옮겨 적은 것입니다(k0-sched의
//! `rotate`, `on_tick`, `block_and_switch`, `resume_from_idle`, `handoff`와
//! kernel의 `idle_until_ready`). 커널 쪽 연결이 바뀌어도 이 테스트는 실패하지
//! 않으므로, 연결을 고칠 때는 여기도 함께 고쳐야 합니다.
//!
//! 모형이 다루지 않는 것은 다음과 같습니다. 태스크가 새로 만들어지는 경우,
//! 시스템 콜에 걸리는 시간, 긴 시스템 콜 때문에 늦어지거나 합쳐지는 틱, 그리고
//! 실제 준비 큐의 구현입니다.

use std::collections::VecDeque;

use super::*;

/// 한 주기의 길이(모형의 시간 단위)
const P: u64 = 100;

/// 태스크가 정해진 시간만큼 돈 뒤에 하는 일입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// 양보도 블록도 하지 않고 끝없이 돎
    Spin,
    Yield,
    /// 틱 n개짜리 기한 대기
    Wait(u64),
    /// 틱과 무관한 시각에 끝나는 대기(콘솔 입력). 값은 깨어나기까지의 시간
    Input(u64),
    Exit,
}

/// (도는 시간, 그 뒤에 하는 일)의 되풀이
type Prog = Vec<(u64, Op)>;

/// 커널이 규칙에 경위를 알려 주는 방식입니다. `Kernel`이 실제 연결이고 나머지는
/// 일부러 틀리게 만든 것입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wiring {
    Kernel,
    /// 규칙이 없던 때: 틱은 언제나 현재 태스크를 끊음
    Old,
    YieldAsTick,
    BlockAsTick,
    ExitAsTick,
    TickAsBetween,
    IdleTickAsBetween,
    IdleInputAsTick,
    BootAsTick,
    /// 준비된 태스크가 없어 전환하지 않은 양보가 면제를 새로 줌
    YieldAloneMarks,
}

/// 전환이 일어나는 자리입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    Tick,
    Yield,
    Block,
    Exit,
    /// 실행할 태스크 없이 기다리다가 틱에서 깨어남
    IdleTick,
    /// 실행할 태스크 없이 기다리다가 틱이 아닌 시각의 입력으로 깨어남
    IdleInput,
    /// 부팅 뒤 첫 태스크의 시작(주기의 중간)
    Boot,
}

impl Path {
    /// 규칙과 따로 적은 기준: 이 자리에서 들어온 태스크가 주기의 처음에
    /// 들어온 것인지, 중간에 들어온 것인지
    fn truth(self) -> Entry {
        match self {
            Path::Tick | Path::IdleTick => Entry::Tick,
            Path::Yield | Path::Block | Path::Exit | Path::IdleInput | Path::Boot => {
                Entry::Between
            }
        }
    }
}

/// 모형이 시작하는 시각. 커널이 루트 태스크로 넘어가는 때처럼 주기의 중간임
const START: u64 = P / 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Blocked,
    Dead,
}

struct Task {
    prog: Prog,
    pc: usize,
    left: u64,
    state: State,
    /// 기한 대기가 끝나는 틱 수(0 = 없음)
    deadline: u64,
    /// 입력 대기가 끝나는 시각(0 = 없음)
    wake_at: u64,
    cpu: u64,
    /// 준비 큐에 선 시각과 그때 이 태스크보다 먼저 실행될 태스크의 수(큐에서
    /// 앞에 선 태스크와, 있다면 실행 중인 태스크)
    queued: Option<(u64, u64)>,
}

#[derive(Clone, Copy, Debug)]
struct Run {
    task: usize,
    truth: Entry,
    start: u64,
    ticks_seen: u64,
}

struct Sim {
    tasks: Vec<Task>,
    queue: VecDeque<usize>,
    run: Option<Run>,
    slice: Slice,
    now: u64,
    ticks: u64,
    wiring: Wiring,
    /// 시스템 콜과 틱이 같은 시각에 걸렸을 때 틱을 먼저 처리하는지
    tick_first: bool,
    /// 기준과 실제가 어긋난 틱의 수
    violations: u64,
    /// 틱에 끊긴 실행 구간의 수와 그 가운데 가장 짧은 것
    cuts: u64,
    shortest_cut: u64,
    /// 준비 큐에서 기다린 시간에서 "먼저 실행될 태스크마다 한 주기"를 뺀 값
    /// 가운데 가장 큰 것
    worst_wait: i64,
}

impl Sim {
    /// 0번 태스크가 주기의 중간에서 실행을 시작하고 나머지는 번호 순서로 준비
    /// 큐에 선 상태를 만드는 함수입니다.
    fn new(progs: Vec<Prog>, wiring: Wiring) -> Self {
        let tasks = progs
            .into_iter()
            .map(|prog| {
                assert!(prog.iter().all(|&(run, _)| run >= 1), "every step must take time");
                Task {
                    left: prog[0].0,
                    prog,
                    pc: 0,
                    state: State::Ready,
                    deadline: 0,
                    wake_at: 0,
                    cpu: 0,
                    queued: None,
                }
            })
            .collect::<Vec<_>>();
        let mut s = Self {
            tasks,
            queue: VecDeque::new(),
            run: Some(Run {
                task: 0,
                truth: Path::Boot.truth(),
                start: START,
                ticks_seen: 0,
            }),
            slice: Slice::new(),
            now: START,
            ticks: 0,
            wiring,
            tick_first: false,
            violations: 0,
            cuts: 0,
            shortest_cut: u64::MAX,
            worst_wait: i64::MIN,
        };
        s.tasks[0].state = State::Running;
        s.slice.entered(s.told(Path::Boot));
        for t in 1..s.tasks.len() {
            s.enqueue(t);
        }
        s
    }

    fn enqueue(&mut self, t: usize) {
        self.tasks[t].state = State::Ready;
        let before = self.queue.len() as u64 + u64::from(self.run.is_some());
        self.tasks[t].queued = Some((self.now, before));
        self.queue.push_back(t);
    }

    /// 이 연결이 규칙에 알려 주는 경위
    fn told(&self, path: Path) -> Entry {
        let kernel = match path {
            Path::Tick => Entry::Tick,
            Path::Yield | Path::Block | Path::Exit | Path::Boot => Entry::Between,
            Path::IdleTick => Entry::after_idle(true),
            Path::IdleInput => Entry::after_idle(false),
        };
        match (self.wiring, path) {
            (Wiring::YieldAsTick, Path::Yield)
            | (Wiring::BlockAsTick, Path::Block)
            | (Wiring::ExitAsTick, Path::Exit)
            | (Wiring::IdleInputAsTick, Path::IdleInput)
            | (Wiring::BootAsTick, Path::Boot) => Entry::Tick,
            (Wiring::TickAsBetween, Path::Tick) | (Wiring::IdleTickAsBetween, Path::IdleTick) => {
                Entry::Between
            }
            _ => kernel,
        }
    }

    /// 준비 큐의 머리를 현재 태스크로 만드는 함수입니다.
    fn switch_in(&mut self, path: Path) {
        let t = self.queue.pop_front().expect("switch with an empty queue");
        let (t0, before) = self.tasks[t].queued.take().expect("ready task without a record");
        let over = (self.now - t0) as i64 - (before * P) as i64;
        self.worst_wait = self.worst_wait.max(over);
        self.tasks[t].state = State::Running;
        self.slice.entered(self.told(path));
        self.run = Some(Run {
            task: t,
            truth: path.truth(),
            start: self.now,
            ticks_seen: 0,
        });
    }

    fn tick(&mut self) {
        self.ticks += 1;
        // 커널의 틱 처리 순서: 입력을 기다리던 태스크, 기한이 지난 대기자, 판정
        for t in 0..self.tasks.len() {
            let task = &mut self.tasks[t];
            if task.state == State::Blocked && task.wake_at != 0 && task.wake_at <= self.now {
                task.wake_at = 0;
                self.enqueue(t);
            }
        }
        for t in 0..self.tasks.len() {
            let task = &mut self.tasks[t];
            if task.state == State::Blocked && task.deadline != 0 && task.deadline <= self.ticks {
                task.deadline = 0;
                self.enqueue(t);
            }
        }
        let Some(mut run) = self.run else {
            if !self.queue.is_empty() {
                self.switch_in(Path::IdleTick);
            }
            return;
        };
        let verdict = if self.wiring == Wiring::Old { Verdict::Rotate } else { self.slice.on_tick() };
        let rotated = verdict == Verdict::Rotate && !self.queue.is_empty();
        let exempt = run.truth == Entry::Between && run.ticks_seen == 0;
        if (exempt && rotated) || (!exempt && !self.queue.is_empty() && !rotated) {
            self.violations += 1;
        }
        run.ticks_seen += 1;
        self.run = Some(run);
        if rotated {
            self.cuts += 1;
            self.shortest_cut = self.shortest_cut.min(self.now - run.start);
            // 커널과 같은 순서: 머리를 꺼낸 뒤에 끊긴 태스크를 끝에 넣음
            self.switch_in(Path::Tick);
            self.enqueue(run.task);
        }
    }

    /// 현재 태스크가 자기 차례의 일을 하는 함수입니다.
    fn do_op(&mut self) {
        let run = self.run.expect("an operation without a running task");
        let task = &mut self.tasks[run.task];
        let (_, op) = task.prog[task.pc];
        task.pc = (task.pc + 1) % task.prog.len();
        task.left = task.prog[task.pc].0;
        match op {
            Op::Spin => task.left = u64::MAX / 4,
            Op::Yield => {
                if !self.queue.is_empty() {
                    self.switch_in(Path::Yield);
                    self.enqueue(run.task);
                } else if self.wiring == Wiring::YieldAloneMarks {
                    self.slice.entered(Entry::Between);
                }
            }
            Op::Wait(_) | Op::Input(_) | Op::Exit => {
                let path = match op {
                    Op::Wait(n) => {
                        task.state = State::Blocked;
                        task.deadline = self.ticks + n + 1;
                        Path::Block
                    }
                    Op::Input(after) => {
                        task.state = State::Blocked;
                        task.wake_at = self.now + after.max(1);
                        Path::Block
                    }
                    _ => {
                        task.state = State::Dead;
                        Path::Exit
                    }
                };
                self.run = None;
                if !self.queue.is_empty() {
                    self.switch_in(path);
                }
            }
        }
    }

    /// 다음 사건까지 시간을 보내고 그 사건 하나를 처리하는 함수입니다. 같은
    /// 시각이면 현재 태스크의 시스템 콜, 입력, 틱의 순서입니다(시스템 콜을
    /// 처리하는 동안 도착한 틱은 그 뒤에 받으므로). `tick_first`면 시스템 콜과
    /// 겹친 틱을 먼저 처리합니다(틱이 시스템 콜 바로 앞에 도착한 경우).
    fn advance(&mut self) {
        let next_tick = (self.ticks + 1) * P;
        let input = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.state == State::Blocked && t.wake_at != 0)
            .map(|(i, t)| (t.wake_at, i))
            .min();
        let op_at = self.run.map(|r| self.now + self.tasks[r.task].left);
        let mut at = next_tick;
        if let Some((w, _)) = input {
            at = at.min(w);
        }
        if let Some(o) = op_at {
            at = at.min(o);
        }
        if let Some(r) = self.run {
            self.tasks[r.task].left -= at - self.now;
            self.tasks[r.task].cpu += at - self.now;
        }
        self.now = at;
        if op_at == Some(at) && !(self.tick_first && at == next_tick) {
            self.do_op();
        } else if let Some((w, t)) = input.filter(|&(w, _)| w == at && w < next_tick) {
            debug_assert_eq!(w, at);
            self.tasks[t].wake_at = 0;
            self.enqueue(t);
            if self.run.is_none() {
                self.switch_in(Path::IdleInput);
            }
        } else {
            self.tick();
        }
    }

    /// 시작한 뒤로 지난 시간
    fn elapsed(&self) -> u64 {
        self.now - START
    }

    /// 시작한 뒤로 `span`만큼 지날 때까지 돌리는 함수입니다.
    fn run_for(&mut self, span: u64, mut each: impl FnMut(&Sim)) {
        while self.elapsed() < span {
            self.advance();
            each(self);
        }
    }
}

struct Rng(u64);

impl Rng {
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

/// 틱 경계 바로 앞뒤에 걸리는 길이가 자주 나오게 고른 무작위 프로그램
fn random_prog(r: &mut Rng) -> Prog {
    (0..1 + r.below(4))
        .map(|_| {
            let run = match r.below(8) {
                0 => 1,
                1 => P - 1,
                2 => P,
                3 => P + 1,
                4 => 2 * P - 1,
                _ => 1 + r.below(3 * P),
            };
            let op = match r.below(20) {
                0..=7 => Op::Yield,
                8..=12 => Op::Wait(1 + r.below(3)),
                13..=16 => Op::Input(1 + r.below(3 * P)),
                17 => Op::Exit,
                _ => Op::Spin,
            };
            (run, op)
        })
        .collect()
}

fn random_sim(seed: u64, wiring: Wiring, tick_first: bool) -> Sim {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let n = 2 + r.below(4);
    let progs = (0..n).map(|_| random_prog(&mut r)).collect();
    let mut s = Sim::new(progs, wiring);
    s.tick_first = tick_first;
    s.run_for(300 * P, |_| {});
    s
}

/// 끝없이 도는 태스크의 프로그램
fn spinner() -> Prog {
    vec![(1, Op::Spin)]
}

/// 모형을 돌리면서 `victim`번 태스크의 몫을 지켜보는 함수입니다. 시작부터 어느
/// 시점까지를 재도 그 태스크가 받은 시간이 "지난 시간의 1/n"에서 뒤처진 정도가
/// 한계를 넘는 순간이 있었으면 false를 함께 줍니다.
///
/// 한계는 그 태스크가 차례를 한 번 기다리는 동안 생길 수 있는 가장 큰
/// 뒤처짐입니다. 다른 태스크 n-1개가 한 주기씩, 그리고 시작한 시각부터 첫 틱까지
/// 입니다. 차례가 올 때마다 온전한 한 주기를 받으므로 뒤처짐은 쌓이지 않습니다.
fn watch_share(progs: Vec<Prog>, victim: usize, wiring: Wiring, periods: u64) -> (Sim, bool) {
    let n = progs.len() as u64;
    let lag = (n - 1) * P + (P - START);
    let mut kept = true;
    let mut run = |tick_first| {
        let mut s = Sim::new(progs.clone(), wiring);
        s.tick_first = tick_first;
        s.run_for(periods * P, |s| {
            if s.tasks[victim].cpu * n + lag < s.elapsed() {
                kept = false;
            }
        });
        s
    };
    // 시스템 콜과 틱이 겹칠 때의 두 순서를 모두 돌리고, 돌려주는 모형은 시스템
    // 콜이 먼저인 쪽
    run(true);
    let s = run(false);
    (s, kept)
}

/// 0번이 끝없이 도는 태스크이고 나머지가 주어진 프로그램인 모형을 돌리는
/// 함수입니다.
fn against_a_spinner(others: Vec<Prog>, wiring: Wiring, periods: u64) -> (Sim, bool) {
    let mut progs = vec![spinner()];
    progs.extend(others);
    watch_share(progs, 0, wiring, periods)
}

fn permille(s: &Sim, t: usize) -> u64 {
    s.tasks[t].cpu * 1000 / s.elapsed()
}

#[test]
fn the_rule_alone() {
    let mut s = Slice::new();
    assert_eq!(s.on_tick(), Verdict::Rotate);
    s.entered(Entry::Between);
    assert_eq!(s.on_tick(), Verdict::Keep);
    assert_eq!(s.on_tick(), Verdict::Rotate);
    assert_eq!(s.on_tick(), Verdict::Rotate);
    // 면제는 쌓이지 않고, 틱에서 들어온 태스크는 앞 태스크의 면제를 물려받지 않음
    s.entered(Entry::Between);
    s.entered(Entry::Between);
    assert_eq!(s.on_tick(), Verdict::Keep);
    assert_eq!(s.on_tick(), Verdict::Rotate);
    s.entered(Entry::Between);
    s.entered(Entry::Tick);
    assert_eq!(s.on_tick(), Verdict::Rotate);
    assert_eq!(Slice::default(), Slice::new());
    assert_eq!(Entry::after_idle(true), Entry::Tick);
    assert_eq!(Entry::after_idle(false), Entry::Between);
}

#[test]
fn a_task_is_never_cut_before_a_full_period() {
    let (mut cuts, mut shortest) = (0, u64::MAX);
    for seed in 1..=4000 {
        for tick_first in [false, true] {
            let s = random_sim(seed, Wiring::Kernel, tick_first);
            assert_eq!(s.violations, 0, "seed {seed} tick first {tick_first}");
            cuts += s.cuts;
            shortest = shortest.min(s.shortest_cut);
        }
    }
    assert!(cuts > 100_000, "too few preemptions to mean anything: {cuts}");
    // 끊긴 구간은 모두 한 주기 이상이었고, 꼭 한 주기인 것도 있었음
    assert_eq!(shortest, P);
}

#[test]
fn the_old_rule_and_every_wrong_wiring_are_caught() {
    for wiring in [
        Wiring::Old,
        Wiring::YieldAsTick,
        Wiring::BlockAsTick,
        Wiring::ExitAsTick,
        Wiring::TickAsBetween,
        Wiring::IdleTickAsBetween,
        Wiring::IdleInputAsTick,
        Wiring::BootAsTick,
        Wiring::YieldAloneMarks,
    ] {
        let bad: u64 = (1..=400).map(|seed| random_sim(seed, wiring, false).violations).sum();
        assert!(bad > 0, "{wiring:?} was not caught");
    }
    // 예전 방식에서는 한 주기보다 짧게 돌고 끊기는 구간이 실제로 나옴
    let shortest = (1..=400).map(|seed| random_sim(seed, Wiring::Old, false).shortest_cut).min();
    assert!(shortest.unwrap() < P / 10);
}

#[test]
fn yielding_just_before_the_tick_no_longer_takes_the_cpu() {
    // 공격하는 태스크 하나가 주기의 1%부터 99% 지점까지에서 양보함
    let (mut worst_old, mut worst_new) = (1000, 1000);
    for pct in 1..100 {
        let attacker = vec![(pct * P / 100, Op::Yield)];
        let (s, kept) = against_a_spinner(vec![attacker.clone()], Wiring::Kernel, 400);
        assert!(kept, "yield at {pct}%");
        worst_new = worst_new.min(permille(&s, 0));
        let (s, _) = against_a_spinner(vec![attacker], Wiring::Old, 400);
        worst_old = worst_old.min(permille(&s, 0));
    }
    assert!(worst_new >= 499, "share fell to {worst_new} permille");
    assert!(worst_old <= 15, "the old rule must show the attack, got {worst_old} permille");

    // QEMU에서 잰 경우와 같은 지점: 90% 지점에서 양보하면 예전에는 상대가 10%쯤만
    // 받았고 지금은 절반을 넘김
    let (s, _) = against_a_spinner(vec![vec![(90, Op::Yield)]], Wiring::Old, 400);
    assert!((95..=105).contains(&permille(&s, 0)), "{}", permille(&s, 0));
    let (s, _) = against_a_spinner(vec![vec![(90, Op::Yield)]], Wiring::Kernel, 400);
    assert!((545..=555).contains(&permille(&s, 0)), "{}", permille(&s, 0));
}

#[test]
fn the_task_behind_the_yielder_is_the_one_that_lost() {
    // 0번이 공격하는 태스크, 1번이 그 바로 뒤에 선 태스크, 2번이 그다음
    let progs = || vec![vec![(90, Op::Yield)], spinner(), spinner()];
    let shares = |wiring| {
        let mut s = Sim::new(progs(), wiring);
        s.run_for(600 * P, |_| {});
        [permille(&s, 0), permille(&s, 1), permille(&s, 2)]
    };
    let near = |got: [u64; 3], want: [u64; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 3);
    let old = shares(Wiring::Old);
    assert!(near(old, [450, 50, 500]), "{old:?}");
    let new = shares(Wiring::Kernel);
    assert!(near(new, [300, 367, 333]), "{new:?}");
}

#[test]
fn a_spinner_keeps_its_share_against_random_tasks() {
    let mut old_failed = false;
    for seed in 1..=3000u64 {
        let mut r = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
        let others: Vec<Prog> = (0..1 + r.below(4)).map(|_| random_prog(&mut r)).collect();
        let (_, kept) = against_a_spinner(others.clone(), Wiring::Kernel, 200);
        assert!(kept, "seed {seed}: {others:?}");
        old_failed |= !against_a_spinner(others, Wiring::Old, 200).1;
    }
    assert!(old_failed, "the old rule should lose the share in some schedule");
}

#[test]
fn a_spinner_keeps_its_share_against_colluding_tasks() {
    // 공격하는 태스크 여럿이 저마다 다른 지점에서 양보하거나 기한 대기를 거는
    // 모든 조합(주기를 아홉 지점으로 나눔)
    let points: Vec<u64> = (1..10).map(|i| i * P / 10 - 1).chain([P - 1]).collect();
    let choices: Vec<(u64, Op)> = points
        .iter()
        .flat_map(|&at| [(at, Op::Yield), (at, Op::Wait(1))])
        .collect();
    for attackers in 1..=3usize {
        let mut pick = vec![0usize; attackers];
        'grid: loop {
            let others: Vec<Prog> = pick.iter().map(|&i| vec![choices[i]]).collect();
            let (_, kept) = against_a_spinner(others, Wiring::Kernel, 120);
            assert!(kept, "{attackers} attackers, choices {pick:?}");
            for slot in pick.iter_mut() {
                *slot += 1;
                if *slot < choices.len() {
                    continue 'grid;
                }
                *slot = 0;
            }
            break;
        }
    }
}

#[test]
fn blocking_must_grant_the_exemption_too() {
    // 양보에만 면제를 주면 블록으로 같은 일을 할 수 있음. 틱 바로 앞에서 번갈아
    // 기한 대기를 거는 두 태스크 사이에 선 태스크(1번)는 주기마다 한 칸만 받음
    let waiter = || vec![(P - 1, Op::Wait(1))];
    let progs = || vec![waiter(), spinner(), waiter()];
    let (s, kept) = watch_share(progs(), 1, Wiring::BlockAsTick, 400);
    assert!(!kept && permille(&s, 1) <= 15, "share {} permille", permille(&s, 1));
    let (s, kept) = watch_share(progs(), 1, Wiring::Kernel, 400);
    assert!(kept && permille(&s, 1) >= 333, "share {} permille", permille(&s, 1));
}

#[test]
fn waiting_costs_at_most_one_period_more_than_before() {
    // 먼저 실행될 태스크가 k개일 때(실행 중인 태스크 포함), 예전 방식에서는 k주기
    // 안에 실행됐음. 지금은 k + 1주기 안임. 늘어난 만큼은 한 주기를 넘지 않고,
    // 틱이 시스템 콜 바로 앞에 도착하는 순서에서는 꼭 한 주기까지 나타남
    let worst = |wiring, tick_first| {
        (1..=3000).map(|seed| random_sim(seed, wiring, tick_first).worst_wait).max().unwrap()
    };
    for tick_first in [false, true] {
        assert_eq!(worst(Wiring::Old, tick_first), 0);
        let now = worst(Wiring::Kernel, tick_first);
        println!("worst wait beyond one period per task ahead (tick first {tick_first}): {now} of {P}");
        assert!(now > P as i64 / 2 && now <= P as i64, "{now}");
    }
    assert_eq!(worst(Wiring::Kernel, true), P as i64);
}
