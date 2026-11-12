//! 진입 페이즈 2의 인터럽트 컨트롤러와 제네릭 타이머 모듈입니다.
//!
//! # Features
//! 커널이 받는 인터럽트는 EL1 가상 타이머의 PPI 하나뿐입니다. 컨트롤러는
//! `GICv3`(시스템 레지스터 CPU 인터페이스)와 `GICv2`(메모리 맵 CPU
//! 인터페이스)를 지원하며, 어느 쪽인지와 레지스터 주소, 타이머의 INTID는
//! DTB에서 발견한 값을 호출자가 넘겨줍니다. 드라이버는 쓰기 전에 하드웨어가
//! 그 기술과 맞는지 식별 레지스터로 확인하고, 펌웨어가 남긴 활성/대기 상태를
//! 정리한 뒤, 타이머 PPI 하나만 켭니다. 가상 타이머를 쓰는 이유는 하이퍼바이저
//! 게스트에서도 항상 쓸 수 있는 타이머가 그것뿐이기 때문입니다.
//!
//! 설계상 발생할 수 없는 FIQ와 미지의 `INTID`는 fail-secure로 정지합니다.
//!
//! 틱 주기는 1초입니다. 틱은 선점 신호이면서 IPC 타임아웃의 단위이고, EL0가
//! 얻을 수 있는 유일한 시간 정보입니다(EL0는 카운터와 타이머 레지스터를 읽지
//! 못함). 주기를 줄이면 EL0에 더 정밀한 시계를 주게 되므로, 주기를 바꾸는
//! 것은 성능 조정이 아니라 사이드 채널에 관한 결정입니다. 틱은 처리된 횟수를
//! 세는 것이지 흐른 시간을 재는 것이 아닙니다. 커널이 인터럽트를 막은 채
//! 한 주기보다 오래 실행되면 그 사이의 틱은 한 번으로 셉니다.
//!
//! # Errors
//! 하드웨어가 기술과 다르거나, 타이머 PPI를 이 보안 상태에서 쓸 수 없거나,
//! 레지스터 반영 대기가 한도를 넘으면 `IrqError`로 반환하며 호출자는 부팅을
//! 중단해야 합니다.

use core::arch::asm;
use core::fmt::Write;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

#[cfg(any(feature = "console-pl011", feature = "console-16550"))]
use crate::earlycon::ConsoleOnly;
use crate::earlycon::EarlyCon;
use crate::vectors::Vectors;

/// 인터럽트 경로가 열렸음을 증명하는 typestate 토큰 구조체입니다.
pub struct Irq {
    _sealed: (),
}

/// 인터럽트 초기화가 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqError {
    /// 식별 레지스터가 기술된 GIC 버전과 다름
    WrongGic,
    /// 이 코어에 시스템 레지스터 CPU 인터페이스가 없거나 켤 수 없음
    NoSystemRegisters,
    /// GICv2로 기술됐는데 시스템 레지스터 인터페이스가 켜져 있음
    SystemRegistersActive,
    GicRwpTimeout,
    GicWakeTimeout,
    /// 부트 코어의 리디스트리뷰터를 찾지 못했거나 둘 이상 찾음
    NoRedistributor,
    /// 리디스트리뷰터 프레임을 들여다볼 수 없음(매핑 실패)
    ProbeFailed,
    /// 타이머 PPI를 켤 수 없음(이 보안 상태의 소유가 아님)
    PpiNotOwned,
    /// 타이머 PPI가 레벨 트리거가 아니고 바꿀 수도 없음
    PpiNotLevel,
    /// 우선순위 마스크를 열 수 없음
    PmrStuck,
    /// 타이머 주파수를 알 수 없음
    NoFrequency,
    /// 타이머의 INTID가 PPI 범위(16..=31) 밖
    BadIntid,
}

const KIND_NONE: u8 = 0;
const KIND_V2: u8 = 2;
const KIND_V3: u8 = 3;

static KIND: AtomicU8 = AtomicU8::new(KIND_NONE);
/// GICv2 CPU 인터페이스 레지스터의 가상 주소(런타임의 승인과 완료 통지에 씀)
static GICC: AtomicUsize = AtomicUsize::new(0);
static TIMER_INTID: AtomicU32 = AtomicU32::new(0);
static TIMER_HZ: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

