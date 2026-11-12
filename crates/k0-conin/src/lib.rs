//! 콘솔 입력의 수신 로직을 담는 크레이트입니다.
//!
//! # Features
//! PL011과 SBSA UART에서 바이트를 받아 고정 크기 링에 담는 상태 기계([Rx])
//! 입니다. 하드웨어에는 직접 닿지 않고 레지스터 접근을 트레이트([Regs])로
//! 받으므로, 커널에서는 MMIO로, 호스트 테스트에서는 가짜 UART로 같은 코드를
//! 돌립니다. QEMU의 UART는 넘침도 수신 타임아웃도 흉내 내지 않아서, 그 경로는
//! 가짜 UART로만 시험됩니다.
//!
//! 지키는 성질은 다음과 같습니다.
//!
//! - 바이트를 해석하지 않습니다. 옮기기만 합니다.
//! - 인터럽트 한 번의 일은 링의 빈자리로 묶여 있습니다. 링이 가득 차면 UART의
//!   수신 인터럽트를 스스로 막고(게이트), 읽는 쪽이 자리를 비운 뒤에 다시
//!   엽니다. 그래서 외부가 아무리 빨리 보내도 인터럽트는 읽는 쪽의 소비
//!   속도만큼만 들어옵니다.
//! - 수신 원인은 지우지 않고 FIFO를 비워서 가라앉힙니다. FIFO에 남은 바이트를
//!   새 인터럽트가 알려 주리라고 기대하지 않고, 게이트를 열 때와 틱마다 직접
//!   들여다봅니다.
//! - 유실은 유실된 자리의 바이트에 표시합니다. 장치의 FIFO가 넘쳤으면 그때
//!   FIFO에 있던 바이트와 그 뒤의 첫 바이트까지 모두 표시합니다(넘친 자리는
//!   FIFO에 있던 바이트들의 뒤이지만 정확한 경계는 알 수 없음).
//! - 원인 없는 인터럽트가 이어지면 입력 경로를 끕니다. 디바이스 트리가 엉뚱한
//!   인터럽트 번호를 주었을 때 잃는 것은 입력뿐입니다.
//!
//! # Examples
//! ```rust,ignore
//! let mut rx = Rx::new(true);
//! Rx::quiesce(&mut regs);
//! rx.open(&mut regs);
//! while let Some((byte, lost)) = rx.pop(&mut regs) { /* ... */ }
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

/// 수신 로직이 건드리는 레지스터입니다. PL011과 SBSA UART에 공통인 것들이고
/// [Reg::Cr]만 PL011 전용입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reg {
    /// 데이터(UARTDR)
    Dr,
    /// 플래그(UARTFR)
    Fr,
    /// 제어(UARTCR), SBSA UART에는 없음
    Cr,
    /// 인터럽트 마스크(UARTIMSC)
    Imsc,
    /// 마스크를 거친 인터럽트 상태(UARTMIS)
    Mis,
    /// 인터럽트 지우기(UARTICR)
    Icr,
}

impl Reg {
    /// 레지스터 0으로부터의 바이트 오프셋을 주는 함수입니다.
    pub const fn offset(self) -> usize {
        match self {
            Reg::Dr => 0x00,
            Reg::Fr => 0x18,
            Reg::Cr => 0x30,
            Reg::Imsc => 0x38,
            Reg::Mis => 0x40,
            Reg::Icr => 0x44,
        }
    }
}

/// UART 레지스터에 닿는 통로입니다.
pub trait Regs {
    /// 레지스터를 32비트로 읽는 함수입니다.
    fn read(&mut self, reg: Reg) -> u32;
    /// 레지스터에 32비트를 쓰는 함수입니다.
    fn write(&mut self, reg: Reg, value: u32);
}

/// 링이 담는 바이트 수
pub const RING: usize = 256;
/// 원인 없는 인터럽트가 이만큼 이어지면 입력 경로를 끔
pub const SPURIOUS_MAX: u8 = 8;
/// FIFO를 한 번 비울 때 데이터 레지스터를 읽는 횟수의 상한. 오류가 난 문자는
/// 링의 자리를 쓰지 않고 버려지므로 링의 빈자리와 따로 묶어 둠
const DRAIN_MAX: usize = RING + 64;
/// 버릴 때 장치의 FIFO에서 꺼내는 횟수의 상한(PL011의 FIFO는 32칸)
const FLUSH_MAX: usize = 64;

const FR_RXFE: u32 = 1 << 4;
const DR_FE: u32 = 1 << 8;
const DR_PE: u32 = 1 << 9;
const DR_BE: u32 = 1 << 10;
const DR_OE: u32 = 1 << 11;
const CR_UARTEN: u32 = 1 << 0;
const CR_RXE: u32 = 1 << 9;
/// 수신, 수신 타임아웃, 넘침 인터럽트
const INT_RX: u32 = 1 << 4;
const INT_RT: u32 = 1 << 6;
const INT_OE: u32 = 1 << 10;
const INT_OURS: u32 = INT_RX | INT_RT | INT_OE;
/// UARTICR에 쓸 수 있는 모든 비트
const INT_ALL: u32 = 0x7FF;
/// 링 항목에서 "이 바이트 바로 앞에서 유실이 있었음"을 뜻하는 비트
const GAP: u16 = 1 << 8;

