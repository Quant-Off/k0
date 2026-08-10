//! 진입 페이즈 1 초기 콘솔(earlycon) 모듈입니다.
//!
//! # Features
//! MMU 이전 단계에서 물리 MMIO 주소로 직접 송신만 수행합니다. 수신은 없고,
//! 초기화도 부트로더(QEMU / m1n1)가 이미 끝낸 상태를 전제합니다. higher-half
//! 점프 후 `use_higher_half`를 부르면 접근 베이스가 TTBR1 별칭 VA로 바뀌어
//! TTBR0이 사용자 공간으로 넘어간 뒤에도 동작합니다.
//!
//! # Examples
//! ```rust,ignore
//! use core::fmt::Write;
//! let mut con = k0_arch::earlycon::EarlyCon;
//! let _ = writeln!(con, "k0: hello");
//! ```

use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

/// earlycon MMIO 물리 베이스(진입 페이즈 1의 디바이스 매핑에도 쓰임)
#[cfg(feature = "plat-virt")]
pub const MMIO_BASE: usize = 0x0900_0000;
#[cfg(feature = "plat-apple")]
pub const MMIO_BASE: usize = 0x2_3520_0000;

// 접근 베이스, 초기값은 물리 주소고 점프 후 TTBR1 별칭 VA로 전환됨
static BASE: AtomicUsize = AtomicUsize::new(MMIO_BASE);

/// earlycon 접근 베이스를 TTBR1 별칭 VA로 전환하는 함수입니다.
///
/// higher-half 점프 이후 한 번 호출합니다. 이 전환이 있어야 진입 페이즈 3에서
/// TTBR0(identity)이 사용자 테이블로 교체된 뒤에도 콘솔이 살아 있습니다.
pub fn use_higher_half() {
    BASE.store(MMIO_BASE + k0_abi::KERNEL_VA_OFFSET as usize, Ordering::Relaxed);
}

#[cfg(feature = "plat-virt")]
mod imp {
    /// QEMU virt의 PL011 UART (virt 머신 고정 주소)
    use super::{Ordering, BASE};
    const DR: usize = 0x000;
    const FR: usize = 0x018;
    const FR_TXFF: u32 = 1 << 5;

    /// 바이트 하나를 송신 FIFO에 넣는 함수입니다.
    ///
    /// # Arguments
    /// `b` - 송신할 바이트
    ///
    /// # Safety
    /// 내부 unsafe 블록은 PL011 MMIO에 volatile 접근합니다. 현재 베이스가
    /// 접근 가능한 상태(MMU OFF의 물리 주소 또는 디바이스 매핑된 VA)여야 합니다.
    pub(super) fn putb(b: u8) {
        let base = BASE.load(Ordering::Relaxed);
        unsafe {
            while core::ptr::read_volatile((base + FR) as *const u32) & FR_TXFF != 0 {}
            core::ptr::write_volatile((base + DR) as *mut u32, u32::from(b));
        }
    }
}

#[cfg(feature = "plat-apple")]
mod imp {
    /// Apple Silicon(M1, t8103) 부트 UART, Samsung S5L 계열
    use super::{Ordering, BASE};
    const UTRSTAT: usize = 0x010;
    const UTXH: usize = 0x020;
    const TX_EMPTY: u32 = 1 << 1;

    /// 바이트 하나를 송신 레지스터에 넣는 함수입니다.
    ///
    /// # Arguments
    /// `b` - 송신할 바이트
    ///
    /// # Safety
    /// 내부 unsafe 블록은 S5L UART MMIO에 volatile 접근합니다. m1n1이 UART를
    /// 초기화해 둔 상태와 현재 베이스의 접근 가능성을 전제합니다.
    pub(super) fn putb(b: u8) {
        let base = BASE.load(Ordering::Relaxed);
        unsafe {
            while core::ptr::read_volatile((base + UTRSTAT) as *const u32) & TX_EMPTY == 0 {}
            core::ptr::write_volatile((base + UTXH) as *mut u32, u32::from(b));
        }
    }
}

/// 초기 콘솔 핸들입니다.
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
    /// `s` - 출력할 문자열, `\n`은 `\r\n`으로 변환됨
    pub fn put_str(&mut self, s: &str) {
        for b in s.bytes() {
            self.put_byte(b);
        }
    }

    /// 바이트 하나를 출력하는 함수입니다. (`\n`은 `\r\n`으로 변환)
    ///
    /// # Arguments
    /// `b` - 출력할 바이트
    pub fn put_byte(&mut self, b: u8) {
        if b == b'\n' {
            imp::putb(b'\r');
        }
        imp::putb(b);
    }

    /// 64비트 값을 0x 접두사 16자리 16진수로 출력하는 함수입니다.
    ///
    /// # Arguments
    /// `v` - 출력할 값
    pub fn put_hex(&mut self, v: u64) {
        self.put_str("0x");
        for i in (0..16).rev() {
            let d = ((v >> (i * 4)) & 0xF) as u8;
            let c = if d < 10 { b'0' + d } else { b'a' + d - 10 };
            imp::putb(c);
        }
    }
}

impl fmt::Write for EarlyCon {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.put_str(s);
        Ok(())
    }
}
