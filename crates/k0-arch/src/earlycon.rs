//! 부트 로그와 콘솔 출력 모듈입니다.
//!
//! # Features
//! 커널의 모든 출력은 두 곳으로 갑니다. 하나는 커널 메모리의 부트 로그이고,
//! 다른 하나는 DTB에서 찾아 검증을 마친 뒤 붙이는 콘솔 UART입니다. 콘솔을
//! 알기 전의 출력은 로그에만 쌓였다가 콘솔이 붙을 때 그대로 재생되므로, 범용
//! 이미지에서도 부팅 순서대로의 기록이 남습니다. 지원하는 UART가 없는
//! 보드에서는 로그가 유일한 기록입니다(디버거나 시험 하니스가 읽음).
//!
//! 콘솔 드라이버는 송신만 합니다. 초기화 쓰기와 보율 설정은 하지 않고
//! 부트로더가 켜 둔 상태를 그대로 씁니다. 송신 가능 대기에는 시간 상한이
//! 있어서, 응답 없는 UART가 인터럽트를 막은 채 커널을 붙잡지 못합니다.
//!
//! 로그는 앞에서부터 채우고 가득 차면 버립니다(되감지 않음). EL0가 Console
//! 케이퍼빌리티로 내는 출력은 정해진 몫까지만 기록해, 사용자 출력이 커널의
//! 진단을 밀어내지 못하게 합니다.
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
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};

/// 부트 로그 본문의 크기 (2의 거듭제곱)
pub const LOG_SIZE: usize = 16 * 1024;

/// EL0 출력에 허용하는 로그 몫
const LOG_USER_CAP: usize = 6 * 1024;

/// 커널 진단 전용으로 남겨 두는 로그 꼬리. 사용자 출력은 이 앞에서 멈춤
const LOG_KERNEL_TAIL: usize = 2 * 1024;

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

fn mask_irqs() -> u64 {
    let daif: u64;
    // SAFETY: DAIF를 읽고 전부 마스크함, 호출자가 restore_irqs로 되돌림
    unsafe { asm!("mrs {}, daif", "msr daifset, #0xf", out(reg) daif, options(nostack)) };
    daif
}

fn restore_irqs(daif: u64) {
    // SAFETY: mask_irqs가 읽어 둔 값을 그대로 복원함
    unsafe { asm!("msr daif, {}", in(reg) daif, options(nostack)) };
}

/// 콘솔 UART의 종류와 레지스터 접근 방식을 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uart {
    Pl011,
    /// 레지스터 n의 위치는 `n << shift`, 접근 폭은 `width`바이트(1 또는 4)
    Ns16550 { shift: u8, width: u8 },
}

const KIND_NONE: u8 = 0;
const KIND_PL011: u8 = 1;
const KIND_NS16550: u8 = 2;

static KIND: AtomicU8 = AtomicU8::new(KIND_NONE);
static BASE: AtomicUsize = AtomicUsize::new(0);
static SHIFT: AtomicU8 = AtomicU8::new(0);
static WIDTH: AtomicU8 = AtomicU8::new(1);
/// 송신 대기가 상한을 넘긴 뒤의 상태. 이 동안은 기다리지 않고 버림
static STALLED: AtomicBool = AtomicBool::new(false);
/// 바이트 하나의 송신 대기 상한(카운터 틱). 0이면 폴링 횟수로 대신함
static BUDGET: AtomicU64 = AtomicU64::new(0);

/// 바이트 하나의 송신 대기에 허용하는 폴링 횟수 상한. 시간 상한이 먼저
/// 걸리는 것이 보통이고, 카운터를 믿을 수 없을 때의 마지막 방어선입니다
const POLL_LIMIT: u32 = 1_000_000;

const PL011_DR: usize = 0x000;
const PL011_FR: usize = 0x018;
const PL011_FR_TXFF: u32 = 1 << 5;
const NS16550_THR: usize = 0;
const NS16550_LSR: usize = 5;
const NS16550_LSR_THRE: u32 = 1 << 5;

/// PL011의 식별 레지스터(PeriphID0, PeriphID1의 하위 니블, PrimeCell ID 넷)가
/// PL011의 값인지 확인하는 함수입니다.
///
/// DTB가 콘솔이라고 가리킨 주소에 처음 쓰기 전에 부릅니다. 16550 계열에는
/// 읽기만으로 확인할 식별 레지스터가 없어 이 검사를 할 수 없습니다.
///
/// # Safety
/// `page`는 PL011의 4KiB 레지스터 프레임 시작이 Device로 매핑된 주소여야 합니다.
pub unsafe fn pl011_identity_ok(page: usize) -> bool {
    let rd = |off: usize| unsafe { core::ptr::read_volatile((page + off) as *const u32) } & 0xFF;
    rd(0xFE0) == 0x11
        && rd(0xFE4) & 0x0F == 0x0
        && rd(0xFF0) == 0x0D
        && rd(0xFF4) == 0xF0
        && rd(0xFF8) == 0x05
        && rd(0xFFC) == 0xB1
}

