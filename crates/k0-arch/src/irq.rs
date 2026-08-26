//! 진입 페이즈 2의 인터럽트 컨트롤러와 제네릭 타이머 모듈입니다.
//!
//! # Features
//! virt에서는 `GICv3`(디스트리뷰터 + 리디스트리뷰터 + 시스템 레지스터 CPU
//! 인터페이스)를 초기화하고 EL1 물리 타이머(INTID 30)를 IRQ로 받습니다.
//! Apple Silicon의 타이머는 `AIC`를 거치지 않고 코어에 `FIQ`로 직접 전달되므로
//! FIQ 경로만 열고, AIC(MMIO) 초기화는 디바이스 `IRQ`가 필요해질 때 추가합니다.
//! 발생할 수 없는 조합(virt의 FIQ, apple의 IRQ)과 미지의 `INTID`는
//! fail-secure로 정지합니다.
//!
//! # Errors
//! `GIC` 웨이크업이나 레지스터 반영 대기가 한도를 넘으면 `IrqError`로 반환하며
//! 호출자는 부팅을 중단해야 합니다.

use core::arch::asm;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::earlycon::EarlyCon;
use crate::vectors::Vectors;

/// 인터럽트 경로가 열렸음을 증명하는 typestate 토큰 구조체입니다.
pub struct Irq {
    _sealed: (),
}

/// 인터럽트 초기화가 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqError {
    GicRwpTimeout,
    GicWakeTimeout,
}

/// EL1 물리 타이머의 INTID (PPI 14), apple은 FIQ 직결이라 GIC 경로에만 존재합니다.
#[cfg(feature = "plat-virt")]
const TIMER_INTID: u64 = 30;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// QEMU virt의 GICv3 MMIO 지도(커널 매핑 구성에도 쓰임) 모듈입니다.
#[cfg(feature = "plat-virt")]
pub mod gic {
    pub const GICD_BASE: u64 = 0x0800_0000;
    pub const GICD_SIZE: u64 = 0x1_0000;
    pub const GICR_BASE: u64 = 0x080A_0000;
    pub const GICR_SIZE: u64 = 0x2_0000;
}

#[cfg(feature = "plat-virt")]
mod gicv3 {
    use super::{gic, IrqError, TIMER_INTID};
    use core::arch::asm;

    // MMIO는 TTBR1 별칭 VA로 접근함.
    // TTBR0이 사용자 테이블로 교체된 뒤에도 유효해야 하고 init은 
    // higher-half 점프 이후에만 호출되기 때문
    const KVA: u64 = k0_abi::KERNEL_VA_OFFSET;
    const GICD_CTLR: u64 = gic::GICD_BASE + KVA;
    const GICR_WAKER: u64 = gic::GICR_BASE + KVA + 0x14;
    const SGI_BASE: u64 = gic::GICR_BASE + KVA + 0x1_0000;
    const GICR_IGROUPR0: u64 = SGI_BASE + 0x80;
    const GICR_ISENABLER0: u64 = SGI_BASE + 0x100;
    const POLL_LIMIT: u32 = 1_000_000;

    /// MMIO 레지스터 하나를 쓰는 함수입니다.
    ///
    /// # Safety
    /// `addr`는 디바이스로 매핑된 유효한 GIC 레지스터 주소여야 합니다.
    unsafe fn write32(addr: u64, v: u32) {
        unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
    }

    /// MMIO 레지스터 하나를 읽는 함수입니다.
    ///
    /// # Safety
    /// `addr`는 디바이스로 매핑된 유효한 GIC 레지스터 주소여야 합니다.
    unsafe fn read32(addr: u64) -> u32 {
        unsafe { core::ptr::read_volatile(addr as *const u32) }
    }

