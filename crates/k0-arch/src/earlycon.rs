//! 부트 로그와 콘솔 출력 모듈입니다.
//!
//! # Features
//! 커널의 모든 출력은 두 곳으로 갑니다. 하나는 커널 메모리의 부트 로그이고,
//! 다른 하나는 DTB에서 찾아 검증을 마친 뒤 붙이는 콘솔 UART입니다. 콘솔을
//! 알기 전의 출력은 로그에만 쌓였다가 콘솔이 붙을 때 그대로 재생되므로, 범용
//! 이미지에서도 부팅 순서대로의 기록이 남습니다. 지원하는 UART가 없는
//! 보드에서는 로그가 유일한 기록입니다(디버거나 시험 하니스가 읽음).
//!
//! 콘솔 UART는 고른 빌드에만 들어갑니다. 드라이버는 feature `console-pl011`
//! (PL011과 SBSA UART)과 `console-16550`(16550 계열)으로 하나씩 고르고, 기본
//! 빌드에는 어느 것도 없습니다. 하나도 고르지 않은 커널에는 UART 레지스터에
//! 접근하는 코드가 없어서 기계 밖으로 나가는 출력이 없고, 로그만 남습니다.
//! 드라이버 코드는 하위 모듈 [uart]에 있습니다.
//!
//! 로그는 앞에서부터 채우고 가득 차면 버립니다(되감지 않음). EL0가 Console
//! 케이퍼빌리티로 내는 출력은 정해진 몫까지만 기록해, 사용자 출력이 커널의
//! 진단을 밀어내지 못하게 합니다. 태스크가 끝날 때 커널이 내는 줄(폴트, 종료,
//! 모르는 시스템 콜)에도 따로 몫을 두고([TaskLine]), 두 몫 모두 커널 진단용으로
//! 남긴 꼬리 앞에서 멈춥니다.
//!
//! # Examples
//! ```rust,ignore
//! use core::fmt::Write;
//! let mut con = k0_arch::earlycon::EarlyCon;
//! let _ = writeln!(con, "k0: hello");
//! ```

use core::arch::asm;
use core::cell::UnsafeCell;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(any(feature = "console-pl011", feature = "console-16550"))]
mod uart;

#[cfg(feature = "console-pl011")]
pub use uart::pl011_identity_ok;
#[cfg(any(feature = "console-pl011", feature = "console-16550"))]
pub use uart::{Uart, attach, attached};

#[cfg(any(feature = "console-pl011", feature = "console-16550"))]
use uart::hw_put;
#[cfg(all(feature = "console-pl011", feature = "console-input"))]
pub(crate) use uart::pl011_base;

/// 드라이버가 하나도 없는 빌드에서 UART 송신 자리를 채우는 함수입니다.
/// 아무것도 하지 않으며, 이 빌드에는 UART에 접근하는 코드가 없습니다.
#[cfg(not(any(feature = "console-pl011", feature = "console-16550")))]
#[inline(always)]
fn hw_put(_b: u8) {}

/// 이 커널에 들어 있는 콘솔 드라이버를 알리는 한 줄입니다.
///
/// 부팅 때 그대로 출력합니다. 줄 전체가 하나의 문자열로 이미지에 들어가므로,
/// 부팅하지 않고도 이미지 파일에서 이 줄을 찾아 어떤 입출력을 골라 빌드한
/// 이미지인지 알 수 있습니다(표지일 뿐 증거는 아님, 증거는 심볼과 부팅 시험).
#[cfg(all(feature = "console-pl011", feature = "console-16550"))]
pub const DRIVERS_LINE: &str = "k0: console drivers [pl011 16550]";
#[cfg(all(feature = "console-pl011", not(feature = "console-16550")))]
pub const DRIVERS_LINE: &str = "k0: console drivers [pl011]";
#[cfg(all(not(feature = "console-pl011"), feature = "console-16550"))]
pub const DRIVERS_LINE: &str = "k0: console drivers [16550]";
#[cfg(not(any(feature = "console-pl011", feature = "console-16550")))]
pub const DRIVERS_LINE: &str = "k0: console drivers [none]";

/// 부트 로그 본문의 크기 (2의 거듭제곱)
pub const LOG_SIZE: usize = 16 * 1024;

/// EL0 출력에 허용하는 로그 몫
const LOG_USER_CAP: usize = 6 * 1024;

/// 커널 진단 전용으로 남겨 두는 로그 꼬리. 사용자 출력은 이 앞에서 멈춤
const LOG_KERNEL_TAIL: usize = 2 * 1024;

