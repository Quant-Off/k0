//! 콘솔 UART 송신 드라이버 모듈입니다.
//!
//! # Features
//! 커널이 기계 밖으로 출력을 내보내는 유일한 경로입니다. 송신만 합니다.
//! 초기화 쓰기와 보율 설정은 하지 않고 부트로더가 켜 둔 상태를 그대로 씁니다.
//! 송신 가능 대기에는 시간 상한이 있어서, 응답 없는 UART가 인터럽트를 막은 채
//! 커널을 붙잡지 못합니다.
//!
//! 이 모듈은 feature `console-pl011`이나 `console-16550`이 하나라도 켜져 있을
//! 때만 빌드됩니다. 드라이버마다 하위 모듈이 따로 있고 각자의 feature 뒤에
//! 있어서, 고르지 않은 드라이버의 코드는 이미지에 들어가지 않습니다. 부팅
//! 매트릭스가 이미지의 심볼로 이를 확인하므로, 드라이버의 진입 함수는
//! 인라인되지 않게 해 둡니다.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use super::{K0_BOOT_LOG, LOG_SIZE};

#[cfg(feature = "console-pl011")]
pub use pl011::identity_ok as pl011_identity_ok;

/// 콘솔 UART의 종류와 레지스터 접근 방식을 나타내는 열거형입니다.
///
/// 빌드에 들어 있는 드라이버의 종류만 값으로 존재합니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uart {
    /// PL011, 그리고 같은 레지스터 배치를 쓰는 SBSA UART
    #[cfg(feature = "console-pl011")]
    Pl011,
    /// 레지스터 n의 위치는 `n << shift`, 접근 폭은 `width`바이트(1 또는 4)
    #[cfg(feature = "console-16550")]
    Ns16550 { shift: u8, width: u8 },
}

const KIND_NONE: u8 = 0;
#[cfg(feature = "console-pl011")]
const KIND_PL011: u8 = 1;
#[cfg(feature = "console-16550")]
const KIND_NS16550: u8 = 2;

static KIND: AtomicU8 = AtomicU8::new(KIND_NONE);
static BASE: AtomicUsize = AtomicUsize::new(0);
/// 송신 대기가 상한을 넘긴 뒤의 상태. 이 동안은 기다리지 않고 버림
static STALLED: AtomicBool = AtomicBool::new(false);
/// 바이트 하나의 송신 대기 상한(카운터 틱). 0이면 폴링 횟수로 대신함
static BUDGET: AtomicU64 = AtomicU64::new(0);

/// 바이트 하나의 송신 대기에 허용하는 폴링 횟수 상한. 시간 상한이 먼저
/// 걸리는 것이 보통이고, 카운터를 믿을 수 없을 때의 마지막 방어선입니다
const POLL_LIMIT: u32 = 1_000_000;

/// PL011과 SBSA UART의 송신 드라이버입니다.
#[cfg(feature = "console-pl011")]
mod pl011 {
    const DR: usize = 0x000;
    const FR: usize = 0x018;
    const FR_TXFF: u32 = 1 << 5;

    /// PL011의 식별 레지스터(PeriphID0, PeriphID1의 하위 니블, PrimeCell ID 넷)가
    /// PL011의 값인지 확인하는 함수입니다.
    ///
    /// DTB가 콘솔이라고 가리킨 주소에 처음 쓰기 전에 부릅니다. SBSA UART와
    /// 16550 계열에는 읽기만으로 확인할 식별 레지스터가 없어 이 검사를 할 수
    /// 없습니다.
    ///
    /// # Safety
    /// `page`는 PL011의 4KiB 레지스터 프레임 시작이 Device로 매핑된 주소여야 합니다.
    #[inline(never)]
    pub unsafe fn identity_ok(page: usize) -> bool {
        // SAFETY: 호출자가 보장한 대로 매핑된 프레임 안의 식별 레지스터를 읽기만 함
        let rd = |off: usize| unsafe { core::ptr::read_volatile((page + off) as *const u32) } & 0xFF;
        rd(0xFE0) == 0x11
            && rd(0xFE4) & 0x0F == 0x0
            && rd(0xFF0) == 0x0D
            && rd(0xFF4) == 0xF0
            && rd(0xFF8) == 0x05
            && rd(0xFFC) == 0xB1
    }

    /// 송신 FIFO에 자리가 있는지 알려 주는 함수입니다.
    #[inline(never)]
    pub(super) fn tx_ready(base: usize) -> bool {
        // SAFETY: attach의 계약대로 매핑된 PL011 레지스터에 대한 volatile 접근
        let fr = unsafe { core::ptr::read_volatile((base + FR) as *const u32) };
        fr & FR_TXFF == 0
    }