/// 로그에 남기는 틱 줄의 수. 이후의 틱은 콘솔에만 찍음
const LOGGED_TICKS: u64 = 4;

const POLL_LIMIT: u32 = 1_000_000;

/// 타이머 PPI에 주는 우선순위와 우선순위 마스크. 비보안 상태에서 쓰면 GIC가
/// 값을 옮겨 저장하지만 "PPI가 마스크보다 높은 우선순위"라는 관계는 유지됨
const PRIORITY: u32 = 0xA0;
const PRIORITY_MASK: u64 = 0xF0;

/// # Safety
/// `addr`는 디바이스로 매핑된 유효한 레지스터 주소여야 합니다.
unsafe fn write32(addr: usize, v: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}

/// # Safety
/// `addr`는 디바이스로 매핑된 유효한 레지스터 주소여야 합니다.
unsafe fn read32(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// # Safety
/// `addr`는 디바이스로 매핑된 유효한 레지스터 주소여야 합니다.
unsafe fn write8(addr: usize, v: u8) {
    unsafe { core::ptr::write_volatile(addr as *mut u8, v) }
}

/// 레지스터의 `mask` 비트가 내려갈 때까지 기다리는 함수입니다.
///
/// # Safety
/// `addr`는 디바이스로 매핑된 유효한 레지스터 주소여야 합니다.
unsafe fn wait_clear(addr: usize, mask: u32, err: IrqError) -> Result<(), IrqError> {
    for _ in 0..POLL_LIMIT {
        if unsafe { read32(addr) } & mask == 0 {
            return Ok(());
        }
    }
    Err(err)
}

/// 이 코어의 어피니티(Aff3:Aff2:Aff1:Aff0)를 주는 함수입니다.
/// GICR_TYPER의 상위 32비트와 같은 배치입니다.
pub fn affinity() -> u32 {
    let mpidr: u64;
    // SAFETY: MPIDR_EL1 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack)) };
    (((mpidr >> 32) & 0xFF) << 24) as u32 | (mpidr & 0xFF_FFFF) as u32
}

fn has_gic_sysregs() -> bool {
    let pfr0: u64;
    // SAFETY: ID 레지스터 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, id_aa64pfr0_el1", out(reg) pfr0, options(nomem, nostack)) };
    (pfr0 >> 24) & 0xF != 0
}

/// GICv3 레지스터 지도와 초기화입니다.
pub mod gicv3 {
    use super::*;

    const GICD_CTLR: usize = 0x0000;
    const GICD_TYPER: usize = 0x0004;
    const GICD_ICENABLER: usize = 0x0180;
    const GICD_ICPENDR: usize = 0x0280;
    const GICD_ICACTIVER: usize = 0x0380;
    /// 식별 레지스터 PIDR2가 프레임 안에서 놓이는 위치
    pub const PIDR2: u64 = 0xFFE8;
    pub const GICR_TYPER: u64 = 0x0008;
    const GICR_CTLR: usize = 0x0000;
    const GICR_WAKER: usize = 0x0014;
    /// SGI_base 프레임은 RD_base 프레임 바로 뒤 64KiB
    pub const SGI_OFFSET: u64 = 0x1_0000;
    const GICR_IGROUPR0: usize = 0x0080;
    const GICR_ISENABLER0: usize = 0x0100;
    const GICR_ICENABLER0: usize = 0x0180;
    const GICR_ICPENDR0: usize = 0x0280;
    const GICR_ICACTIVER0: usize = 0x0380;
    const GICR_IPRIORITYR: usize = 0x0400;
    const GICR_ICFGR1: usize = 0x0C04;
    const GICR_IGRPMODR0: usize = 0x0D00;

    const CTLR_RWP: u32 = 1 << 31;
    const CTLR_ARE_NS: u32 = 1 << 4;
    const CTLR_DS: u32 = 1 << 6;
    const CTLR_GRP1NS: u32 = 1 << 1;

    /// 초기화에 쓰는 레지스터 페이지들의 가상 주소입니다. 각 값은 해당 프레임의
    /// 첫 바이트(오프셋 0)에 대응하는 주소입니다.
    pub struct Frames {
        /// GICD 프레임의 시작
        pub dist: usize,
        /// GICD의 PIDR2 레지스터 주소
        pub dist_pidr2: usize,
        /// 부트 코어의 RD_base 프레임 시작
        pub rd: usize,
        /// 부트 코어의 SGI_base 프레임 시작
        pub sgi: usize,
    }

    /// PIDR2의 아키텍처 리비전이 GICv3 또는 GICv4인지 확인하는 함수입니다.
    ///
    /// # Safety
    /// `pidr2`는 GICv3 프레임의 PIDR2가 Device로 매핑된 주소여야 합니다.
    pub unsafe fn revision_ok(pidr2: usize) -> bool {
        matches!((unsafe { read32(pidr2) } >> 4) & 0xF, 3 | 4)
    }

    /// 부트 코어의 리디스트리뷰터를 찾는 함수입니다.
    ///
    /// DTB가 준 구간을 프레임 단위로 걸으며 `GICR_TYPER`의 어피니티가 이 코어와
    /// 같은 프레임을 찾습니다. 구간을 통째로 매핑하지 않도록, 레지스터 하나의
    /// 물리 주소를 받아 읽을 수 있는 가상 주소를 돌려주는 `probe`를 통해서만
    /// 읽습니다. 정확히 하나를 찾아야 하며 그 RD_base의 물리 주소를 반환합니다.
    ///
    /// # Arguments
    /// `regions` - 리디스트리뷰터 구간들의 (물리 시작, 크기)
    /// `stride` - DTB가 준 프레임 간격, 0이면 프레임의 VLPIS 비트로 정함
    /// `probe` - 물리 주소를 잠시 매핑해 가상 주소를 주는 함수
    ///
    /// # Errors
    /// 식별 불일치는 `WrongGic`, 매핑 실패는 `ProbeFailed`, 일치하는 프레임이
    /// 없거나 둘 이상이면 `NoRedistributor`
    pub fn find_redistributor(
        regions: &[(u64, u64)],
        stride: u64,
        probe: &mut dyn FnMut(u64) -> Option<usize>,
    ) -> Result<u64, IrqError> {
        /// 구간 하나에서 훑는 프레임 수의 상한
        const MAX_FRAMES: u32 = 1024;
        let want = affinity();
        let mut found: Option<u64> = None;
        // 실패했을 때 진단에 쓰는 값: 훑은 프레임 수와 마지막으로 읽은 어피니티
        let (mut frames, mut seen) = (0u32, 0u32);

        for &(base, size) in regions {
            let end = base + size;
            let mut frame = base;
            for _ in 0..MAX_FRAMES {
                if frame + 2 * SGI_OFFSET > end {
                    break;
                }
                // 프레임마다 식별 레지스터를 확인함. 구간의 첫 프레임이 GICv3가
                // 아니면 기술이 틀린 것이고, 그 뒤의 프레임이 아니면 실제
                // 리디스트리뷰터가 거기서 끝난 것임(0으로 읽히는 빈 자리가
                // 어피니티 0과 같아 보이는 일을 막음)
                let pidr2 = probe(frame + PIDR2).ok_or(IrqError::ProbeFailed)?;
                // SAFETY: probe가 방금 Device로 매핑해 준 주소
                if !unsafe { revision_ok(pidr2) } {
                    if frame == base {
                        return Err(IrqError::WrongGic);
                    }
                    break;
                }
                let typer = probe(frame + GICR_TYPER).ok_or(IrqError::ProbeFailed)?;
                // 64비트 레지스터를 한 번에 읽음. 절반씩 읽으면 상위 절반을 0으로
                // 돌려주는 가상 GIC가 있음
                // SAFETY: probe가 방금 Device로 매핑해 준 64비트 레지스터
                let value = unsafe { core::ptr::read_volatile(typer as *const u64) };
                let (lo, hi) = (value as u32, (value >> 32) as u32);
                frames += 1;
                seen = hi;
                if hi == want {
                    if let Some(first) = found {
                        let _ = writeln!(
                            EarlyCon,
                            "k0: redistributors at {first:#x} and {frame:#x} both claim cpu affinity {want:#x}"
                        );
                        return Err(IrqError::NoRedistributor);
                    }
                    found = Some(frame);
                }
                // Last(비트 4)는 이 구간의 마지막 프레임, VLPIS(비트 1)는 프레임이
                // 64KiB 넷으로 이루어진 GICv4를 뜻함
                if lo & (1 << 4) != 0 {
                    break;
                }
                frame += match stride {
                    0 if lo & (1 << 1) != 0 => 4 * SGI_OFFSET,
                    0 => 2 * SGI_OFFSET,
                    s => s,
                };
            }
        }
        if found.is_none() {
            let _ = writeln!(
                EarlyCon,
                "k0: no redistributor for cpu affinity {want:#x} ({frames} frames, last {seen:#x})"
            );
        }
        found.ok_or(IrqError::NoRedistributor)
    }

    /// GICv3를 초기화하고 타이머 PPI 하나를 켜는 함수입니다.
    ///
    /// # Safety
    /// `f`의 주소들은 해당 프레임이 Device로 매핑된 주소여야 하고, `f.rd`는
    /// 이 코어의 리디스트리뷰터여야 합니다.
    pub(super) unsafe fn init(f: &Frames, intid: u32) -> Result<(), IrqError> {
        // ID 레지스터의 GIC 필드는 보지 않음. 시스템 레지스터 인터페이스가
        // 동작하는데도 그 필드를 0으로 보고하는 하이퍼바이저가 있음. 쓸 수
        // 있는지는 아래에서 SRE를 켜고 읽어 보는 것으로 판정함
        let bit = 1u32 << intid;
        // SAFETY: 함수 계약대로 매핑된 GIC 레지스터에 대한 접근
        unsafe {
            if !revision_ok(f.dist_pidr2) {
                return Err(IrqError::WrongGic);
            }

            // 디스트리뷰터: 끈 상태에서 SPI를 전부 정리한 뒤 어피니티 라우팅과
            // 비보안 Group 1만 켬. 비트 0은 보안 상태가 하나뿐인 GIC에서 Group 0을
            // 뜻하므로 쓰지 않음
            // 어피니티 라우팅(ARE)을 켜진 상태에서 끄는 것은 결과가 정해지지
            // 않은 전이라, 펌웨어가 켜 둔 ARE와 DS는 그대로 두고 그룹만 끔
            let keep = read32(f.dist + GICD_CTLR) & (CTLR_ARE_NS | CTLR_DS);
            write32(f.dist + GICD_CTLR, keep);
            wait_clear(f.dist + GICD_CTLR, CTLR_RWP, IrqError::GicRwpTimeout)?;
            let lines = (read32(f.dist + GICD_TYPER) & 0x1F) as usize;
            for n in 1..=lines {
                write32(f.dist + GICD_ICENABLER + 4 * n, !0);
                write32(f.dist + GICD_ICPENDR + 4 * n, !0);
                write32(f.dist + GICD_ICACTIVER + 4 * n, !0);
            }
            wait_clear(f.dist + GICD_CTLR, CTLR_RWP, IrqError::GicRwpTimeout)?;
            // ARE는 그룹이 꺼진 동안에만 켤 수 있으므로 먼저 ARE만, 그다음 그룹
            write32(f.dist + GICD_CTLR, (keep & CTLR_DS) | CTLR_ARE_NS);
            wait_clear(f.dist + GICD_CTLR, CTLR_RWP, IrqError::GicRwpTimeout)?;
            write32(f.dist + GICD_CTLR, (keep & CTLR_DS) | CTLR_ARE_NS | CTLR_GRP1NS);
            wait_clear(f.dist + GICD_CTLR, CTLR_RWP, IrqError::GicRwpTimeout)?;
            if read32(f.dist + GICD_CTLR) & CTLR_ARE_NS == 0 {
                return Err(IrqError::WrongGic);
            }

            // 리디스트리뷰터 웨이크업 (비보안에서 못 쓰는 구성이면 읽기가 0이라
            // 그대로 통과하고, 아래의 켜기 읽어 보기가 사용 가능 여부를 가림)
            let waker = read32(f.rd + GICR_WAKER);
            if waker & (1 << 1) != 0 {
                write32(f.rd + GICR_WAKER, waker & !(1 << 1));
            }
            wait_clear(f.rd + GICR_WAKER, 1 << 2, IrqError::GicWakeTimeout)?;

            // 이 코어의 SGI와 PPI: 펌웨어가 켜 두었거나 활성으로 남긴 것을 지움
            write32(f.sgi + GICR_ICENABLER0, !0);
            wait_clear(f.rd + GICR_CTLR, 1 << 3, IrqError::GicRwpTimeout)?;
            write32(f.sgi + GICR_ICPENDR0, !0);
            write32(f.sgi + GICR_ICACTIVER0, !0);
            write32(f.sgi + GICR_IGROUPR0, !0);
            write32(f.sgi + GICR_IGRPMODR0, 0);
            write8(f.sgi + GICR_IPRIORITYR + intid as usize, PRIORITY as u8);

            // 타이머는 레벨 인터럽트여야 함. 설정은 꺼진 동안에만 바꿀 수 있음
            let cfg_bit = 1u32 << (2 * (intid % 16) + 1);
            if read32(f.sgi + GICR_ICFGR1) & cfg_bit != 0 {
                let v = read32(f.sgi + GICR_ICFGR1) & !cfg_bit;
                write32(f.sgi + GICR_ICFGR1, v);
                if read32(f.sgi + GICR_ICFGR1) & cfg_bit != 0 {
                    return Err(IrqError::PpiNotLevel);
                }
            }
        }

        // CPU 인터페이스: 시스템 레지스터 사용을 켜고(읽어 보기로 확인), 펌웨어가
        // 남겼을 수 있는 실행 우선순위와 분할 설정을 지운 뒤 Group 1을 켬
        // SAFETY: ICC 시스템 레지스터 접근. 디스트리뷰터의 식별 레지스터가
        //         GICv3임을 확인한 뒤이고, 인터페이스가 없는 코어라면 미정의
        //         명령 예외로 진단을 남기고 정지함
        unsafe {
            let mut sre: u64;
            asm!("mrs {}, S3_0_C12_C12_5", out(reg) sre, options(nomem, nostack));
            asm!("msr S3_0_C12_C12_5, {}", "isb", in(reg) sre | 1, options(nomem, nostack));
            asm!("mrs {}, S3_0_C12_C12_5", out(reg) sre, options(nomem, nostack));
            if sre & 1 == 0 {
                return Err(IrqError::NoSystemRegisters);
            }
            // ICC_CTLR_EL1: EOImode = 0이라 완료 통지 하나로 우선순위 강하와
            // 비활성화가 함께 일어남. PRIbits(10:8)로 활성 우선순위 레지스터 수를 앎
            asm!("msr S3_0_C12_C12_4, {}", in(reg) 0u64, options(nomem, nostack));
            let ctlr: u64;
            asm!("mrs {}, S3_0_C12_C12_4", out(reg) ctlr, options(nomem, nostack));
            let pribits = ((ctlr >> 8) & 0x7) + 1;
            asm!("msr S3_0_C12_C9_0, {}", in(reg) 0u64, options(nomem, nostack)); // AP1R0
            if pribits >= 6 {
                asm!("msr S3_0_C12_C9_1, {}", in(reg) 0u64, options(nomem, nostack));
            }
            if pribits >= 7 {
                asm!("msr S3_0_C12_C9_2, {}", in(reg) 0u64, options(nomem, nostack));
                asm!("msr S3_0_C12_C9_3, {}", in(reg) 0u64, options(nomem, nostack));
            }
            asm!("msr S3_0_C12_C12_3, {}", in(reg) 0u64, options(nomem, nostack)); // BPR1
            asm!("msr S3_0_C4_C6_0, {}", in(reg) PRIORITY_MASK, options(nomem, nostack)); // PMR
            asm!("msr S3_0_C12_C12_7, {}", "isb", in(reg) 1u64, options(nomem, nostack)); // IGRPEN1
        }

        // SAFETY: 함수 계약대로 매핑된 리디스트리뷰터 레지스터
        unsafe {
            write32(f.sgi + GICR_ISENABLER0, bit);
            // 보안 상태가 소유한 PPI는 비보안에서 켜지지 않고 0으로 읽힘
            if read32(f.sgi + GICR_ISENABLER0) & bit == 0 {
                return Err(IrqError::PpiNotOwned);
            }
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

/// GICv2 레지스터 지도와 초기화입니다.
pub mod gicv2 {
    use super::*;

    const GICD_CTLR: usize = 0x000;
    const GICD_TYPER: usize = 0x004;
    const GICD_IGROUPR: usize = 0x080;
    const GICD_ISENABLER: usize = 0x100;
    const GICD_ICENABLER: usize = 0x180;
    const GICD_ICPENDR: usize = 0x280;
    const GICD_ICACTIVER: usize = 0x380;
    const GICD_IPRIORITYR: usize = 0x400;
    const GICD_ICFGR: usize = 0xC00;
    const GICD_PIDR2: usize = 0xFE8;
    const GICC_CTLR: usize = 0x00;
    const GICC_PMR: usize = 0x04;
    const GICC_BPR: usize = 0x08;
    pub(super) const GICC_IAR: usize = 0x0C;
    pub(super) const GICC_EOIR: usize = 0x10;
    const GICC_APR: usize = 0xD0;
    const GICC_IIDR: usize = 0xFC;

    /// GICv2를 초기화하고 타이머 PPI 하나를 켜는 함수입니다.
    ///
    /// # Safety
    /// `dist`와 `cpu`는 각각 GICD와 GICC 프레임의 시작이 Device로 매핑된
    /// 주소여야 합니다.
    pub(super) unsafe fn init(dist: usize, cpu: usize, intid: u32) -> Result<(), IrqError> {
        // 시스템 레지스터 인터페이스가 켜진 코어에서는 메모리 맵 CPU 인터페이스가
        // 동작하지 않음
        if has_gic_sysregs() {
            let sre: u64;
            // SAFETY: ID 레지스터로 ICC_SRE_EL1의 존재를 확인함
            unsafe { asm!("mrs {}, S3_0_C12_C12_5", out(reg) sre, options(nomem, nostack)) };
            if sre & 1 != 0 {
                return Err(IrqError::SystemRegistersActive);
            }
        }
        let bit = 1u32 << intid;
        // SAFETY: 함수 계약대로 매핑된 GIC 레지스터에 대한 접근
        unsafe {
            // 식별: 디스트리뷰터의 아키텍처 리비전과 CPU 인터페이스의 아키텍처 버전
            let rev = (read32(dist + GICD_PIDR2) >> 4) & 0xF;
            let arch = (read32(cpu + GICC_IIDR) >> 16) & 0xF;
            if !matches!(rev, 1 | 2) || !matches!(arch, 1 | 2) {
                return Err(IrqError::WrongGic);
            }

            write32(dist + GICD_CTLR, 0);
            let typer = read32(dist + GICD_TYPER);
            let lines = (typer & 0x1F) as usize;
            for n in 0..=lines {
                write32(dist + GICD_ICENABLER + 4 * n, !0);
                write32(dist + GICD_ICPENDR + 4 * n, !0);
                write32(dist + GICD_ICACTIVER + 4 * n, !0);
            }
            // 보안 확장이 없는 GIC에서는 타이머를 Group 0에 두어 IRQ로 받음.
            // 보안 확장이 있으면 이 레지스터는 비보안에서 읽기 0 / 쓰기 무시이고
            // 펌웨어가 PPI를 비보안에 넘겨 두었어야 함
            if typer & (1 << 10) == 0 {
                write32(dist + GICD_IGROUPR, 0);
            }
            write8(dist + GICD_IPRIORITYR + intid as usize, PRIORITY as u8);
            let cfg_bit = 1u32 << (2 * (intid % 16) + 1);
            if read32(dist + GICD_ICFGR + 4) & cfg_bit != 0 {
                return Err(IrqError::PpiNotLevel);
            }

            // CPU 인터페이스: 우회 비활성 비트(5..=8)는 보존하고 비트 0만 켬.
            // AckCtl, FIQEn, CBPR, EOImode는 전부 0
            write32(cpu + GICC_PMR, PRIORITY_MASK as u32);
            if read32(cpu + GICC_PMR) == 0 {
                return Err(IrqError::PmrStuck);
            }
            write32(cpu + GICC_BPR, 0);
            for n in 0..4 {
                write32(cpu + GICC_APR + 4 * n, 0);
            }
            let ctlr = read32(cpu + GICC_CTLR) & 0x1E0;
            write32(cpu + GICC_CTLR, ctlr | 1);
            write32(dist + GICD_CTLR, 1);

            write32(dist + GICD_ISENABLER, bit);
            if read32(dist + GICD_ISENABLER) & bit == 0 {
                return Err(IrqError::PpiNotOwned);
            }
        }
        Ok(())
    }
}

/// 초기화할 인터럽트 컨트롤러와 그 레지스터의 가상 주소입니다.
pub enum Gic {
    V2 { dist: usize, cpu: usize },
    V3(gicv3::Frames),
}

/// 카운터 주파수를 정하는 함수입니다.
///
/// 펌웨어가 `CNTFRQ_EL0`를 채웠으면 그 값을, 0이면 DTB의 값을 씁니다.
///
/// # Errors
/// 둘 다 없거나 쓸 수 없는 값이면 `NoFrequency`. 하한은 1kHz이고, 상한은 1초
/// 간격이 타이머의 부호 있는 32비트 TVAL에 들어가야 해서 2^31 미만입니다
pub fn counter_hz(dt_hz: u32) -> Result<u64, IrqError> {
    let f: u64;
    // SAFETY: CNTFRQ_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
    let hz = if f & 0xFFFF_FFFF != 0 { f & 0xFFFF_FFFF } else { u64::from(dt_hz) };
    if (1_000..1u64 << 31).contains(&hz) {
        Ok(hz)
    } else {
        Err(IrqError::NoFrequency)
    }
}

fn timer_arm(interval: u64) {
    // SAFETY: EL1 가상 타이머 재장전과 활성화 (IMASK=0)
    unsafe {
        asm!(
            "msr cntv_tval_el0, {i}",
            "msr cntv_ctl_el0, {c}",
            i = in(reg) interval,
            c = in(reg) 1u64,
            options(nomem, nostack),
        );
    }
}

fn timer_tick() {
    let n = TICKS.load(Ordering::Relaxed) + 1;
    TICKS.store(n, Ordering::Relaxed);
    // 틱은 끝없이 이어지므로 처음 몇 줄만 로그에 남김
    if n <= LOGGED_TICKS {
        let _ = writeln!(EarlyCon, "k0: tick {n}");
    } else {
        // 콘솔 드라이버가 없는 빌드에는 쓸 곳이 없으므로 서식 작업도 하지 않음
        #[cfg(any(feature = "console-pl011", feature = "console-16550"))]
        let _ = writeln!(ConsoleOnly, "k0: tick {n}");
    }
    // 1초 주기 재장전 (레벨 인터럽트의 원인 제거도 겸함)
    timer_arm(TIMER_HZ.load(Ordering::Relaxed));
}

/// 부팅 뒤로 지나간 타이머 틱 수를 주는 함수입니다.
///
/// IPC 타임아웃의 기한이 이 값을 기준으로 정해집니다.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// 실행할 태스크가 없을 때 다음 인터럽트까지 코어를 재우고 그 인터럽트를
/// 처리하는 함수입니다.
///
/// IRQ를 마스크한 채로 `wfi`를 실행하므로 깨어날 조건을 확인한 뒤 잠들기
/// 전에 인터럽트가 지나가 버리는 틈이 없습니다(`wfi`는 마스크와 무관하게
/// 대기 중인 인터럽트가 있으면 끝남). 깨어난 뒤에 IRQ 마스크를 잠깐 풀어
/// EL1 벡터가 그 인터럽트를 처리하게 하고 다시 마스크합니다. 다른 마스크
/// 비트는 건드리지 않습니다.
///
/// 이 어셈블리 블록에는 `nomem`과 `nostack`을 붙이지 않습니다. 블록 안에서
/// 인터럽트 핸들러가 실행되어 틱 수를 비롯한 메모리를 바꾸고 현재 스택 아래를
/// 쓰기 때문입니다. 붙이면 컴파일러가 이 호출을 건너 틱 수 읽기를 옮기거나
/// 합칠 수 있습니다.
///
/// # Safety
/// EL0에서 진입한 예외의 핸들러 안에서만 호출해야 합니다. 그 시점에는 사용자
/// 컨텍스트(ELR/SPSR 포함)가 TCB에 저장돼 있어 EL1 IRQ가 ELR_EL1/SPSR_EL1을
/// 덮어써도 잃는 것이 없습니다. 커널 스택에는 벡터가 쓸 176바이트가 남아
/// 있어야 합니다.
pub unsafe fn wait_for_interrupt() {
    // SAFETY: 함수 계약대로 중첩 IRQ가 안전한 문맥이고, 마스크는 진입 때와
    //         같은 상태로 되돌림
    unsafe {
        asm!("wfi", "msr DAIFClr, #2", "isb", "msr DAIFSet, #2");
    }
}

/// 인터럽트 컨트롤러와 타이머를 초기화하고 해당 마스크를 여는 함수입니다.
///
/// # Arguments
/// `_vectors` - 벡터 테이블이 먼저 설치됐음을 증명하는 토큰
/// `gic` - 컨트롤러의 종류와 레지스터 주소
/// `intid` - EL1 가상 타이머의 INTID
/// `hz` - 카운터 주파수([counter_hz]의 결과)
///
/// # Safety
/// `gic`의 주소들은 DTB에서 발견해 메모리 맵과의 대조를 마친 디바이스가
/// Device로 매핑된 주소여야 합니다.
///
/// # Errors
/// 하드웨어가 기술과 다르거나 타이머 PPI를 쓸 수 없으면 `IrqError`
pub unsafe fn init(_vectors: &Vectors, gic: &Gic, intid: u32, hz: u64) -> Result<Irq, IrqError> {
    if !(16..=31).contains(&intid) {
        return Err(IrqError::BadIntid);
    }
    // 초기화가 끝나기 전에 타이머가 대기 상태로 남아 있지 않게 먼저 끔
    // SAFETY: EL1 가상 타이머 비활성화
    unsafe { asm!("msr cntv_ctl_el0, {}", in(reg) 0u64, options(nomem, nostack)) };
    match gic {
        // SAFETY: 함수 계약을 그대로 전달
        Gic::V3(frames) => unsafe {
            gicv3::init(frames, intid)?;
            KIND.store(KIND_V3, Ordering::Relaxed);
        },
        // SAFETY: 함수 계약을 그대로 전달
        Gic::V2 { dist, cpu } => unsafe {
            gicv2::init(*dist, *cpu, intid)?;
            GICC.store(*cpu, Ordering::Relaxed);
            KIND.store(KIND_V2, Ordering::Relaxed);
        },
    }
    TIMER_INTID.store(intid, Ordering::Relaxed);
    TIMER_HZ.store(hz, Ordering::Relaxed);
    timer_arm(hz);
    // SAFETY: 벡터와 GIC가 준비된 뒤의 IRQ + SError(A) 언마스크
    //         SError를 계속 마스크하면 커널 실행 중의 시스템 오류가 조용히
    //         pending으로 쌓였다가 EL0 진입 후 엉뚱한 문맥에서 터짐
    unsafe { asm!("msr DAIFClr, #6", options(nomem, nostack)) };
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
    let timer = u64::from(TIMER_INTID.load(Ordering::Relaxed));
    match KIND.load(Ordering::Relaxed) {
        KIND_V3 => {
            let iar = gicv3::ack();
            match iar & 0xFF_FFFF {
                id if id == timer => {
                    timer_tick();
                    gicv3::eoi(iar);
                    true
                }
                1020..=1023 => false, // 스퓨리어스: 승인 없이 복귀
                other => unexpected("irq", other),
            }
        }
        KIND_V2 => {
            let cpu = GICC.load(Ordering::Relaxed);
            // SAFETY: init이 저장한 GICC 프레임의 매핑된 주소
            let iar = unsafe { read32(cpu + gicv2::GICC_IAR) };
            match u64::from(iar & 0x3FF) {
                id if id == timer => {
                    timer_tick();
                    // SAFETY: 승인에서 받은 값 그대로 완료 통지
                    unsafe { write32(cpu + gicv2::GICC_EOIR, iar) };
                    true
                }
                1020..=1023 => false,
                other => unexpected("irq", other),
            }
        }
        _ => unexpected("irq", 0),
    }
}

/// EL1과 EL0 양쪽 FIQ 벡터가 공유하는 실제 분배 함수입니다.
///
/// 커널은 모든 인터럽트를 IRQ로만 받으므로 FIQ는 설계상 없고, 들어오면
/// fail-secure로 정지합니다.
fn dispatch_fiq() -> bool {
    unexpected("fiq", 0)
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
