//! 틱 시각의 산술을 담는 모듈입니다.
//!
//! # Features
//! 틱 n의 예정 시각은 `t0 + n * period`이고 카운터만으로 정해집니다. 타이머
//! 인터럽트가 왔을 때 그것이 몇 번째 틱인지, 비교값보다 이른 인터럽트인지를
//! 여기서 판정합니다. 레지스터를 읽고 쓰는 일은 k0-arch가 하고 이 모듈은
//! 하드웨어에 닿지 않으므로 호스트에서 시험합니다.
//!
//! 도메인이 바뀌는 틱은 보호 구간([guard])만큼 일찍 인터럽트를 겁니다. 전환
//! 작업은 그 구간 안에서 끝내고, 예정 시각([due])이 될 때까지 기다렸다가
//! 다음 도메인을 시작합니다. 그래서 구간보다 짧은 커널 경로는 다음 도메인의
//! 시작 시각을 움직이지 못합니다.

/// 타이머 인터럽트 하나에 대한 판정입니다.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fire {
    /// 걸어 둔 시각보다 일찍 온 인터럽트. 틱이 아니므로 비교값만 다시 겁니다
    Spurious,
    /// 이 번호의 틱. 처리가 한 주기 넘게 늦었으면 번호가 여럿 건너뜁니다
    Tick(u64),
}

/// 보호 구간의 길이를 주는 함수입니다. 주기의 1/40입니다.
///
/// # Arguments
/// `period` - 틱 주기(카운터 단위)
pub const fn guard(period: u64) -> u64 {
    period / 40
}

/// 틱 `n`의 예정 시각을 카운터 값으로 주는 함수입니다.
///
/// 카운터의 범위를 넘으면 최댓값에 머뭅니다. 그런 틱은 [fire]가 틱으로 치지
/// 않으므로 앞당겨지는 일 없이 틱이 멈출 뿐입니다.
///
/// # Arguments
/// `t0` - 틱 0의 시각
/// `period` - 틱 주기(카운터 단위)
/// `n` - 틱 번호
pub const fn due(t0: u64, period: u64, n: u64) -> u64 {
    match due_in_range(t0, period, n) {
        Some(v) => v,
        None => u64::MAX,
    }
}

/// 틱 `n`의 예정 시각이 카운터의 범위 안에 있으면 그 값을 주는 함수입니다.
const fn due_in_range(t0: u64, period: u64, n: u64) -> Option<u64> {
    match n.checked_mul(period) {
        Some(d) => t0.checked_add(d),
        None => None,
    }
}

/// 타이머 인터럽트가 몇 번째 틱인지 판정하는 함수입니다.
///
/// 번호는 카운터가 정합니다. 다음 틱을 건 시각보다 이르면 틱이 아니고, 한
/// 주기 넘게 늦었으면 지나간 번호를 건너뛰어 카운터가 가리키는 번호를 줍니다.
///
/// # Arguments
/// `now` - 지금의 카운터 값
/// `t0` - 틱 0의 시각
/// `period` - 틱 주기(카운터 단위)
/// `last` - 마지막으로 처리한 틱의 번호
/// `early` - 다음 틱의 비교값을 예정 시각보다 얼마나 일찍 걸었는지(걸지
/// 않았으면 0)
pub const fn fire(now: u64, t0: u64, period: u64, last: u64, early: u64) -> Fire {
    // 번호가 더 오를 수 없거나 다음 틱의 시각이 카운터의 범위 밖이면 틱은 없음
    let Some(next) = last.checked_add(1) else {
        return Fire::Spurious;
    };
    let Some(armed) = due_in_range(t0, period, next) else {
        return Fire::Spurious;
    };
    if period == 0 || now < armed.saturating_sub(early) {
        return Fire::Spurious;
    }
    let by_counter = now.saturating_sub(t0) / period;
    if by_counter > next {
        Fire::Tick(by_counter)
    } else {
        Fire::Tick(next)
    }
}

/// 틱 `n`의 예정 시각이 지났는지 알려 주는 함수입니다.
///
/// # Arguments
/// `now` - 지금의 카운터 값
/// `t0` - 틱 0의 시각
/// `period` - 틱 주기(카운터 단위)
/// `n` - 틱 번호
pub const fn reached(now: u64, t0: u64, period: u64, n: u64) -> bool {
    now >= due(t0, period, n)
}