/// 태스크가 일으킨 커널 줄(폴트, 종료, 모르는 시스템 콜)에 허용하는 로그 몫
const LOG_TASK_CAP: usize = 4 * 1024;

/// 그런 줄 하나가 로그에서 차지하는 길이의 상한(줄 끝 포함)
const TASK_LINE_MAX: usize = 128;

/// 태스크가 일으킨 줄이 지금까지 로그에 쓴 바이트 수. 하니스가 읽는 [BootLog]의
/// 배치를 바꾸지 않으려고 로그 밖에 둠
static TASK_LEN: AtomicUsize = AtomicUsize::new(0);

/// 그 몫을 넘겼다는 줄을 이미 남겼는지
static TASK_NOTED: AtomicBool = AtomicBool::new(false);

/// 로그 머리의 식별값. 시험 하니스가 메모리에서 로그를 찾는 표지
pub const LOG_MAGIC: u64 = 0x6B30_2D62_6F6F_746C;

/// 부트 로그입니다. 시험 하니스가 물리 메모리에서 읽으므로 배치를 고정합니다.
#[repr(C)]
pub struct BootLog {
    magic: u64,
    len: u64,
    user_len: u64,
    buf: [u8; LOG_SIZE],
}

#[repr(transparent)]
pub struct LogCell(UnsafeCell<BootLog>);

/// # Safety
/// 단일 코어에서만 접근하고, 모든 기록은 인터럽트를 마스크한 구간에서
/// 이루어지므로 동시 접근이 없습니다.
unsafe impl Sync for LogCell {}

/// 부트 로그의 실체. 이름이 곧 하니스와의 계약이라 바꾸면 안 됩니다.
#[unsafe(no_mangle)]
pub static K0_BOOT_LOG: LogCell = LogCell(UnsafeCell::new(BootLog {
    magic: 0,
    len: 0,
    user_len: 0,
    buf: [0; LOG_SIZE],
}));

/// 로그에 바이트 하나를 덧붙이는 함수입니다.
///
/// MMU 이전에도 불리므로 평범한 적재와 저장만 씁니다(MMU가 꺼진 메모리는
/// Device 타입이라 배타 접근 명령을 쓸 수 없음). 인덱스는 마스크로 범위 안에
/// 묶어 경계 검사 실패 경로가 생기지 않게 합니다.
fn log_push(b: u8, user: bool) {
    let daif = mask_irqs();
    // SAFETY: 인터럽트가 마스크된 단일 코어 구간이라 이 가변 참조는 유일함
    let log = unsafe { &mut *K0_BOOT_LOG.0.get() };
    let len = log.len as usize;
    let room = if user {
        (log.user_len as usize) < LOG_USER_CAP && len < LOG_SIZE - LOG_KERNEL_TAIL
    } else {
        len < LOG_SIZE
    };
    if room {
        log.buf[len & (LOG_SIZE - 1)] = b;
        log.len = (len + 1) as u64;
        if user {
            log.user_len += 1;
        }
        log.magic = LOG_MAGIC;
    }
    restore_irqs(daif);
}

pub(crate) fn mask_irqs() -> u64 {
    let daif: u64;
    // SAFETY: DAIF를 읽고 전부 마스크함, 호출자가 restore_irqs로 되돌림
    unsafe { asm!("mrs {}, daif", "msr daifset, #0xf", out(reg) daif, options(nostack)) };
    daif
}

pub(crate) fn restore_irqs(daif: u64) {
    // SAFETY: mask_irqs가 읽어 둔 값을 그대로 복원함
    unsafe { asm!("msr daif, {}", in(reg) daif, options(nostack)) };
}

fn put(b: u8, user: bool) {
    log_push(b, user);
    if b == b'\n' {
        hw_put(b'\r');
    }
    hw_put(b);
}

/// 커널 출력 핸들입니다.
///
/// 상태가 없는 ZST이며 `core::fmt::Write`를 구현해 `writeln!`으로 쓸 수 있습니다.
/// 단 fmt 경로는 절대 주소 재배치가 담긴 rodata를 읽기 때문에 higher-half 점프
/// 이후에만 유효합니다. 점프 전 구간과 예외 진단은 `put_str` / `put_hex`만
/// 사용해야 합니다.
pub struct EarlyCon;

impl EarlyCon {
    /// fmt 기계 없이 문자열을 그대로 출력하는 함수입니다.
    ///
    /// # Arguments
    /// `s` - 출력할 문자열, 콘솔에서는 `\n`이 `\r\n`으로 나감
    pub fn put_str(&mut self, s: &str) {
        for b in s.bytes() {
            put(b, false);
        }
    }

