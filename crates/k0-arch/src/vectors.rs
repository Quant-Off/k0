//! 진입 페이즈 2의 예외 벡터 설치와 fail-secure 예외 진단 모듈입니다.
//!
//! # Features
//! `VBAR_EL1`에 벡터 테이블(`vectors.S`)을 설치합니다. 현재 단계에서는 모든
//! 예외가 신드롬(ESR/ELR/FAR/SPSR)을 출력하고 정지하는 fatal 경로입니다.
//! 벡터가 설치되기 전의 폴트는 진단 불가능한 행이 되므로 부트 경로에서
//! 최대한 일찍 설치해야 합니다. IRQ 분배는 GIC 초기화와 함께 확장됩니다.

use core::arch::asm;

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

fn kind_name(kind: u64) -> &'static str {
    match kind {
        0 => "sync-sp0",
        1 => "irq-sp0",
        2 => "fiq-sp0",
        3 => "serror-sp0",
        4 => "sync-spx",
        5 => "irq-spx",
        6 => "fiq-spx",
        7 => "serror-spx",
        8 => "sync-a64",
        9 => "irq-a64",
        10 => "fiq-a64",
        11 => "serror-a64",
        12 => "sync-a32",
        13 => "irq-a32",
        14 => "fiq-a32",
        15 => "serror-a32",
        _ => "?",
    }
}

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

    // fmt를 쓰지 않는 raw 출력: higher-half 점프 전의 폴트에서도 동작해야 함
    let ec = (esr >> 26) & 0x3F;
    let mut con = EarlyCon;
    con.put_str("k0: EXCEPTION ");
    con.put_str(kind_name(kind));
    con.put_str("\nk0: esr = ");
    con.put_hex(esr);
    con.put_str(" ec ");
    con.put_str(ec_name(ec));
    con.put_str("\nk0: elr = ");
    con.put_hex(elr);
    con.put_str(" far = ");
    con.put_hex(far);
    con.put_str(" spsr = ");
    con.put_hex(spsr);
    con.put_str("\n");

    loop {
        // SAFETY: wfe는 대기만 함
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