/// 검증을 마친 콘솔을 붙이고 그동안 쌓인 로그를 재생하는 함수입니다.
///
/// # Arguments
/// `uart` - UART의 종류
/// `base` - 레지스터 0의 가상 주소(디바이스 창에 매핑된 주소)
/// `counter_hz` - 카운터 주파수, 송신 대기의 시간 상한 계산에 씀(0이면 모름)
///
/// # Safety
/// `base`부터 드라이버가 건드리는 범위가 Device로 매핑돼 있어야 하고, 그
/// 주소가 실제로 해당 종류의 UART임을 호출자가 가능한 만큼 확인했어야 합니다.
pub unsafe fn attach(uart: Uart, base: usize, counter_hz: u64) {
    let (kind, shift, width) = match uart {
        Uart::Pl011 => (KIND_PL011, 0, 4),
        Uart::Ns16550 { shift, width } => (KIND_NS16550, shift, width),
    };
    // 바이트 하나에 10ms: 1200보에서도 한 글자가 나가는 시간
    BUDGET.store(counter_hz / 100, Ordering::Relaxed);
    SHIFT.store(shift, Ordering::Relaxed);
    WIDTH.store(width, Ordering::Relaxed);
    BASE.store(base, Ordering::Relaxed);
    STALLED.store(false, Ordering::Relaxed);
    KIND.store(kind, Ordering::Relaxed);

    // 콘솔을 알기 전에 쌓인 출력을 순서대로 내보냄
    // SAFETY: 읽기만 하고, 길이는 버퍼 크기로 묶음
    let log = unsafe { &*K0_BOOT_LOG.0.get() };
    let len = (log.len as usize).min(LOG_SIZE);
    for &b in &log.buf[..len] {
        if b == b'\n' {
            hw_put(b'\r');
        }
        hw_put(b);
    }
}

/// 콘솔이 붙어 있는지 알려 주는 함수입니다.
pub fn attached() -> bool {
    KIND.load(Ordering::Relaxed) != KIND_NONE
}

fn counter() -> u64 {
    let v: u64;
    // SAFETY: 가상 카운터 읽기는 부작용이 없음
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
    v
}

fn reg_read(base: usize, index: usize, shift: u8, width: u8) -> u32 {
    let addr = base + (index << shift);
    // SAFETY: attach의 계약대로 매핑된 UART 레지스터에 대한 volatile 접근
    unsafe {
        if width == 1 {
            u32::from(core::ptr::read_volatile(addr as *const u8))
        } else {
            core::ptr::read_volatile(addr as *const u32)
        }
    }
}

fn reg_write(base: usize, index: usize, shift: u8, width: u8, v: u8) {
    let addr = base + (index << shift);
    // SAFETY: attach의 계약대로 매핑된 UART 레지스터에 대한 volatile 접근
    unsafe {
        if width == 1 {
            core::ptr::write_volatile(addr as *mut u8, v);
        } else {
            core::ptr::write_volatile(addr as *mut u32, u32::from(v));
        }
    }
}

/// 바이트 하나를 UART로 내보내는 함수입니다. 콘솔이 없으면 아무것도 하지 않습니다.
fn hw_put(b: u8) {
    let kind = KIND.load(Ordering::Relaxed);
    if kind == KIND_NONE {
        return;
    }
    let base = BASE.load(Ordering::Relaxed);
    let (shift, width) = (SHIFT.load(Ordering::Relaxed), WIDTH.load(Ordering::Relaxed));
    let ready = || match kind {
        KIND_PL011 => reg_read(base, PL011_FR >> 2, 2, 4) & PL011_FR_TXFF == 0,
        _ => reg_read(base, NS16550_LSR, shift, width) & NS16550_LSR_THRE != 0,
    };

    if STALLED.load(Ordering::Relaxed) {
        // 멈춘 UART는 기다리지 않음. 다시 받을 수 있게 됐을 때만 재개함
        if !ready() {
            return;
        }
        STALLED.store(false, Ordering::Relaxed);
    } else {
        let budget = BUDGET.load(Ordering::Relaxed);
        let start = counter();
        let mut polls = 0u32;
        while !ready() {
            polls += 1;
            // 시간 상한에 더해 횟수 상한을 항상 둠. 카운터가 멈춰 있는
            // 시스템에서도 응답 없는 UART에 붙잡히지 않게 함
            let expired = polls > POLL_LIMIT
                || (budget != 0 && counter().wrapping_sub(start) > budget);
            if expired {
                STALLED.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
    match kind {
        KIND_PL011 => reg_write(base, PL011_DR >> 2, 2, 4, b),
        _ => reg_write(base, NS16550_THR, shift, width, b),
    }
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

/// 로그에는 남기지 않고 콘솔에만 쓰는 핸들입니다. 끝없이 반복되는 출력(주기
/// 틱)이 로그를 채워 뒤의 진단을 밀어내지 않게 하는 데 씁니다.
pub struct ConsoleOnly;

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