    /// 바이트 하나를 출력하는 함수입니다.
    ///
    /// # Arguments
    /// `b` - 출력할 바이트
    pub fn put_byte(&mut self, b: u8) {
        put(b, false);
    }

    /// EL0가 Console 케이퍼빌리티로 낸 바이트 하나를 출력하는 함수입니다.
    /// 로그에는 사용자 몫까지만 기록됩니다.
    ///
    /// # Arguments
    /// `b` - 출력할 바이트
    pub fn put_user_byte(&mut self, b: u8) {
        put(b, true);
    }

    /// 64비트 값을 0x 접두사 16자리 16진수로 출력하는 함수입니다.
    ///
    /// # Arguments
    /// `v` - 출력할 값
    pub fn put_hex(&mut self, v: u64) {
        self.put_str("0x");
        for i in (0..16).rev() {
            let d = ((v >> (i * 4)) & 0xF) as u8;
            put(if d < 10 { b'0' + d } else { b'a' + d - 10 }, false);
        }
    }
}

impl fmt::Write for EarlyCon {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.put_str(s);
        Ok(())
    }
}

/// 태스크가 일으킨 커널 줄 하나를 쓰는 핸들입니다.
///
/// 태스크 하나가 끝날 때마다 한 줄씩 나오는 진단(폴트, 종료, 모르는 시스템
/// 콜)에 씁니다. 태스크를 만들 권한이 있는 쪽은 이런 줄을 원하는 만큼 만들 수
/// 있으므로, 로그에는 정해진 몫까지만 줄 단위로 남기고 커널 진단용 꼬리는
/// 건드리지 않습니다. 콘솔에는 몫과 상관없이 그대로 나갑니다. 몫을 처음 넘길
/// 때 그 사실을 한 줄로 남깁니다. 핸들 하나로 줄 하나만 씁니다.
pub struct TaskLine {
    /// 이 줄이 로그에 남는지
    logged: bool,
    /// 이 줄이 로그에 쓴 바이트 수
    used: usize,
}

impl TaskLine {
    /// 줄 하나를 시작하는 함수입니다. 줄 하나가 통째로 들어갈 자리가 로그에
    /// 있는지 여기서 한 번 정하므로 줄이 중간에 끊겨 남지 않습니다.
    pub fn begin() -> Self {
        let daif = mask_irqs();
        // SAFETY: 인터럽트가 마스크된 단일 코어 구간에서 길이만 읽음
        let len = unsafe { (*K0_BOOT_LOG.0.get()).len as usize };
        restore_irqs(daif);
        let logged = TASK_LEN.load(Ordering::Relaxed) <= LOG_TASK_CAP - TASK_LINE_MAX
            && len <= LOG_SIZE - LOG_KERNEL_TAIL - TASK_LINE_MAX;
        if !logged && !TASK_NOTED.load(Ordering::Relaxed) {
            TASK_NOTED.store(true, Ordering::Relaxed);
            EarlyCon.put_str("k0: task lines over the log quota, later ones go to the console only\n");
        }
        Self { logged, used: 0 }
    }
}

impl fmt::Write for TaskLine {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            // 마지막 한 자리는 줄 끝을 위해 남겨 두어 긴 줄도 줄 끝으로 닫힘
            let room = if b == b'\n' { TASK_LINE_MAX } else { TASK_LINE_MAX - 1 };
            if self.logged && self.used < room {
                log_push(b, false);
                self.used = self.used.saturating_add(1);
                TASK_LEN.store(TASK_LEN.load(Ordering::Relaxed).saturating_add(1), Ordering::Relaxed);
            }
            if b == b'\n' {
                hw_put(b'\r');
            }
            hw_put(b);
        }
        Ok(())
    }
}

/// 로그에는 남기지 않고 콘솔에만 쓰는 핸들입니다. 끝없이 반복되는 출력(주기
/// 틱)이 로그를 채워 뒤의 진단을 밀어내지 않게 하는 데 씁니다. 콘솔 드라이버가
/// 없는 빌드에는 쓸 곳이 없으므로 이 핸들도 없습니다.
#[cfg(any(feature = "console-pl011", feature = "console-16550"))]
pub struct ConsoleOnly;

#[cfg(any(feature = "console-pl011", feature = "console-16550"))]
impl fmt::Write for ConsoleOnly {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                hw_put(b'\r');
            }
            hw_put(b);
        }
        Ok(())
    }
}