    /// 바이트 하나를 송신 레지스터에 쓰는 함수입니다.
    #[inline(never)]
    pub(super) fn tx(base: usize, b: u8) {
        // SAFETY: attach의 계약대로 매핑된 PL011 레지스터에 대한 volatile 접근
        unsafe { core::ptr::write_volatile((base + DR) as *mut u32, u32::from(b)) };
    }
}

/// 16550 계열 UART의 송신 드라이버입니다.
#[cfg(feature = "console-16550")]
mod ns16550 {
    use core::sync::atomic::{AtomicU8, Ordering};

    const THR: usize = 0;
    const LSR: usize = 5;
    const LSR_THRE: u32 = 1 << 5;

    static SHIFT: AtomicU8 = AtomicU8::new(0);
    static WIDTH: AtomicU8 = AtomicU8::new(1);

    /// 레지스터 간격과 접근 폭을 기억하는 함수입니다.
    ///
    /// # Arguments
    /// `shift` - 레지스터 n의 위치는 `n << shift`
    /// `width` - 접근 폭(바이트), 1 또는 4
    pub(super) fn configure(shift: u8, width: u8) {
        SHIFT.store(shift, Ordering::Relaxed);
        WIDTH.store(width, Ordering::Relaxed);
    }

    fn addr(base: usize, index: usize) -> usize {
        base + (index << SHIFT.load(Ordering::Relaxed))
    }

    /// 송신 보관 레지스터가 비었는지 알려 주는 함수입니다.
    #[inline(never)]
    pub(super) fn tx_ready(base: usize) -> bool {
        let at = addr(base, LSR);
        // SAFETY: attach의 계약대로 매핑된 UART 레지스터에 대한 volatile 접근
        let lsr = unsafe {
            if WIDTH.load(Ordering::Relaxed) == 1 {
                u32::from(core::ptr::read_volatile(at as *const u8))
            } else {
                core::ptr::read_volatile(at as *const u32)
            }
        };
        lsr & LSR_THRE != 0
    }

    /// 바이트 하나를 송신 보관 레지스터에 쓰는 함수입니다.
    #[inline(never)]
    pub(super) fn tx(base: usize, b: u8) {
        let at = addr(base, THR);
        // SAFETY: attach의 계약대로 매핑된 UART 레지스터에 대한 volatile 접근
        unsafe {
            if WIDTH.load(Ordering::Relaxed) == 1 {
                core::ptr::write_volatile(at as *mut u8, b);
            } else {
                core::ptr::write_volatile(at as *mut u32, u32::from(b));
            }
        }
    }
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
    let kind = match uart {
        #[cfg(feature = "console-pl011")]
        Uart::Pl011 => KIND_PL011,
        #[cfg(feature = "console-16550")]
        Uart::Ns16550 { shift, width } => {
            ns16550::configure(shift, width);
            KIND_NS16550
        }
    };
    // 바이트 하나에 10ms: 1200보에서도 한 글자가 나가는 시간
    BUDGET.store(counter_hz / 100, Ordering::Relaxed);
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

fn tx_ready(kind: u8, base: usize) -> bool {
    match kind {
        #[cfg(feature = "console-pl011")]
        KIND_PL011 => pl011::tx_ready(base),
        #[cfg(feature = "console-16550")]
        KIND_NS16550 => ns16550::tx_ready(base),
        _ => false,
    }
}

fn tx(kind: u8, base: usize, b: u8) {
    match kind {
        #[cfg(feature = "console-pl011")]
        KIND_PL011 => pl011::tx(base, b),
        #[cfg(feature = "console-16550")]
        KIND_NS16550 => ns16550::tx(base, b),
        _ => {}
    }
}

/// 바이트 하나를 UART로 내보내는 함수입니다. 콘솔이 없으면 아무것도 하지 않습니다.
pub(super) fn hw_put(b: u8) {
    let kind = KIND.load(Ordering::Relaxed);
    if kind == KIND_NONE {
        return;
    }
    let base = BASE.load(Ordering::Relaxed);

    if STALLED.load(Ordering::Relaxed) {
        // 멈춘 UART는 기다리지 않음. 다시 받을 수 있게 됐을 때만 재개함
        if !tx_ready(kind, base) {
            return;
        }
        STALLED.store(false, Ordering::Relaxed);
    } else {
        let budget = BUDGET.load(Ordering::Relaxed);
        let start = counter();
        let mut polls = 0u32;
        while !tx_ready(kind, base) {
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
    tx(kind, base, b);
}
