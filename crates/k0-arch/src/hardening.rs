//! 진입 페이즈 2의 PAC(포인터 인증)과 BTI 상태 점검 및 활성화 모듈입니다.
//!
//! # Features
//! ID 레지스터로 지원 여부를 실측하고, PAC이 있으면 부트 키를 주입한 뒤
//! `SCTLR_EL1`의 `EnIA`/`EnIB`/`EnDA`/`EnDB`를 활성화 합니다. 
//! 현재 stable Rust(1.89.0)는 branch-protection 코드젠이 
//! unstable이라서 컴파일러가 pac-ret 프롤로그와 `BTI` 랜딩 패드를 심지 
//! 못하기 때문에 하드웨어 상태만 준비합니다. BTI의 페이지 `GP` 비트는 랜딩 
//! 패드 없는 코드의 간접 분기를 전부 폴트로 만들기 때문에 코드젠이 가능해질 
//! 때까지 켜지 않습니다. 부트 키는 `CNTPCT` 기반 혼합값이며 진짜 엔트로피 
//! 소스 연결은 이후의 과제입니다.

use core::arch::asm;

/// 하드닝 기능의 실측 결과를 담는 구조체입니다.
pub struct Hardening {
    pub pac: bool,
    pub bti: bool,
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 지원되는 하드닝 기능을 실측하고 활성화하는 함수입니다.
pub fn enable() -> Hardening {
    let (isar1, pfr1): (u64, u64);
    // SAFETY: ID 레지스터 읽기는 부작용이 없음
    unsafe {
        asm!(
            "mrs {i}, id_aa64isar1_el1",
            "mrs {p}, id_aa64pfr1_el1",
            i = out(reg) isar1,
            p = out(reg) pfr1,
            options(nomem, nostack),
        );
    }

    let pac_addr = (isar1 >> 4) & 0xF != 0 || (isar1 >> 8) & 0xF != 0; // APA | API
    let pac_generic = (isar1 >> 24) & 0xF != 0 || (isar1 >> 28) & 0xF != 0; // GPA | GPI
    let bti = pfr1 & 0xF != 0; // BT

    if pac_addr {
        let mut seed: u64;
        // SAFETY: CNTPCT_EL0 읽기는 부작용이 없음
        unsafe { asm!("mrs {}, cntpct_el0", out(reg) seed, options(nomem, nostack)) };
        seed ^= 0xA076_1D64_78BD_642F;

        // 주소 인증 키 4쌍 (IA / IB / DA / DB)
        // SAFETY: pac_addr 확인 후에만 접근하기 때문에 키 레지스터가 존재함
        unsafe {
            asm!(
                "msr S3_0_C2_C1_0, {0}", "msr S3_0_C2_C1_1, {1}",
                "msr S3_0_C2_C1_2, {2}", "msr S3_0_C2_C1_3, {3}",
                "msr S3_0_C2_C2_0, {4}", "msr S3_0_C2_C2_1, {5}",
                "msr S3_0_C2_C2_2, {6}", "msr S3_0_C2_C2_3, {7}",
                in(reg) splitmix64(&mut seed), in(reg) splitmix64(&mut seed),
                in(reg) splitmix64(&mut seed), in(reg) splitmix64(&mut seed),
                in(reg) splitmix64(&mut seed), in(reg) splitmix64(&mut seed),
                in(reg) splitmix64(&mut seed), in(reg) splitmix64(&mut seed),
                options(nomem, nostack),
            );
        }
        if pac_generic {
            // SAFETY: pac_generic 확인 후에만 접근 (APGAKey)
            unsafe {
                asm!(
                    "msr S3_0_C2_C3_0, {0}", "msr S3_0_C2_C3_1, {1}",
                    in(reg) splitmix64(&mut seed), in(reg) splitmix64(&mut seed),
                    options(nomem, nostack),
                );
            }
        }

        // SCTLR_EL1: EnIA(31) EnIB(30) EnDA(27) EnDB(13)
        // SAFETY: 키 주입 뒤의 활성화, PAC 명령이 없는 현 코드에는 무영향
        unsafe {
            let mut sctlr: u64;
            asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack));
            sctlr |= (1 << 31) | (1 << 30) | (1 << 27) | (1 << 13);
            asm!("msr sctlr_el1, {}", "isb", in(reg) sctlr, options(nomem, nostack));
        }
    }

    Hardening { pac: pac_addr, bti }
}
