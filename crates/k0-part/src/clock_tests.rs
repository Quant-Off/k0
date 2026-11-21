//! 틱 시각의 산술([crate::clock])을 기준과 견주어 검증하는 테스트 모듈입니다.
//!
//! 기준은 clock.rs와 따로 적었습니다. clock.rs는 u64의 포화 연산으로 적었고 기준은
//! u128 위에서 넘침을 `min`으로 자르는 방식으로 적었으므로 같은 줄을 베낀 것이
//! 아닙니다. 작은 수는 격자로 전부 돌리고 큰 수는 무작위와 경계값 격자로 돌립니다.
//!
//! 속성은 함수로 받습니다. 진짜 [fire]와 일부러 틀리게 만든 사본([Break])이 같은
//! 속성 함수를 거치므로, 사본이 걸리는지는 `tests::deliberate_breaks_are_caught`가
//! 확인합니다.
//!
//! 모형이 다루지 않는 것은 카운터 레지스터를 읽고 쓰는 일과 인터럽트가 실제로
//! 언제 오는지입니다. 여기서는 카운터 값이 순서대로 주어진다고만 봅니다.

use crate::clock::{Fire, due, fire, guard, reached};

const MAX: u64 = u64::MAX;

/// 카운터 값 5개를 받아 틱을 판정하는 함수의 모양입니다. `(now, t0, period, last, early)`
type FireFn<'a> = &'a dyn Fn(u64, u64, u64, u64, u64) -> Fire;

/// 일부러 틀리게 만든 판정입니다. 한 번에 하나만 틀립니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Break {
    /// 틱 번호를 카운터가 아니라 마지막 번호 + 1로 정함
    LastPlusOne,
    /// 비교값보다 이른 인터럽트도 틱으로 침
    SpuriousIgnored,
    /// 일찍 건 만큼의 앞당김을 무시함
    EarlyIgnored,
    /// 다음 틱의 시각이 카운터 범위를 넘어도 최댓값으로 잘라 틱으로 침
    SaturatedRangeTicks,
    /// 번호가 더 오를 수 없어도(last가 최댓값) 틱으로 침
    LastMaxTicks,
}

pub(crate) const ALL_BREAKS: [Break; 5] = [
    Break::LastPlusOne,
    Break::SpuriousIgnored,
    Break::EarlyIgnored,
    Break::SaturatedRangeTicks,
    Break::LastMaxTicks,
];

fn broken(v: Break, now: u64, t0: u64, period: u64, last: u64, early: u64) -> Fire {
    let next = last.saturating_add(1);
    if v != Break::LastMaxTicks && last == MAX {
        return Fire::Spurious;
    }
    if v != Break::SaturatedRangeTicks && o_due_exact(t0, period, next).is_none() {
        return Fire::Spurious;
    }
    let by_counter = if period == 0 { 0 } else { now.saturating_sub(t0) / period };
    let lo = match v {
        Break::EarlyIgnored => due(t0, period, next),
        _ => due(t0, period, next).saturating_sub(early),
    };
    if v != Break::SpuriousIgnored && (period == 0 || now < lo) {
        return Fire::Spurious;
    }
    match v {
        Break::LastPlusOne => Fire::Tick(next),
        _ => Fire::Tick(by_counter.max(next)),
    }
}

/// 진짜 [fire]와 사본이 속성 함수에 같은 모양으로 들어가게 하는 함수입니다.
pub(crate) fn caught(v: Break) -> bool {
    properties(&|a, b, c, d, e| broken(v, a, b, c, d, e), true).is_err()
}

/// 기준의 틱 `n` 예정 시각입니다. 넘치면 u64의 최댓값에서 멈춥니다.
fn o_due(t0: u64, p: u64, n: u64) -> u64 {
    let v = t0 as u128 + n as u128 * p as u128;
    v.min(MAX as u128) as u64
}

/// 기준의 틱 `n` 예정 시각입니다. 카운터의 범위를 넘으면 None입니다.
fn o_due_exact(t0: u64, p: u64, n: u64) -> Option<u64> {
    let v = t0 as u128 + n as u128 * p as u128;
    if v > MAX as u128 { None } else { Some(v as u64) }
}