/// 인터럽트 하나를 처리한 결과입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Irq {
    /// UART가 원인이었고 FIFO를 비웠음(링에 바이트가 있을 수 있음)
    Served,
    /// UART에 원인이 없었음
    Spurious,
    /// 원인 없는 인터럽트가 이어져 입력 경로를 껐음. 호출자는 인터럽트
    /// 컨트롤러에서 이 선을 꺼야 함
    CutOff,
}

/// 수신 상태를 담는 구조체입니다.
///
/// 커널의 static 하나에 놓이고, 인터럽트 핸들러와 시스템 콜 경로가 번갈아
/// 씁니다(둘이 겹치지 않게 하는 것은 호출자의 몫).
pub struct Rx {
    ring: [u16; RING],
    head: usize,
    len: usize,
    /// UARTCR이 있는 UART인지(PL011은 있고 SBSA UART는 없음)
    has_cr: bool,
    /// 태스크가 입력을 쓰기 시작해 수신 인터럽트를 켠 상태인지
    open: bool,
    /// 입력 경로가 꺼졌는지(되돌리지 않음)
    off: bool,
    /// 링이 가득 차 UART의 수신 인터럽트를 막아 둔 상태인지
    gated: bool,
    /// 다음에 넣는 바이트에 유실 표시를 붙일지
    gap: bool,
    /// 장치의 FIFO가 넘쳤고 그 FIFO를 아직 끝까지 비우지 못했는지
    overrun: bool,
    spurious: u8,
}

impl Rx {
    /// 닫힌 상태의 수신 로직을 만드는 함수입니다.
    ///
    /// # Arguments
    /// `has_cr` - UARTCR이 있는 UART(PL011)면 true, SBSA UART면 false
    pub const fn new(has_cr: bool) -> Self {
        Self {
            ring: [0; RING],
            head: 0,
            len: 0,
            has_cr,
            open: false,
            off: false,
            gated: false,
            gap: false,
            overrun: false,
            spurious: 0,
        }
    }

    /// UART의 인터럽트를 전부 막고 남은 원인을 지우는 함수입니다.
    ///
    /// 부팅 때, 인터럽트 컨트롤러에서 콘솔의 선을 켜기 전에 부릅니다.
    /// 펌웨어가 켜 둔 인터럽트(송신 인터럽트 등)가 선을 붙들지 않게 합니다.
    pub fn quiesce(regs: &mut impl Regs) {
        regs.write(Reg::Imsc, 0);
        regs.write(Reg::Icr, INT_ALL);
    }

    /// 입력을 쓰기 시작하는 함수입니다. 그동안 장치에 쌓인 바이트를 버리고
    /// 수신 인터럽트를 켭니다. 이미 열렸거나 꺼진 경로에는 아무 일도 하지
    /// 않습니다.
    pub fn open(&mut self, regs: &mut impl Regs) {
        if self.open || self.off {
            return;
        }
        if self.has_cr {
            // 선로 설정은 건드리지 않고 UART와 수신기가 켜져 있게만 함
            let cr = regs.read(Reg::Cr);
            if cr & (CR_UARTEN | CR_RXE) != CR_UARTEN | CR_RXE {
                regs.write(Reg::Cr, cr | CR_UARTEN | CR_RXE);
            }
        }
        self.open = true;
        self.flush(regs);
    }

    /// 입력이 열려 있는지 알려 주는 함수입니다.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// 입력 경로가 꺼졌는지 알려 주는 함수입니다.
    pub fn is_off(&self) -> bool {
        self.off
    }

    /// 링에 꺼낼 바이트가 있는지 알려 주는 함수입니다.
    pub fn has_data(&self) -> bool {
        self.len > 0
    }

    /// 입력 경로를 끄는 함수입니다. UART의 인터럽트를 막고 받아 둔 것을
    /// 버립니다.
    pub fn turn_off(&mut self, regs: &mut impl Regs) {
        regs.write(Reg::Imsc, 0);
        self.off = true;
        self.open = false;
        self.gated = false;
        self.len = 0;
    }