    pub(super) fn init() -> Result<(), IrqError> {
        // SAFETY: GICD/GICR는 enable_paging이 디바이스로 매핑해 둔 상태
        //         (호출 순서는 kernel_init이 강제)
        unsafe {
            // 디스트리뷰터: 어피니티 라우팅 + Group1 활성
            write32(GICD_CTLR, (1 << 4) | (1 << 1));
            let mut n = 0;
            while read32(GICD_CTLR) & (1 << 31) != 0 {
                n += 1;
                if n > POLL_LIMIT {
                    return Err(IrqError::GicRwpTimeout);
                }
            }

            // 리디스트리뷰터 웨이크업
            let waker = read32(GICR_WAKER) & !(1 << 1);
            write32(GICR_WAKER, waker);
            n = 0;
            while read32(GICR_WAKER) & (1 << 2) != 0 {
                n += 1;
                if n > POLL_LIMIT {
                    return Err(IrqError::GicWakeTimeout);
                }
            }

            // PPI 전부 Group1으로 두고 타이머 INTID만 활성
            write32(GICR_IGROUPR0, 0xFFFF_FFFF);
            write32(GICR_ISENABLER0, 1 << TIMER_INTID);
        }

        // CPU 인터페이스: SRE 활성 -> 우선순위 마스크 해제 -> Group1 활성
        // SAFETY: ICC 시스템 레지스터 접근, EL1에서 유효
        unsafe {
            let sre: u64;
            asm!("mrs {}, S3_0_C12_C12_5", out(reg) sre, options(nomem, nostack));
            asm!("msr S3_0_C12_C12_5, {}", "isb", in(reg) sre | 1, options(nomem, nostack));
            asm!("msr S3_0_C4_C6_0, {}", in(reg) 0xFFu64, options(nomem, nostack));
            asm!("msr S3_0_C12_C12_7, {}", "isb", in(reg) 1u64, options(nomem, nostack));
        }
        Ok(())
    }

    pub(super) fn ack() -> u64 {
        let iar: u64;
        // SAFETY: ICC_IAR1_EL1 읽기는 최고 우선순위 대기 인터럽트를 승인함
        unsafe { asm!("mrs {}, S3_0_C12_C12_0", out(reg) iar, options(nomem, nostack)) };
        iar
    }

    pub(super) fn eoi(iar: u64) {
        // SAFETY: ack에서 받은 값 그대로 완료 통지
        unsafe { asm!("msr S3_0_C12_C12_1, {}", in(reg) iar, options(nomem, nostack)) };
    }
}

fn timer_freq() -> u64 {
    let f: u64;
    // SAFETY: CNTFRQ_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
    f
}

fn timer_arm(interval: u64) {
    // SAFETY: EL1 물리 타이머 재장전과 활성화 (IMASK=0)
    unsafe {
        asm!(
            "msr cntp_tval_el0, {i}",
            "msr cntp_ctl_el0, {c}",
            i = in(reg) interval,
            c = in(reg) 1u64,
            options(nomem, nostack),
        );
    }
}

#[cfg(feature = "plat-apple")]
fn timer_pending() -> bool {
    let ctl: u64;
    // SAFETY: CNTP_CTL_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, cntp_ctl_el0", out(reg) ctl, options(nomem, nostack)) };
    ctl & (1 << 2) != 0 // ISTATUS
}

fn timer_tick() {
    let n = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    let mut con = EarlyCon;
    let _ = writeln!(con, "k0: tick {n}");
    timer_arm(timer_freq()); // 1초 주기 재장전 (레벨 인터럽트 해제도 겸함)
}

/// 인터럽트 컨트롤러와 타이머를 초기화하고 해당 마스크를 여는 함수입니다.
///
/// # Arguments
/// `_vectors` - 벡터 테이블이 먼저 설치됐음을 증명하는 토큰
///
/// # Errors
/// GIC 초기화 폴링이 한도를 넘으면 `IrqError`
pub fn init(_vectors: &Vectors) -> Result<Irq, IrqError> {
    #[cfg(feature = "plat-virt")]
    {
        gicv3::init()?;
        timer_arm(timer_freq());
        // SAFETY: 벡터와 GIC가 준비된 뒤의 IRQ + SError(A) 언마스크
        //         SError를 계속 마스크하면 커널 실행 중의 시스템 오류가 조용히
        //         pending으로 쌓였다가 EL0 진입 후 엉뚱한 문맥에서 터짐
        unsafe { asm!("msr DAIFClr, #6", options(nomem, nostack)) };
    }
    #[cfg(feature = "plat-apple")]
    {
        timer_arm(timer_freq());
        // SAFETY: Apple 타이머는 코어 FIQ 직결이라 FIQ + SError(A)만 언마스크
        unsafe { asm!("msr DAIFClr, #5", options(nomem, nostack)) };
    }
    Ok(Irq { _sealed: () })
}