/// 기준의 판정입니다. 주기가 0이거나 번호가 더 오를 수 없거나 다음 틱의 시각이
/// 카운터의 범위를 넘으면 틱이 없으므로 Spurious입니다.
fn o_fire(now: u64, t0: u64, p: u64, last: u64, early: u64) -> Fire {
    if p == 0 || last == MAX {
        return Fire::Spurious;
    }
    let next = last + 1;
    let Some(due_next) = o_due_exact(t0, p, next) else {
        return Fire::Spurious;
    };
    let armed = due_next.saturating_sub(early);
    if now < armed {
        return Fire::Spurious;
    }
    let by = if now >= t0 { (now - t0) / p } else { 0 };
    Fire::Tick(by.max(next))
}

/// 작은 수의 격자로 기준과 견줍니다.
fn grid(f: FireFn) -> Result<(), String> {
    for t0 in 0..6u64 {
        for p in 0..7u64 {
            for last in 0..7u64 {
                for early in 0..8u64 {
                    for now in 0..60u64 {
                        let (got, want) = (f(now, t0, p, last, early), o_fire(now, t0, p, last, early));
                        if got != want {
                            return Err(format!(
                                "now {now} t0 {t0} period {p} last {last} early {early}: got {got:?} want {want:?}"
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// 걸어 둔 시각의 바로 앞과 바로 뒤에서 Spurious가 갈립니다.
fn armed_edge(f: FireFn) -> Result<(), String> {
    for t0 in [0u64, 3, 1000] {
        for p in 1..=50u64 {
            for last in [0u64, 1, 5, 40] {
                for early in [0u64, 1, p / 40, p / 2, p - 1, p] {
                    let armed = o_due(t0, p, last + 1).saturating_sub(early);
                    if armed > 0 && f(armed - 1, t0, p, last, early) != Fire::Spurious {
                        return Err(format!("one count before the armed instant: t0 {t0} p {p} last {last} early {early}"));
                    }
                    if matches!(f(armed, t0, p, last, early), Fire::Spurious) {
                        return Err(format!("at the armed instant: t0 {t0} p {p} last {last} early {early}"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// 처리가 3.5주기 늦으면 카운터가 가리키는 번호에 내립니다.
fn late(f: FireFn) -> Result<(), String> {
    for t0 in [0u64, 7, 1_000_000] {
        for p in 2..=40u64 {
            for last in [0u64, 1, 9, 100] {
                let next = last + 1;
                let now = o_due(t0, p, next) + 7 * p / 2;
                if f(now, t0, p, last, 0) != Fire::Tick(next + 3) {
                    return Err(format!("3.5 periods late: t0 {t0} p {p} last {last} got {:?}", f(now, t0, p, last, 0)));
                }
                // 정확히 3주기 늦거나 4주기 직전
                if f(o_due(t0, p, next) + 3 * p, t0, p, last, 0) != Fire::Tick(next + 3) {
                    return Err(format!("3 periods late: t0 {t0} p {p} last {last}"));
                }
                if f(o_due(t0, p, next) + 4 * p - 1, t0, p, last, 0) != Fire::Tick(next + 3) {
                    return Err(format!("just before 4 periods late: t0 {t0} p {p} last {last}"));
                }
            }
        }
    }
    Ok(())
}

/// 값이 u64의 끝에 가까운 곳의 경계 격자입니다. 패닉 없이 기준과 같아야 합니다.
fn near_max(f: FireFn) -> Result<(), String> {
    let vals = |extra: &[u64]| {
        let mut v = vec![0u64, 1, 2, 3, 39, 40, 41, 1000, MAX / 40, MAX / 3, MAX / 2, MAX - 3, MAX - 2, MAX - 1, MAX];
        v.extend_from_slice(extra);
        v
    };
    for &t0 in &vals(&[]) {
        for &p in &vals(&[1 << 32, 1 << 40, 24_000_000]) {
            for &last in &vals(&[MAX / p.max(1), (MAX / p.max(1)).saturating_sub(1)]) {
                for &early in &[0u64, 1, guard(p), p / 2, MAX] {
                    for &now in &vals(&[t0.saturating_add(p), t0.saturating_sub(1)]) {
                        let (got, want) = (f(now, t0, p, last, early), o_fire(now, t0, p, last, early));
                        if got != want {
                            return Err(format!(
                                "now {now} t0 {t0} period {p} last {last} early {early}: got {got:?} want {want:?}"
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 크기를 가리지 않고 고르게 나오도록 비트 수를 먼저 정한 수입니다.
    fn wide(&mut self) -> u64 {
        match self.next() % 8 {
            0 => MAX - self.next() % 8,
            1 => self.next() % 8,
            _ => self.next() >> (self.next() % 64),
        }
    }
}

/// 큰 수를 무작위로 기준과 견줍니다.
fn random(f: FireFn, n: u64) -> Result<(), String> {
    let mut r = Rng(0x1234_5678);
    for _ in 0..n {
        let (now, t0, p, last, early) = (r.wide(), r.wide(), r.wide(), r.wide(), r.wide());
        // 걸어 둔 시각 근처의 값도 만든다
        let near = o_due(t0, p, last.saturating_add(1)).saturating_sub(early);
        for now in [now, near.saturating_sub(1), near, near.saturating_add(1), near.saturating_add(r.wide() % 1000)] {
            let (got, want) = (f(now, t0, p, last, early), o_fire(now, t0, p, last, early));
            if got != want {
                return Err(format!("now {now} t0 {t0} period {p} last {last} early {early}: got {got:?} want {want:?}"));
            }
        }
    }
    Ok(())
}

/// 카운터가 줄지 않는 열에 처리기가 하는 대로(틱이면 마지막 번호를 갱신하고 앞당김을
/// 없앰, 보호 구간이 필요하면 다시 거는 것을 흉내 냄) 적용해 번호의 성질을 봅니다.
fn drive(f: FireFn, n: u64) -> Result<(), String> {
    let mut r = Rng(0xFEED);
    for case in 0..n {
        let p = 1 + r.next() % 1000;
        let t0 = r.next() % 5000;
        let mut last = r.next() % 4;
        let mut early = 0u64;
        let mut now = t0 + r.next() % (3 * p);
        let mut seen: Vec<u64> = Vec::new();
        for _ in 0..200 {
            let step = match r.next() % 6 {
                0 => 0,
                1 => 1 + r.next() % 3,
                2 => p / 2,
                3 => p,
                4 => p + p / 2 + r.next() % p,
                _ => 3 * p + p / 2,
            };
            now = now.saturating_add(step);
            match f(now, t0, p, last, early) {
                Fire::Spurious => {
                    // 이른 인터럽트: 걸어 둔 시각보다 정말로 이르다
                    let armed = o_due_exact(t0, p, last + 1).map(|d| d.saturating_sub(early));
                    if let Some(armed) = armed
                        && now >= armed
                    {
                        return Err(format!("case {case}: Spurious at {now} but armed at {armed}"));
                    }
                }
                Fire::Tick(k) => {
                    if k <= last {
                        return Err(format!("case {case}: index {k} not after {last}"));
                    }
                    if seen.contains(&k) {
                        return Err(format!("case {case}: index {k} repeated"));
                    }
                    let by_counter = (now - t0) / p;
                    if k < by_counter {
                        return Err(format!("case {case}: index {k} but the counter says {by_counter} at {now}"));
                    }
                    if k > last + 1 && k != by_counter {
                        return Err(format!("case {case}: jump to {k} but the counter says {by_counter}"));
                    }
                    // 번호가 예정 시각(앞당김만큼 일찍은 허용)보다 이르게 처리되지 않았다
                    if now < o_due(t0, p, k).saturating_sub(early) {
                        return Err(format!("case {case}: index {k} taken at {now} before its time"));
                    }
                    seen.push(k);
                    last = k;
                    early = if r.next() % 2 == 0 { guard(p) } else { 0 };
                }
            }
        }
    }
    Ok(())
}

/// 카운터가 u64의 끝까지 오르는 열에서 번호의 성질과 기준과의 일치를 봅니다.
///
/// 틱의 번호는 마지막 번호보다 언제나 크고 되풀이되지 않으며, 번호가 최댓값에
/// 닿은 뒤에는 틱이 다시 오지 않습니다. 틱으로 친 번호의 예정 시각은 카운터의
/// 범위 안에 있습니다.
fn drive_top(f: FireFn) -> Result<(), String> {
    let mut r = Rng(0xA11CE);
    let periods = [1u64, 2, 3, 40, 1 << 20, 1 << 32, 1 << 40, MAX / 3, MAX / 2, MAX];
    let origins = [0u64, 1, 1000, MAX / 2, MAX - 1000, MAX - 5, MAX];
    for case in 0..30_000u64 {
        let p = periods[(r.next() % periods.len() as u64) as usize];
        let t0 = origins[(r.next() % origins.len() as u64) as usize];
        let top = MAX / p;
        let starts = [0, 5, top.saturating_sub(3), top.saturating_sub(1), top, top.saturating_add(1), MAX - 3, MAX - 1];
        let mut last = starts[(r.next() % starts.len() as u64) as usize];
        let mut early = 0u64;
        let mut now = match r.next() % 3 {
            0 => 0,
            1 => t0,
            _ => t0.saturating_add(r.next() % (3 * p.min(1 << 61))),
        };
        let mut seen: Vec<u64> = Vec::new();
        for i in 0..40 {
            let step = match r.next() % 7 {
                0 => 0,
                1 => 1,
                2 => p,
                3 => p.saturating_add(1),
                4 => r.wide(),
                5 => MAX / 2,
                _ => MAX,
            };
            now = now.saturating_add(step);
            if i == 39 {
                now = MAX;
            }
            let got = f(now, t0, p, last, early);
            let want = o_fire(now, t0, p, last, early);
            if got != want {
                return Err(format!(
                    "case {case}: now {now} t0 {t0} period {p} last {last} early {early}: got {got:?} want {want:?}"
                ));
            }
            if let Fire::Tick(k) = got {
                if last == MAX {
                    return Err(format!("case {case}: a tick after the index reached the top"));
                }
                if k <= last || seen.contains(&k) {
                    return Err(format!("case {case}: index {k} after {last} goes back or repeats"));
                }
                if o_due_exact(t0, p, k).is_none() {
                    return Err(format!("case {case}: index {k} has no due instant in the counter range"));
                }
                seen.push(k);
                last = k;
                early = if r.next() % 2 == 0 { guard(p) } else { 0 };
            }
        }
    }
    Ok(())
}

/// 모든 속성입니다. 가벼운 쪽(`quick`)은 깨뜨리기 시험이 씁니다.
pub(crate) fn properties(f: FireFn, quick: bool) -> Result<(), String> {
    grid(f).map_err(|e| format!("grid: {e}"))?;
    armed_edge(f).map_err(|e| format!("armed edge: {e}"))?;
    late(f).map_err(|e| format!("late: {e}"))?;
    near_max(f).map_err(|e| format!("near max: {e}"))?;
    random(f, if quick { 20_000 } else { 400_000 }).map_err(|e| format!("random: {e}"))?;
    drive(f, if quick { 200 } else { 3000 }).map_err(|e| format!("sequence: {e}"))?;
    drive_top(f).map_err(|e| format!("sequence to the top: {e}"))?;
    Ok(())
}

#[test]
fn fire_matches_the_independent_oracle() {
    properties(&fire, false).unwrap();
}

#[test]
fn a_copy_without_a_break_passes_and_each_break_fails() {
    properties(&|a, b, c, d, e| fire(a, b, c, d, e), true).unwrap();
    for v in ALL_BREAKS {
        let r = properties(&|a, b, c, d, e| broken(v, a, b, c, d, e), true);
        println!("{v:?}: {r:?}");
        assert!(r.is_err(), "{v:?} passed the clock properties");
    }
}

#[test]
fn spurious_exactly_before_the_armed_instant() {
    for (t0, p, last, early) in [(0u64, 100u64, 0u64, 0u64), (0, 100, 0, 25), (50, 40, 7, 1), (50, 40, 7, 39)] {
        let armed = due(t0, p, last + 1) - early;
        for now in 0..armed {
            assert_eq!(fire(now, t0, p, last, early), Fire::Spurious, "now {now} armed {armed}");
        }
        assert_eq!(fire(armed, t0, p, last, early), Fire::Tick(last + 1));
        assert!(matches!(fire(armed + 1, t0, p, last, early), Fire::Tick(_)));
    }
}

#[test]
fn now_exactly_at_due_and_one_count_before_due_minus_early() {
    let (t0, p, last) = (1000u64, 600u64, 9u64);
    let d = due(t0, p, 10);
    assert_eq!(d, 7000);
    assert_eq!(fire(d, t0, p, last, 0), Fire::Tick(10));
    assert_eq!(fire(d - 1, t0, p, last, 0), Fire::Spurious);
    let g = guard(p);
    assert_eq!(g, 15);
    assert_eq!(fire(d - g, t0, p, last, g), Fire::Tick(10));
    assert_eq!(fire(d - g - 1, t0, p, last, g), Fire::Spurious);
    // 일찍 걸었어도 예정 시각 뒤의 인터럽트는 같은 번호의 틱이다
    assert_eq!(fire(d, t0, p, last, g), Fire::Tick(10));
    // 일찍 받은 틱의 번호는 카운터가 가리키는 이전 번호로 내려가지 않는다
    assert_eq!((d - g - t0) / p, 9);
    assert_eq!(fire(d - g, t0, p, last, g), Fire::Tick(10));
}

#[test]
fn period_zero_never_panics_and_never_ticks() {
    for t0 in [0u64, 1, MAX] {
        for last in [0u64, 5, MAX] {
            for early in [0u64, 1, MAX] {
                for now in [0u64, 1, 1000, MAX - 1, MAX] {
                    assert_eq!(fire(now, t0, 0, last, early), Fire::Spurious);
                }
            }
        }
    }
    assert_eq!(due(7, 0, 12345), 7);
    assert!(reached(7, 7, 0, 12345));
    assert!(!reached(6, 7, 0, 12345));
    assert_eq!(guard(0), 0);
}

#[test]
fn values_near_the_top_do_not_wrap_to_an_earlier_instant() {
    // 예정 시각은 번호에 대해 줄지 않고 t0보다 이르지 않다
    for t0 in [0u64, 1, MAX / 2, MAX - 5, MAX] {
        for p in [1u64, 2, 40, 1 << 32, MAX / 3, MAX / 2, MAX - 1, MAX] {
            let mut prev = t0;
            for n in [0u64, 1, 2, 3, 100, MAX / 2, MAX - 1, MAX] {
                let d = due(t0, p, n);
                assert!(d >= t0, "due below t0: t0 {t0} p {p} n {n}");
                assert_eq!(d, o_due(t0, p, n));
                assert!(d >= prev, "due went back: t0 {t0} p {p} n {n}");
                prev = d;
            }
        }
    }
    // 예정 시각은 범위를 넘으면 최댓값에 머문다
    assert_eq!(due(MAX - 1, 1 << 40, 5), MAX);
    assert_eq!(due(MAX, 1, 1), MAX);
    assert_eq!(due(0, MAX, 1), MAX);
    assert_eq!(due(0, MAX, 2), MAX);
    // 다음 틱의 시각이 범위를 넘으면 카운터가 최댓값이어도 틱이 아니다
    assert_eq!(fire(MAX - 1, MAX - 1, 1 << 40, 4, 0), Fire::Spurious);
    assert_eq!(fire(MAX, MAX, 1, 0, 0), Fire::Spurious);
    // 시각이 범위 안이면 앞당김이 아무리 커도 틱이다(비교값은 0으로 잘림)
    assert_eq!(fire(MAX, MAX - 1, 1, 0, MAX), Fire::Tick(1));
    // 결함 보고의 두 입력: 곱이 넘침, 번호가 더 오를 수 없음
    assert_eq!(fire(MAX, 0, 1 << 40, (1 << 24) - 1, 0), Fire::Spurious);
    assert_eq!(fire(MAX, 0, 1, MAX, 0), Fire::Spurious);
    // 곱이 범위 안에 마지막으로 드는 번호는 틱이다(2^24 - 1번의 시각은 2^64 - 2^40)
    assert_eq!(due(0, 1 << 40, (1 << 24) - 1), MAX - ((1 << 40) - 1));
    assert_eq!(fire(MAX, 0, 1 << 40, (1 << 24) - 2, 0), Fire::Tick((1 << 24) - 1));
    assert_eq!(fire(MAX - ((1 << 40) - 1) - 1, 0, 1 << 40, (1 << 24) - 2, 0), Fire::Spurious);
    // t0가 크면 합이 먼저 넘친다
    assert_eq!(fire(MAX, MAX, 1, 0, 0), Fire::Spurious);
    assert_eq!(fire(MAX - 1, MAX - 1, 1, 0, 0), Fire::Spurious);
    assert_eq!(fire(MAX, MAX - 1, 1, 0, 0), Fire::Tick(1));
    // 시각이 범위 끝에 닿는 번호는 틱이고, 그 번호 다음은 더 오를 수 없다
    assert_eq!(fire(MAX - 1, 0, 1, MAX - 2, 0), Fire::Tick(MAX - 1));
    assert_eq!(fire(MAX, 0, 1, MAX - 1, 0), Fire::Tick(MAX));
    assert_eq!(fire(MAX - 1, 0, 1, MAX - 1, 0), Fire::Spurious);
    for early in [0u64, 1, MAX] {
        assert_eq!(fire(MAX, 0, 1, MAX, early), Fire::Spurious);
        assert_eq!(fire(MAX, 7, 3, MAX, early), Fire::Spurious);
    }
    // 번호는 건너뛰어 끝에 닿을 수 있고, 그 뒤에는 틱이 없다
    assert_eq!(fire(MAX, 0, 1, 5, 0), Fire::Tick(MAX));
    assert_eq!(fire(MAX, 0, 1, MAX, 0), Fire::Spurious);
    // 큰 t0에서 now가 t0보다 이르면 번호는 다음 번호 이하로 내려가지 않는다
    assert_eq!(fire(0, MAX / 2, 1000, 3, MAX), Fire::Tick(4));
}

#[test]
fn reached_agrees_with_due() {
    for t0 in 0..5u64 {
        for p in 0..5u64 {
            for n in 0..6u64 {
                for now in 0..40u64 {
                    assert_eq!(reached(now, t0, p, n), now >= o_due(t0, p, n), "now {now} t0 {t0} p {p} n {n}");
                }
            }
        }
    }
    // 앞당김이 없을 때 fire가 틱이 되는 것은 다음 번호의 예정 시각에 닿은 것과 같다
    let mut r = Rng(99);
    for _ in 0..200_000 {
        let (now, t0, p, last) = (r.wide(), r.wide(), r.wide(), r.wide() % (MAX - 1));
        if p == 0 {
            continue;
        }
        let ticks = !matches!(fire(now, t0, p, last, 0), Fire::Spurious);
        // 시각이 카운터의 범위를 넘는 번호는 reached가 최댓값에서 참이어도 틱이 아니다
        let in_range = o_due_exact(t0, p, last + 1).is_some();
        assert_eq!(ticks, in_range && reached(now, t0, p, last + 1), "now {now} t0 {t0} p {p} last {last}");
        if !in_range {
            assert_eq!(due(t0, p, last + 1), MAX);
            assert_eq!(fire(MAX, t0, p, last, 0), Fire::Spurious);
        }
        let d = due(t0, p, last + 1);
        assert_eq!(reached(now, t0, p, last + 1), now >= d);
        if d > 0 {
            assert!(!reached(d - 1, t0, p, last + 1));
        }
        assert!(reached(d, t0, p, last + 1));
    }
}

#[test]
fn guard_is_a_fortieth_of_the_period() {
    for p in 0..=5000u64 {
        let g = guard(p);
        // 독립된 식 40g <= p < 40g + 40
        assert!(40 * g <= p && p < 40 * g + 40, "p {p} g {g}");
        if p > 0 {
            assert!(g < p);
        }
    }
    assert_eq!(guard(39), 0);
    assert_eq!(guard(40), 1);
    assert_eq!(guard(24_000_000), 600_000);
    assert_eq!(guard(MAX), MAX / 40);
    // 앞당긴 비교값은 앞 번호의 예정 시각보다 늦다(앞 번호와 헷갈리지 않음)
    for p in [40u64, 41, 1000, 24_000_000, 1 << 40] {
        for n in 1..4u64 {
            assert!(due(5, p, n + 1) - guard(p) > due(5, p, n));
        }
    }
}

#[test]
fn the_index_never_goes_back_or_repeats_over_any_counter_sequence() {
    drive(&fire, 6000).unwrap();
    // 작은 수의 모든 비감소 열이 아니라 모든 두 걸음 열을 격자로 본다
    for p in 1..=6u64 {
        for t0 in 0..4u64 {
            for last in 0..4u64 {
                for early in 0..=p {
                    for a in 0..40u64 {
                        for b in a..40u64 {
                            let mut l = last;
                            let mut e = early;
                            let mut prev: Option<u64> = None;
                            for now in [a, b] {
                                if let Fire::Tick(k) = fire(now, t0, p, l, e) {
                                    assert!(k > l);
                                    if let Some(q) = prev {
                                        assert!(k > q);
                                    }
                                    prev = Some(k);
                                    l = k;
                                    e = 0;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
