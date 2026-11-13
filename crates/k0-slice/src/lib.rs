//! 타이머 틱이 현재 태스크를 끊을지 정하는 규칙을 담는 크레이트입니다.
//!
//! # Features
//! 준비 큐와 문맥 전환은 k0-sched에 있고, 이 크레이트는 판정 하나만 합니다.
//! "지금 온 틱이 현재 태스크를 끊는가"입니다. 하드웨어에도 스케줄러 상태에도
//! 닿지 않으므로 호스트 테스트가 가짜 준비 큐 위에서 같은 코드를 돌립니다.
//!
//! 규칙은 다음과 같습니다.
//!
//! - 틱의 판정으로 CPU를 받은 태스크([Entry::Tick])는 주기의 처음에 들어온
//!   것이므로 다음 틱에서 끊깁니다.
//! - 틱과 틱 사이에 CPU를 받은 태스크([Entry::Between])는 처음 맞는 틱 하나를
//!   넘깁니다. 앞선 태스크가 양보했거나, 블록됐거나, 종료됐거나, 커널이 대기
//!   중에 틱이 아닌 일로 깨어난 경우입니다. 그 태스크는 주기의 남은 조각만
//!   받고 들어왔으므로, 넘긴 틱부터 온전한 한 주기를 채운 뒤에 끊깁니다.
//!
//! 이 규칙이 없으면 틱 직전에 양보하는 태스크가 뒤에 선 태스크에게 주기의
//! 끝자락만 주고 다음 주기를 통째로 다시 받습니다. 규칙이 지키는 것은 "CPU를
//! 받은 태스크는 스스로 내놓지 않는 한 온전한 한 주기 전에는 끊기지 않는다"
//! 입니다. 그 결과 준비된 태스크가 N개일 때 계속 도는 태스크의 몫은 다른
//! 태스크가 양보와 블록을 어떻게 섞어도 1/N 아래로 내려가지 않습니다(테스트가
//! 무작위 일정과 격자 탐색으로 확인하며, 증명은 아님).
//!
//! 면제는 전환할 때마다 들어온 경위만으로 다시 정해집니다([Slice::entered]는
//! 이전 값에 기대지 않고 대입함). 그래서 전환 경로 하나가 표시를 빠뜨려 앞
//! 태스크의 면제가 다음 태스크에 남는 일이 없습니다.
//!
//! 막지 않는 것도 있습니다. 태스크는 양보하거나 블록하는 시점으로 다른
//! 태스크가 언제 얼마나 도는지를 여전히 바꿀 수 있으므로, 스케줄러를 통한
//! 타이밍 채널은 그대로입니다.
//!
//! # Examples
//! ```rust,ignore
//! let mut slice = Slice::new();
//! slice.entered(Entry::Between);
//! assert_eq!(slice.on_tick(), Verdict::Keep);
//! assert_eq!(slice.on_tick(), Verdict::Rotate);
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

/// 현재 태스크가 CPU를 받은 경위를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// 틱의 판정으로 받음(주기의 처음)
    Tick,
    /// 틱과 틱 사이에 받음(양보, 블록, 종료, 틱이 아닌 일로 끝난 대기)
    Between,
}

impl Entry {
    /// 커널이 실행할 태스크 없이 기다리다가 태스크를 들여보낼 때의 경위를
    /// 정하는 함수입니다.
    ///
    /// 기다리는 동안 틱이 지나갔으면 그 태스크는 틱 처리 직후에, 곧 주기의
    /// 처음에 들어오므로 면제가 없습니다. 틱 없이 깨어났으면(콘솔 입력) 주기
    /// 중간에 들어오는 것입니다. 둘이 함께 일어났으면 틱으로 봅니다.
    ///
    /// # Arguments
    /// `tick_seen` - 기다리는 동안 틱이 한 번이라도 지나갔는지
    pub const fn after_idle(tick_seen: bool) -> Self {
        if tick_seen { Self::Tick } else { Self::Between }
    }
}

/// 틱 하나에 대한 판정을 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 현재 태스크를 계속 실행함
    Keep,
    /// 준비된 태스크가 있으면 현재 태스크를 준비 큐의 끝으로 돌림
    Rotate,
}

/// 현재 태스크가 다음 틱 하나를 넘길 수 있는지를 담는 구조체입니다.
///
/// 스케줄러가 현재 태스크를 가리키는 값 옆에 하나만 둡니다. 태스크마다 두지
/// 않는 까닭은 이 값이 지금 실행 중인 태스크에 대해서만 뜻이 있기 때문입니다.
/// 값이 참이라는 것은 "현재 태스크가 틱의 판정이 아닌 전환으로 들어왔고 그
/// 뒤로 틱을 맞은 적이 없다"는 뜻입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slice {
    exempt: bool,
}

impl Slice {
    /// 면제가 없는 상태를 만드는 함수입니다.
    pub const fn new() -> Self {
        Self { exempt: false }
    }

    /// 현재 태스크가 바뀔 때마다 부르는 함수입니다.
    ///
    /// 앞 태스크가 남긴 값은 버리고 경위만으로 면제를 정합니다.
    ///
    /// # Arguments
    /// `how` - 새 현재 태스크가 CPU를 받은 경위
    pub fn entered(&mut self, how: Entry) {
        self.exempt = matches!(how, Entry::Between);
    }

    /// 현재 태스크가 실행 중일 때 온 틱 하나를 판정하는 함수입니다.
    ///
    /// 면제가 있으면 그것을 쓰고 [Verdict::Keep]을 줍니다. 면제는 한 번뿐이라
    /// 그다음 틱은 [Verdict::Rotate]입니다.
    pub fn on_tick(&mut self) -> Verdict {
        if self.exempt {
            self.exempt = false;
            Verdict::Keep
        } else {
            Verdict::Rotate
        }
    }
}

impl Default for Slice {
    fn default() -> Self {
        Self::new()
    }
}