/// 설계상 발생할 수 없는 인터럽트를 fail-secure로 정지시키는 함수입니다.
///
/// # Arguments
/// `kind` - 어느 경로로 들어왔는지 (irq / fiq)
/// `id` - 승인된 INTID (없으면 0)
fn unexpected(kind: &str, id: u64) -> ! {
    // SAFETY: 모든 예외 마스크는 fail-secure 정지의 전제 조건
    unsafe { asm!("msr DAIFSet, #0xf", options(nomem, nostack)) };
    let mut con = EarlyCon;
    let _ = writeln!(con, "k0: UNEXPECTED {kind} intid={id}");
    loop {
        // SAFETY: wfe는 대기만 한다
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

unsafe extern "C" {
    /// 타이머 선점 정책, 커널 바이너리가 정의함 (링크 계약)
    ///
    /// # Safety
    /// EL0에서 진입한 IRQ/FIQ 컨텍스트에서 벡터가 사용자 컨텍스트 저장을
    /// 마친 뒤에만 호출됩니다.
    fn k0_preempt();
}

/// EL1과 EL0 양쪽 IRQ 벡터가 공유하는 실제 분배 함수입니다.
///
/// 타이머 틱이었으면 true를 반환합니다(EL0 경로의 선점 판단용).
fn dispatch_irq() -> bool {
    #[cfg(feature = "plat-virt")]
    {
        let iar = gicv3::ack();
        match iar & 0xFF_FFFF {
            TIMER_INTID => {
                timer_tick();
                gicv3::eoi(iar);
                true
            }
            1020..=1023 => false, // 스퓨리어스: 승인 없이 복귀
            other => unexpected("irq", other),
        }
    }
    #[cfg(feature = "plat-apple")]
    unexpected("irq", 0); // AIC는 아직 초기화하지 않기 때문에 IRQ는 설계상 없음
}

/// EL1과 EL0 양쪽 FIQ 벡터가 공유하는 실제 분배 함수입니다.
///
/// 타이머 틱이었으면 true를 반환합니다(EL0 경로의 선점 판단용).
fn dispatch_fiq() -> bool {
    #[cfg(feature = "plat-apple")]
    {
        if timer_pending() {
            timer_tick();
            return true;
        }
        unexpected("fiq", 0);
    }
    #[cfg(feature = "plat-virt")]
    unexpected("fiq", 0); // virt에서 FIQ는 설계상 없음
}

/// 커널(EL1) 실행 중 IRQ 벡터(`vectors.S`)가 사용하는 디스패처 함수입니다.
///
/// 커널은 선점하지 않으므로 틱 여부는 버립니다.
#[unsafe(no_mangle)]
extern "C" fn irq_current() {
    let _ = dispatch_irq();
}

/// 커널(EL1) 실행 중 FIQ 벡터(`vectors.S`)가 사용하는 디스패처 함수입니다.
#[unsafe(no_mangle)]
extern "C" fn fiq_current() {
    let _ = dispatch_fiq();
}

/// 사용자 공간(EL0) 실행 중 IRQ 벡터가 사용하는 디스패처 함수입니다.
///
/// 타이머 틱이면 커널의 선점 정책을 부릅니다. 복귀는 벡터의
/// `__user_restore`가 (선점 정책이 갱신했을 수 있는) 현재 컨텍스트를
/// 복원하며 수행합니다.
#[unsafe(no_mangle)]
extern "C" fn irq_lower(_ctx: &mut crate::usermode::Context) {
    if dispatch_irq() {
        // SAFETY: 벡터가 사용자 컨텍스트 저장을 마친 EL0 IRQ 경로임
        unsafe { k0_preempt() };
    }
}

/// 사용자 공간(EL0) 실행 중 FIQ 벡터가 사용하는 디스패처 함수입니다.
#[unsafe(no_mangle)]
extern "C" fn fiq_lower(_ctx: &mut crate::usermode::Context) {
    if dispatch_fiq() {
        // SAFETY: 벡터가 사용자 컨텍스트 저장을 마친 EL0 FIQ 경로임
        unsafe { k0_preempt() };
    }
}