    /// 받아 둔 입력과 유실 표시를 전부 버리는 함수입니다.
    ///
    /// 링을 비우고 장치의 FIFO에서도 정해진 횟수까지 꺼내 버립니다. 그 뒤에도
    /// 들어오는 바이트는 평소대로 받습니다.
    pub fn flush(&mut self, regs: &mut impl Regs) {
        if !self.open {
            return;
        }
        // 버리는 동안에는 인터럽트를 막아 두고, 끝에서 FIFO의 상태에 맞춰 다시 엶
        regs.write(Reg::Imsc, 0);
        self.gated = true;
        self.head = 0;
        self.len = 0;
        for _ in 0..FLUSH_MAX {
            if regs.read(Reg::Fr) & FR_RXFE != 0 {
                break;
            }
            let _ = regs.read(Reg::Dr);
        }
        regs.write(Reg::Icr, INT_ALL);
        self.gap = false;
        self.overrun = false;
        self.service(regs);
    }

    /// 인터럽트 하나를 처리하는 함수입니다.
    pub fn interrupt(&mut self, regs: &mut impl Regs) -> Irq {
        if self.off {
            // 선을 끈 뒤에 이미 전달돼 있던 인터럽트. 레지스터를 건드리지 않음
            return Irq::Spurious;
        }
        let mis = regs.read(Reg::Mis);
        if mis & INT_OURS != 0 && self.open {
            let full = self.len == RING;
            if mis & INT_OE != 0 {
                // 넘침은 FIFO를 비워도 가라앉지 않으므로 지움
                self.overrun = true;
                regs.write(Reg::Icr, INT_OE);
            }
            // 바이트를 꺼냈거나, 넘침 알림이었거나, 링이 가득 차 게이트를 닫은
            // 경우가 제대로 된 원인임
            if self.service(regs) > 0 || mis & INT_OE != 0 || full {
                self.spurious = 0;
                return Irq::Served;
            }
        }
        // 원인이 없거나, 원인이 있다는데 꺼낼 바이트가 없었음. 다른 장치의
        // 선이거나 고장 난 장치면 선이 내려가지 않으므로 횟수를 세어 끊음
        self.spurious += 1;
        if self.spurious >= SPURIOUS_MAX {
            self.turn_off(regs);
            return Irq::CutOff;
        }
        Irq::Spurious
    }

    /// 인터럽트 없이 FIFO를 들여다보는 함수입니다.
    ///
    /// 틱마다, 그리고 읽는 태스크가 잠들기 직전에 부릅니다. 수신 타임아웃
    /// 인터럽트가 없는 UART나 조용히 틀린 인터럽트 번호에서도 입력이 (늦게나마)
    /// 도착하게 하는 보조 경로입니다.
    pub fn poll(&mut self, regs: &mut impl Regs) {
        if self.open && !self.off && self.service(regs) > 0 {
            self.spurious = 0;
        }
    }

    /// 링에서 바이트 하나를 꺼내는 함수입니다. 둘째 값은 이 바이트 바로 앞에서
    /// 입력이 유실됐는지입니다.
    pub fn pop(&mut self, regs: &mut impl Regs) -> Option<(u8, bool)> {
        if self.len == 0 {
            return None;
        }
        let e = self.ring[self.head];
        self.head = (self.head + 1) % RING;
        self.len -= 1;
        // 게이트는 링이 절반 비었을 때 다시 엶. 한 바이트마다 열면 홍수 때
        // 꺼낼 때마다 장치를 건드리게 됨
        if self.gated && self.len <= RING / 2 {
            self.service(regs);
        }
        Some((e as u8, e & GAP != 0))
    }

    /// FIFO를 링으로 옮기고 게이트를 맞추는 함수입니다. 옮긴(또는 버린) 문자
    /// 수를 돌려줍니다.
    fn service(&mut self, regs: &mut impl Regs) -> usize {
        let mut taken = 0;
        let mut empty = false;
        for _ in 0..DRAIN_MAX {
            if regs.read(Reg::Fr) & FR_RXFE != 0 {
                empty = true;
                break;
            }
            if self.len == RING {
                break;
            }
            let d = regs.read(Reg::Dr);
            taken += 1;
            if d & DR_OE != 0 {
                self.overrun = true;
            }
            if d & (DR_FE | DR_PE | DR_BE) != 0 {
                // 오류가 난 문자는 버리고 다음 바이트에 표시함
                self.gap = true;
                continue;
            }
            let mark = if self.gap || self.overrun { GAP } else { 0 };
            self.gap = false;
            self.ring[(self.head + self.len) % RING] = (d & 0xFF) as u16 | mark;
            self.len += 1;
        }
        if empty {
            if self.overrun {
                // 넘친 자리는 지금까지 FIFO에 있던 바이트들의 뒤임. 다음에
                // 오는 첫 바이트까지 표시함
                self.overrun = false;
                self.gap = true;
            }
            if self.gated {
                self.gated = false;
                regs.write(Reg::Imsc, INT_OURS);
            }
        } else if !self.gated {
            // FIFO에 남은 것이 있는데 받을 자리가 없음. 선이 내려가지 않으므로
            // UART 쪽에서 막고, 자리가 나면 pop이 다시 들여다봄
            self.gated = true;
            regs.write(Reg::Imsc, 0);
        }
        taken
    }
}
