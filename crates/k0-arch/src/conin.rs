//! 콘솔 입력의 커널 쪽 접착부 모듈입니다.
//!
//! # Features
//! 수신 로직(링, 게이트, 유실 표시, 원인 없는 인터럽트의 차단)은 `k0-conin`
//! 크레이트에 있고 호스트에서 가짜 UART로 시험됩니다. 이 모듈은 그 로직을 실제
//! UART의 레지스터와 커널의 static 하나에 잇기만 합니다. feature
//! `console-input`이 켜져 있을 때만 빌드되므로, 고르지 않은 이미지에는 UART에서
//! 읽는 코드가 없습니다.
//!
//! 수신 드라이버는 PL011과 SBSA UART에만 있습니다(feature `console-pl011`).
//! 16550 콘솔에서는 [prepare]가 false를 돌려주고 입력이 없습니다.
//!
//! 수신 상태는 인터럽트 핸들러와 시스템 콜 경로가 함께 씁니다. 단일 코어이고
//! 모든 접근이 인터럽트를 마스크한 구간 안에서 이루어지므로 서로 겹치지
//! 않습니다. 핸들러는 링에 바이트를 옮기기만 하고 스케줄러는 건드리지 않습니다.
//! 기다리는 태스크를 깨우는 일은 커널이 자기 문맥에서 합니다.

#[cfg(feature = "console-pl011")]
mod pl011 {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    use k0_conin::{Irq, Reg, Regs, Rx};

    use crate::earlycon::{mask_irqs, pl011_base, restore_irqs};

    /// 매핑된 UART 레지스터에 닿는 통로입니다.
    struct Mmio(usize);

    impl Regs for Mmio {
        fn read(&mut self, reg: Reg) -> u32 {
            // SAFETY: prepare의 계약대로 매핑된 PL011 레지스터에 대한 volatile 접근
            unsafe { core::ptr::read_volatile((self.0 + reg.offset()) as *const u32) }
        }

        fn write(&mut self, reg: Reg, value: u32) {
            // SAFETY: prepare의 계약대로 매핑된 PL011 레지스터에 대한 volatile 접근
            unsafe { core::ptr::write_volatile((self.0 + reg.offset()) as *mut u32, value) }
        }
    }

    struct Cell(UnsafeCell<Rx>);

    // SAFETY: 단일 코어이고 모든 접근이 with 안에서 인터럽트를 마스크한 채
    //         이루어지므로 동시에 두 곳에서 접근하지 않음
    unsafe impl Sync for Cell {}

    static RX: Cell = Cell(UnsafeCell::new(Rx::new(true)));
    /// prepare가 끝나 수신 경로가 준비됐는지
    static READY: AtomicBool = AtomicBool::new(false);

    /// 인터럽트를 마스크한 채 수신 상태에 접근하는 함수입니다. 수신 경로가
    /// 준비되지 않았으면 `None`입니다.
    fn with<R>(f: impl FnOnce(&mut Rx, &mut Mmio) -> R) -> Option<R> {
        if !READY.load(Ordering::Relaxed) {
            return None;
        }
        let base = pl011_base()?;
        let daif = mask_irqs();
        // SAFETY: 인터럽트가 마스크된 단일 코어 구간이라 이 가변 참조는 유일함.
        //         핸들러 안에서 불려도 그 핸들러가 끊은 코드는 이 구간 밖에 있음
        let r = f(unsafe { &mut *RX.0.get() }, &mut Mmio(base));
        restore_irqs(daif);
        Some(r)
    }

    pub unsafe fn prepare(has_cr: bool) -> bool {
        let Some(base) = pl011_base() else {
            return false;
        };
        let daif = mask_irqs();
        // SAFETY: 인터럽트가 마스크된 단일 코어 구간이고, 아직 READY가 아니라
        //         다른 경로는 이 상태에 접근하지 않음
        unsafe { *RX.0.get() = Rx::new(has_cr) };
        Rx::quiesce(&mut Mmio(base));
        READY.store(true, Ordering::Relaxed);
        restore_irqs(daif);
        true
    }

