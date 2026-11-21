//! 시간 분할 표에 따라 어느 도메인이 CPU를 쓰는지 정하고 그 안의 태스크 전환을
//! 판정하는 크레이트입니다.
//!
//! # Features
//! 태스크는 도메인에 속하고, 표(Table)가 "어느 도메인이 몇 틱 동안 도는가"를
//! 정합니다. 이 크레이트는 스케줄링 판정을 전부 담고 하드웨어와 준비 큐에는
//! 닿지 않습니다. 준비 큐, 현재 태스크 설치, 기한 목록은 [Env]를 통해 커널이
//! 맡으므로 호스트 테스트가 같은 코드를 가짜 환경 위에서 돌립니다.
//!
//! 규칙은 다음과 같습니다.
//!
//! - 틱 번호 n에서 도는 도메인은 표와 n만으로 정해집니다. 다른 도메인의
//!   태스크가 무엇을 하든 바뀌지 않습니다.
//! - 도메인의 창이 열려 있는데 그 도메인에 준비된 태스크가 없으면 CPU는
//!   놉니다. 다른 도메인에 넘어가지 않습니다(남는 시간의 양도 없음).
//! - 깨어나는 태스크는 자기 도메인의 큐에만 들어갑니다. 깨우는 일은 이
//!   크레이트의 사건이 아니며 커널이 `Env::push`에 태스크 자신의 도메인을
//!   넘겨 합니다.
//! - 도메인 안에서는 k0-slice의 틱 규칙이 그대로 쓰입니다. 창의 경계에서는
//!   도는 태스크에 규칙을 한 번 적용한 뒤 자기 도메인 큐에 세워 두고, 그
//!   도메인의 다음 창에서 큐 순서대로 다시 들어옵니다.
//! - 표를 바꾸면 현재 틱 + 2에서 표의 0번 항목부터 효력이 생깁니다.
//!
//! 틱이 늦게 처리되어 번호가 여럿 건너뛰어도 사건은 한 번이고, 도는 도메인은
//! 도착한 번호로 정해집니다.
//!
//! 이 크레이트가 주장하는 것은 "도메인 사이에서 스케줄러를 거치는 채널을
//! 닫는다"까지입니다. 캐시, 예측기, 커널 경로의 길이 같은 다른 채널은 다루지
//! 않습니다. 도메인이 공유하기로 한 오브젝트(엔드포인트 등)를 통한 깨움도
//! 일부러 연 채널이라 주장 밖입니다.
//!
//! # Examples
//! ```rust,ignore
//! let mut core: Core<u64> = Core::new();
//! core.start(&mut env, first);
//! core.set_table(table);
//! let next = core.tick(&mut env);
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

use k0_slice::{Entry, Slice, Verdict};

pub mod clock;

#[cfg(test)]
mod clock_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod tests;

/// 도메인의 최대 개수
pub const MAX_DOMAINS: usize = 8;

/// 표 항목의 최대 개수
pub const MAX_ENTRIES: usize = 16;

/// 창 하나의 최대 길이(틱)
pub const MAX_WINDOW: u32 = 255;

/// 태스크가 속하는 도메인의 번호를 담는 구조체입니다.
///
/// 값은 항상 [MAX_DOMAINS]보다 작습니다. 0번은 루트 태스크의 도메인입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Domain(u8);

impl Domain {
    /// 부팅 때 루트 태스크가 속하는 도메인
    pub const ROOT: Domain = Domain(0);

    /// 번호가 범위 안일 때만 도메인을 만드는 함수입니다.
    ///
    /// # Arguments
    /// `id` - 도메인 번호(0 이상 [MAX_DOMAINS] 미만)
    pub const fn new(id: u8) -> Option<Domain> {
        if (id as usize) < MAX_DOMAINS { Some(Domain(id)) } else { None }
    }

    /// 도메인 번호를 주는 함수입니다.
    pub const fn id(self) -> u8 {
        self.0
    }

