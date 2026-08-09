//! 진입 페이즈 2의 예외 벡터 설치와 fail-secure 예외 진단 모듈입니다.
//!
//! # Features
//! `VBAR_EL1`에 벡터 테이블(`vectors.S`)을 설치합니다. 현재 단계에서는 모든
//! 예외가 신드롬(ESR/ELR/FAR/SPSR)을 출력하고 정지하는 fatal 경로입니다.
//! 벡터가 설치되기 전의 폴트는 진단 불가능한 행이 되므로 부트 경로에서
//! 최대한 일찍 설치해야 합니다. IRQ 분배는 GIC 초기화와 함께 확장됩니다.

use core::arch::asm;
use core::fmt::Write;

use crate::earlycon::EarlyCon;

core::arch::global_asm!(include_str!("vectors.S"));

unsafe extern "C" {
    static __vectors: u8;
}

/// 벡터 테이블이 설치됐음을 증명하는 typestate 토큰 구조체입니다.
///
/// 진입 페이즈 2의 후속 단계(GIC, 타이머)가 이 토큰을 입력으로 받게 됩니다.
pub struct Vectors {
    _sealed: (),
}

/// VBAR_EL1에 벡터 테이블을 설치하는 함수입니다.
pub fn install() -> Vectors {
    let vbar = &raw const __vectors as u64;
    // SAFETY: __vectors는 2KiB 정렬된 벡터 테이블이고 현재 주소 공간에서
    //         실행 가능한 .text에 있음(identity 매핑이라 MMU 전환 후에도 유효)
    unsafe { asm!("msr vbar_el1, {}", "isb", in(reg) vbar, options(nomem, nostack)) };
    Vectors { _sealed: () }
}

const KIND_NAMES: [&str; 16] = [
    "sync-sp0", "irq-sp0", "fiq-sp0", "serror-sp0",
    "sync-spx", "irq-spx", "fiq-spx", "serror-spx",
    "sync-a64", "irq-a64", "fiq-a64", "serror-a64",
    "sync-a32", "irq-a32", "fiq-a32", "serror-a32",
];

fn ec_name(ec: u64) -> &'static str {
    match ec {
        0x00 => "unknown",
        0x15 => "svc64",
        0x18 => "sysreg",
        0x20 => "iabort-lower",
        0x21 => "iabort",
        0x22 => "pc-align",
        0x24 => "dabort-lower",
        0x25 => "dabort",
        0x26 => "sp-align",
        0x2F => "serror",
        0x30..=0x34 => "debug",
        _ => "?",
    }
}

/// 모든 벡터가 수렴하는 fail-secure 진단 종착점 함수입니다.
///
/// 진입 시점에 SP는 전용 예외 스택(`__exc_stack_top`)으로 교체되어
/// 있기 때문에 스택 오버플로 폴트 중에도 안전하게 출력할 수 있습니다.
///
/// # Arguments
/// `kind` - 벡터 인덱스(0..16), 어느 엔트리로 진입했는지
#[unsafe(no_mangle)]
extern "C" fn exception_fatal(kind: u64) -> ! {
    let (esr, elr, far, spsr): (u64, u64, u64, u64);
    // SAFETY: 예외 신드롬 레지스터 읽기는 부작용이 없다
    unsafe {
        asm!(
            "mrs {esr}, esr_el1",
            "mrs {elr}, elr_el1",
            "mrs {far}, far_el1",
            "mrs {spsr}, spsr_el1",
            esr = out(reg) esr,
            elr = out(reg) elr,
            far = out(reg) far,
            spsr = out(reg) spsr,
            options(nomem, nostack),
        );
    }

    let name = KIND_NAMES.get(kind as usize).copied().unwrap_or("?");
    let ec = (esr >> 26) & 0x3F;
    let mut con = EarlyCon;
    let _ = writeln!(con, "k0: EXCEPTION {name}");
    let _ = writeln!(
        con,
        "k0: esr={esr:#x} (ec={ec:#x} {}) iss={:#x}",
        ec_name(ec),
        esr & 0x1FF_FFFF
    );
    let _ = writeln!(con, "k0: elr={elr:#x} far={far:#x} spsr={spsr:#x}");

    loop {
        // SAFETY: wfe는 대기만 함
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