    pub fn available() -> bool {
        with(|rx, _| !rx.is_off()).unwrap_or(false)
    }

    pub fn open() {
        with(|rx, regs| rx.open(regs));
    }

    pub fn pop() -> Option<(u8, bool)> {
        with(|rx, regs| rx.pop(regs)).flatten()
    }

    pub fn flush() {
        with(|rx, regs| rx.flush(regs));
    }

    pub fn poll() {
        with(|rx, regs| rx.poll(regs));
    }

    pub fn has_data() -> bool {
        with(|rx, _| rx.has_data()).unwrap_or(false)
    }

    pub fn shut() {
        with(|rx, regs| rx.turn_off(regs));
    }

    pub(crate) fn interrupt() -> Irq {
        with(|rx, regs| rx.interrupt(regs)).unwrap_or(Irq::Spurious)
    }
}

/// 수신 드라이버가 없는 빌드(16550만 고른 커널)에서 자리를 채우는 모듈입니다.
/// 이 빌드에는 UART에서 읽는 코드가 없습니다.
#[cfg(not(feature = "console-pl011"))]
mod pl011 {
    pub unsafe fn prepare(_has_cr: bool) -> bool {
        false
    }

    pub fn available() -> bool {
        false
    }

    pub fn open() {}

    pub fn pop() -> Option<(u8, bool)> {
        None
    }

    pub fn flush() {}

    pub fn poll() {}

    pub fn has_data() -> bool {
        false
    }

    pub fn shut() {}

    pub(crate) fn interrupt() -> k0_conin::Irq {
        k0_conin::Irq::Spurious
    }
}

/// 붙어 있는 콘솔에 수신 경로를 준비하는 함수입니다. UART의 인터럽트를 전부
/// 막아 두기만 하고 켜지는 않습니다(켜는 것은 [open]).
///
/// 인터럽트 컨트롤러에서 콘솔의 선을 켜기 전에 불러야 합니다. 수신 드라이버가
/// 없는 UART면 false를 돌려줍니다.
///
/// # Arguments
/// `has_cr` - 제어 레지스터가 있는 UART(PL011)면 true, SBSA UART면 false
///
/// # Safety
/// 콘솔이 [crate::earlycon::attach]로 붙어 있어야 하고, 그 레지스터 프레임에서
/// 수신 로직이 쓰는 범위(오프셋 0x48 미만)가 Device로 매핑돼 있어야 합니다.
pub unsafe fn prepare(has_cr: bool) -> bool {
    // SAFETY: 함수 계약을 그대로 전달
    unsafe { pl011::prepare(has_cr) }
}

/// 입력 경로가 있고 꺼지지 않았는지 알려 주는 함수입니다.
pub fn available() -> bool {
    pl011::available()
}

/// 입력을 쓰기 시작하는 함수입니다. 그동안 장치에 쌓인 바이트를 버리고 수신
/// 인터럽트를 켭니다. 이미 열린 경로에는 아무 일도 하지 않습니다.
pub fn open() {
    pl011::open();
}

/// 받아 둔 바이트 하나를 꺼내는 함수입니다. 둘째 값은 이 바이트 바로 앞에서
/// 입력이 유실됐는지입니다.
pub fn pop() -> Option<(u8, bool)> {
    pl011::pop()
}

/// 받아 둔 입력과 유실 표시를 전부 버리는 함수입니다.
pub fn flush() {
    pl011::flush();
}

/// 인터럽트 없이 장치의 FIFO를 들여다보는 함수입니다. 틱마다, 그리고 읽는
/// 태스크가 잠들기 직전에 부릅니다.
pub fn poll() {
    pl011::poll();
}

/// 꺼낼 바이트가 있는지 알려 주는 함수입니다.
pub fn has_data() -> bool {
    pl011::has_data()
}

/// 입력 경로를 끄는 함수입니다. 되돌리지 않습니다.
pub fn shut() {
    pl011::shut();
}

/// 콘솔 인터럽트 하나를 처리하는 함수입니다.
pub(crate) fn interrupt() -> k0_conin::Irq {
    pl011::interrupt()
}