    /// 도메인별 배열의 첨자로 쓸 값을 주는 함수입니다.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// 표를 읽거나 만들 때 거절된 까닭을 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableError {
    /// 항목 수가 0이거나 [MAX_ENTRIES]보다 큼
    Count,
    /// 창의 길이가 0이거나 [MAX_WINDOW]보다 큼
    Window,
    /// 도메인 번호가 [MAX_DOMAINS] 이상임
    Domain,
    /// 쓰지 않는 비트가 0이 아님
    Reserved,
}

/// 도메인이 도는 순서와 길이를 담은 되풀이 표를 나타내는 구조체입니다.
///
/// 항목은 (도메인, 틱 수)이고 표는 0번 항목부터 차례로 돌다가 끝나면 처음으로
/// 돌아갑니다. 만들 때 모든 항목이 검증되므로 이 타입의 값은 언제나 올바릅니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Table {
    dom: [u8; MAX_ENTRIES],
    ticks: [u8; MAX_ENTRIES],
    len: u8,
    cycle: u16,
}

impl Table {
    /// 도메인 0 하나가 1틱씩 도는 기본 표입니다. 오늘의 스케줄러와 같습니다.
    pub const DEFAULT: Table = Table {
        dom: [0; MAX_ENTRIES],
        ticks: {
            let mut t = [0; MAX_ENTRIES];
            t[0] = 1;
            t
        },
        len: 1,
        cycle: 1,
    };

    /// 항목 목록에서 표를 만드는 함수입니다.
    ///
    /// # Arguments
    /// `entries` - (도메인, 틱 수) 목록
    ///
    /// # Errors
    /// 항목 수가 0이거나 [MAX_ENTRIES]보다 크면 [TableError::Count], 틱 수가 0이거나
    /// [MAX_WINDOW]보다 크면 [TableError::Window]입니다.
    pub fn from_entries(entries: &[(Domain, u32)]) -> Result<Table, TableError> {
        if entries.is_empty() || entries.len() > MAX_ENTRIES {
            return Err(TableError::Count);
        }
        let mut t = Table { dom: [0; MAX_ENTRIES], ticks: [0; MAX_ENTRIES], len: 0, cycle: 0 };
        for (i, &(d, w)) in entries.iter().enumerate() {
            if w == 0 || w > MAX_WINDOW {
                return Err(TableError::Window);
            }
            t.dom[i] = d.0;
            t.ticks[i] = w as u8;
            t.cycle += w as u16;
        }
        t.len = entries.len() as u8;
        Ok(t)
    }

    /// 시스템 콜의 레지스터에서 표를 읽는 함수입니다.
    ///
    /// 항목 i는 `regs[i / 4]`의 `16 * (i % 4)`번째 비트에서 시작하는 16비트이고
    /// 값은 `(틱 수 << 8) | 도메인`입니다. 틱 수는 1 이상 255 이하, 도메인은 0
    /// 이상 7 이하이며 `count` 뒤의 칸은 모두 0이어야 합니다.
    ///
    /// # Arguments
    /// `count` - 항목 수(x1)
    /// `regs` - 항목이 든 레지스터 x2부터 x5
    ///
    /// # Errors
    /// 항목 수가 범위 밖이면 [TableError::Count], 그다음 순서대로 항목마다 도메인이
    /// 범위 밖이면 [TableError::Domain], 틱 수가 0이면 [TableError::Window], 마지막으로
    /// `count` 뒤의 칸이 0이 아니면 [TableError::Reserved]입니다.
    pub fn decode(count: u64, regs: [u64; 4]) -> Result<Table, TableError> {
        if count == 0 || count > MAX_ENTRIES as u64 {
            return Err(TableError::Count);
        }
        let slot = |i: usize| ((regs[i / 4] >> (16 * (i % 4))) & 0xffff) as u16;
        let count = count as usize;
        let mut t = Table { dom: [0; MAX_ENTRIES], ticks: [0; MAX_ENTRIES], len: count as u8, cycle: 0 };
        for i in 0..count {
            let v = slot(i);
            let (d, w) = ((v & 0xff) as u8, (v >> 8) as u8);
            if d as usize >= MAX_DOMAINS {
                return Err(TableError::Domain);
            }
            if w == 0 {
                return Err(TableError::Window);
            }
            t.dom[i] = d;
            t.ticks[i] = w;
            t.cycle += w as u16;
        }
        for i in count..MAX_ENTRIES {
            if slot(i) != 0 {
                return Err(TableError::Reserved);
            }
        }
        Ok(t)
    }

