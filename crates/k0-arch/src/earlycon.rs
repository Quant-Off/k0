//! 진입 페이즈 1 초기 콘솔(earlycon) 모듈입니다.
//!
//! # Features
//! MMU 이전 단계에서 물리 MMIO 주소로 직접 송신만 수행합니다. 수신은 없고,
//! 초기화도 부트로더(QEMU / m1n1)가 이미 끝낸 상태를 전제합니다. MMU 활성화
//! 이후에는 디바이스 매핑을 거치는 정식 드라이버로 교체됩니다.
//!
//! # Examples
//! ```rust,ignore
//! use core::fmt::Write;
//! let mut con = k0_arch::earlycon::EarlyCon;
//! let _ = writeln!(con, "k0: hello");
//! ```

use core::fmt;

#[cfg(feature = "plat-virt")]
mod imp {
    /// QEMU virt의 PL011 UART 베이스 (virt 머신 고정 주소)
    const UART_BASE: usize = 0x0900_0000;
    const DR: usize = 0x000;
    const FR: usize = 0x018;
    const FR_TXFF: u32 = 1 << 5;

    /// 바이트 하나를 송신 FIFO에 넣는 함수입니다.
    ///
    /// # Arguments
    /// `b` - 송신할 바이트
    ///
    /// # Safety
    /// 내부 unsafe 블록은 PL011 MMIO에 volatile 접근합니다. MMU OFF이거나
    /// 해당 주소가 디바이스 메모리로 매핑된 상태에서만 호출해야 합니다.
    pub(super) fn putb(b: u8) {
        unsafe {
            while core::ptr::read_volatile((UART_BASE + FR) as *const u32) & FR_TXFF != 0 {}
            core::ptr::write_volatile((UART_BASE + DR) as *mut u32, u32::from(b));
        }
    }
}

#[cfg(feature = "plat-apple")]
mod imp {
    /// Apple Silicon(M1, t8103) 부트 UART 베이스, Samsung S5L 계열
    const UART_BASE: usize = 0x2_3520_0000;
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
    /// 초기화해 둔 상태와 MMU OFF(또는 디바이스 매핑)를 전제합니다.
    pub(super) fn putb(b: u8) {
        unsafe {
            while core::ptr::read_volatile((UART_BASE + UTRSTAT) as *const u32) & TX_EMPTY == 0 {}
            core::ptr::write_volatile((UART_BASE + UTXH) as *mut u32, u32::from(b));
        }
    }
}

/// 초기 콘솔 핸들입니다.
///
/// 상태가 없는 ZST이며 `core::fmt::Write`를 구현해 `writeln!`으로 쓸 수 있습니다.
pub struct EarlyCon;

impl fmt::Write for EarlyCon {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                imp::putb(b'\r');
            }
            imp::putb(b);
        }
        Ok(())
    }
}