    /// 표에 그 도메인의 창이 하나라도 있는지 알려 주는 함수입니다.
    pub fn contains(&self, d: Domain) -> bool {
        self.dom[..self.len as usize].contains(&d.0)
    }

    /// 표에 루트 도메인밖에 없는지 알려 주는 함수입니다. 그런 표는 창의 길이와
    /// 상관없이 오늘의 스케줄러와 똑같이 움직입니다.
    pub fn is_default(&self) -> bool {
        self.dom[..self.len as usize].iter().all(|&d| d == 0)
    }

    /// 항목 수를 주는 함수입니다.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// 항목이 없는지 알려 주는 함수입니다. 올바른 표에서는 언제나 false입니다.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// i번 항목을 주는 함수입니다.
    ///
    /// # Arguments
    /// `i` - 항목 번호
    pub fn entry(&self, i: usize) -> Option<(Domain, u32)> {
        if i < self.len as usize { Some((Domain(self.dom[i]), self.ticks[i] as u32)) } else { None }
    }

    /// 표 한 바퀴의 틱 수를 주는 함수입니다.
    pub fn cycle(&self) -> u32 {
        self.cycle as u32
    }

    /// 표가 시작한 틱에서 `offset`틱 뒤에 도는 도메인을 주는 함수입니다.
    ///
    /// # Arguments
    /// `offset` - 표의 0번 항목이 시작한 틱부터 센 틱 수
    pub fn domain_at(&self, offset: u64) -> Domain {
        let mut r = offset % self.cycle as u64;
        for i in 0..self.len as usize {
            if r < self.ticks[i] as u64 {
                return Domain(self.dom[i]);
            }
            r -= self.ticks[i] as u64;
        }
        Domain::ROOT
    }
}

/// 스케줄러가 커널에 요구하는 동작을 모은 트레이트입니다.
///
/// 준비 큐는 도메인마다 하나이고 이 크레이트는 큐의 내용을 모릅니다. 태스크는
/// `T`로 표현되며 도메인은 태스크 자신(TCB)에서 읽습니다.
pub trait Env<T: Copy + Eq> {
    /// 지금까지 센 틱 번호를 주는 함수입니다.
    fn now(&self) -> u64;
    /// 태스크가 속한 도메인을 주는 함수입니다.
    fn domain(&self, t: T) -> Domain;
    /// 태스크를 Ready로 두고 도메인 큐의 뒤에 넣는 함수입니다.
    fn push(&mut self, d: Domain, t: T);
    /// 태스크를 Ready로 두고 도메인 큐의 앞에 넣는 함수입니다.
    fn push_front(&mut self, d: Domain, t: T);
    /// 도메인 큐의 앞에서 태스크를 꺼내는 함수입니다.
    fn pop(&mut self, d: Domain) -> Option<T>;
    /// 태스크를 Running으로 두고 현재 태스크, 문맥, 주소 공간을 바꾸는 함수입니다.
    fn run(&mut self, t: T);
    /// 창의 경계에서 새 도메인에 들어갈 때 부르는 함수입니다(TLB 비우기).
    fn enter_domain(&mut self, d: Domain);
    /// 그 도메인의 일만 하는 함수입니다. 콘솔 입력으로 깨울 태스크를 먼저, 그다음
    /// 기한이 지난 대기자를 깨웁니다. 다른 도메인에는 손대지 않습니다.
    fn serve(&mut self, d: Domain, now: u64);
}

/// 사건을 처리한 뒤 CPU가 할 일을 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next<T> {
    /// 이 태스크가 실행 중이거나 막 실행됨
    Run(T),
    /// 실행할 태스크가 없으니 인터럽트를 기다림
    Idle,
}

/// 도메인 표, 도는 도메인, 현재 태스크, 틱 규칙의 상태를 담은 구조체입니다.
///
/// 사건 함수 말고는 상태를 바꾸는 길이 없습니다. 커널은 CPU를 받은 경위(틱의
/// 판정인지 틱 사이인지)를 직접 고르지 않고 사건의 이름으로 알립니다.
pub struct Core<T> {
    table: Table,
    epoch: u64,
    pending: Option<(Table, u64)>,
    seen: u64,
    rd: Domain,
    running: Option<T>,
    slice: Slice,
}

impl<T: Copy + Eq> Core<T> {
    /// 기본 표와 빈 상태로 시작하는 함수입니다.
    pub const fn new() -> Self {
        Self {
            table: Table::DEFAULT,
            epoch: 0,
            pending: None,
            seen: 0,
            rd: Domain::ROOT,
            running: None,
            slice: Slice::new(),
        }
    }

    /// 틱 `k`에서 도는 도메인을 주는 함수입니다. 상태는 바꾸지 않습니다.
    fn plan_at(&self, k: u64) -> Domain {
        if let Some((t, s)) = &self.pending
            && k >= *s
        {
            return t.domain_at(k - *s);
        }
        self.table.domain_at(k.saturating_sub(self.epoch))
    }

    /// 시작 틱이 `n` 이하인 보류 표를 표로 올리는 함수입니다.
    fn advance(&mut self, n: u64) {
        if let Some((t, s)) = self.pending
            && s <= n
        {
            self.table = t;
            self.epoch = s;
            self.pending = None;
        }
    }

    fn status(&self) -> Next<T> {
        match self.running {
            Some(c) => Next::Run(c),
            None => Next::Idle,
        }
    }

    /// 현재 태스크를 바꾸는 유일한 자리입니다. 경위를 인자로 받아 면제를 대입합니다.
    fn switch<E: Env<T>>(&mut self, env: &mut E, x: T, how: Entry) {
        self.slice.entered(how);
        self.running = Some(x);
        env.run(x);
    }

    /// 도메인 `d`의 큐 앞을 틱의 판정으로 들여보내는 함수입니다.
    fn enter<E: Env<T>>(&mut self, env: &mut E, d: Domain) -> Next<T> {
        match env.pop(d) {
            Some(x) => {
                self.switch(env, x, Entry::Tick);
                Next::Run(x)
            }
            None => Next::Idle,
        }
    }

    /// 루트 태스크로 제어권을 넘길 때 부르는 함수입니다.
    ///
    /// 루트 태스크는 주기의 중간에 들어오므로 첫 틱 하나를 넘깁니다. 표는 기본
    /// 표이고 도는 도메인은 루트입니다.
    ///
    /// # Arguments
    /// `first` - 첫 태스크
    pub fn start<E: Env<T>>(&mut self, env: &mut E, first: T) {
        *self = Self::new();
        self.seen = env.now();
        self.epoch = self.seen;
        self.switch(env, first, Entry::Between);
    }

    /// 현재 태스크가 CPU를 내놓을 때(YIELD) 부르는 함수입니다.
    ///
    /// 같은 도메인에 준비된 다른 태스크가 있으면 현재 태스크를 큐 뒤에 세우고
    /// 큐 앞을 틱 사이의 전환으로 실행합니다. 없으면 아무 일도 없고 면제도
    /// 새로 생기지 않습니다.
    pub fn yield_now<E: Env<T>>(&mut self, env: &mut E) {
        let Some(c) = self.running else { return };
        if let Some(x) = env.pop(self.rd) {
            env.push(self.rd, c);
            self.switch(env, x, Entry::Between);
        }
    }

    /// 현재 태스크가 블록되거나 종료되거나 폴트로 떠난 뒤에 부르는 함수입니다.
    ///
    /// 떠난 태스크는 호출자가 이미 깨울 주체에 걸어 두었어야 합니다. 같은
    /// 도메인에 준비된 태스크가 있으면 틱 사이의 전환으로 실행하고, 없으면
    /// [Next::Idle]입니다(다른 도메인으로 넘어가지 않음).
    pub fn block<E: Env<T>>(&mut self, env: &mut E) -> Next<T> {
        self.running = None;
        match env.pop(self.rd) {
            Some(x) => {
                self.switch(env, x, Entry::Between);
                Next::Run(x)
            }
            None => Next::Idle,
        }
    }

    /// EL0에서 틱이 들어왔을 때 부르는 함수입니다.
    ///
    /// 틱 번호가 마지막으로 처리한 것보다 크지 않으면 새로운 일이 없으므로 현재
    /// 상태만 돌려줍니다. 번호가 여럿 건너뛰어도 사건은 한 번입니다.
    pub fn tick<E: Env<T>>(&mut self, env: &mut E) -> Next<T> {
        let n = env.now();
        if n <= self.seen {
            return self.status();
        }
        self.advance(n);
        let old = self.rd;
        let new = self.table.domain_at(n - self.epoch);
        self.seen = n;
        if new == old {
            env.serve(old, n);
            return match self.running {
                Some(c) => match self.slice.on_tick() {
                    Verdict::Keep => Next::Run(c),
                    Verdict::Rotate => match env.pop(old) {
                        Some(x) => {
                            env.push(old, c);
                            self.switch(env, x, Entry::Tick);
                            Next::Run(x)
                        }
                        None => Next::Run(c),
                    },
                },
                None => self.enter(env, old),
            };
        }
        if let Some(c) = self.running.take() {
            match self.slice.on_tick() {
                Verdict::Keep => env.push_front(old, c),
                Verdict::Rotate => match env.pop(old) {
                    Some(x) => {
                        env.push(old, c);
                        env.push_front(old, x);
                    }
                    None => env.push_front(old, c),
                },
            }
        }
        self.rd = new;
        env.enter_domain(new);
        env.serve(new, n);
        self.enter(env, new)
    }

    /// 실행할 태스크 없이 기다리는 루프가 인터럽트를 받은 뒤에 부르는 함수입니다.
    ///
    /// 틱 번호가 새로 올랐으면 틱 사건과 같습니다. 아니면(콘솔 입력) 도는
    /// 도메인의 일을 하고, 준비된 태스크가 생겼으면 틱 사이의 전환으로
    /// 실행합니다.
    pub fn poll<E: Env<T>>(&mut self, env: &mut E) -> Next<T> {
        let n = env.now();
        if n > self.seen {
            return self.tick(env);
        }
        env.serve(self.rd, n);
        if self.running.is_some() {
            return self.status();
        }
        match env.pop(self.rd) {
            Some(x) => {
                self.switch(env, x, Entry::Between);
                Next::Run(x)
            }
            None => Next::Idle,
        }
    }

    /// 새 표를 보류시키는 함수입니다. 마지막으로 처리한 틱의 두 틱 뒤에 0번
    /// 항목부터 효력이 생깁니다.
    ///
    /// 그 전에 한 번 더 부르면 앞의 표는 버려집니다. 이미 효력이 생긴 표는
    /// 새 표의 시작 틱까지 그대로입니다. 시작 틱은 이 구조체가 처리한 틱에서
    /// 구하므로 부르는 쪽이 틱 번호를 넘길 일이 없습니다. 바로 다음 틱은 언제나
    /// 지금의 계획대로 가므로, 그 틱을 일찍 받도록 이미 걸어 둔 타이머는 이
    /// 호출이 보류 표를 갈아 끼울 때만 달라집니다(부른 쪽이 호출 뒤에
    /// [Core::next_tick_switches]를 다시 물어야 함).
    ///
    /// # Arguments
    /// `t` - 새 표
    pub fn set_table(&mut self, t: Table) {
        self.pending = Some((t, self.seen.saturating_add(2)));
    }

    /// 마지막으로 처리한 틱 다음 틱에서 도메인이 바뀌는지 알려 주는 함수입니다.
    ///
    /// 커널이 그 틱을 일찍 받도록 타이머를 앞당길지 정하는 데 씁니다.
    pub fn next_tick_switches(&self) -> bool {
        self.plan_at(self.seen.saturating_add(1)) != self.rd
    }

    /// 지금 도는 도메인을 주는 함수입니다.
    pub fn running_domain(&self) -> Domain {
        self.rd
    }

    /// 표나 보류 표가 기본 표가 아닌지 알려 주는 함수입니다.
    pub fn partitioned(&self) -> bool {
        !self.table.is_default() || self.pending.is_some_and(|(t, _)| !t.is_default())
    }
}

impl<T: Copy + Eq> Default for Core<T> {
    fn default() -> Self {
        Self::new()
    }
}
