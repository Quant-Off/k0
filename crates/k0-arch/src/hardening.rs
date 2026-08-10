//! 진입 페이즈 2의 PAC(포인터 인증)/BTI/PAN 상태 점검 및 활성화 모듈입니다.
//!
//! # Features
//! ID 레지스터로 지원 여부를 실측하고, PAC이 있으면 부트 키를 주입한 뒤
//! `SCTLR_EL1`의 `EnIA`/`EnIB`/`EnDA`/`EnDB`를 활성화 합니다.
//! 현재 stable Rust(1.89.0)는 branch-protection 코드젠이
//! unstable이라서 컴파일러가 pac-ret 프롤로그와 `BTI` 랜딩 패드를 심지
//! 못하기 때문에 하드웨어 상태만 준비합니다. BTI의 페이지 `GP` 비트는 랜딩
//! 패드 없는 코드의 간접 분기를 전부 폴트로 만들기 때문에 코드젠이 가능해질
//! 때까지 켜지 않습니다. 부트 키는 호출자가 파생해 전달합니다(k0-boot의
//! SHA-256 KDF가 DTB `/chosen` 엔트로피 + CNTPCT + RNDR을 혼합). 키 품질은
//! 부트로더가 제공하는 엔트로피에 좌우되며, 저엔트로피 여부는 호출자가
//! 로그로 드러냅니다.
//!
//! PAN(FEAT_PAN)이 있으면 `PSTATE.PAN`을 켜고 `SCTLR_EL1.SPAN`을 내려
//! 이후 모든 EL1 예외 진입에서 PAN이 자동 설정되게 합니다. 커널이 사용자
//! 매핑(EL0 접근 가능)을 사용자 VA로 역참조하면 하드웨어가 거부합니다.
//! 커널은 사용자 프레임을 EL1 전용 윈도우 별칭으로만 만지기 때문에 현재
//! 코드에는 영향이 없고, 이후 사용자 버퍼를 읽는 시스템 콜은 비특권
//! 접근(LDTR/STTR) 복사 루틴을 거쳐야 합니다.

use core::arch::asm;

/// 하드닝 기능의 실측 결과를 담는 구조체입니다.
///
/// `pan2`는 PAN을 반영하는 AT 변형(S1E1RP/S1E1WP)의 존재를 뜻하며 자가
/// 검증에 사용됩니다. (일반 AT S1E1R/W는 PSTATE.PAN을 무시함)
pub struct Hardening {
    pub pac: bool,
    pub bti: bool,
    pub pan: bool,
    pub pan2: bool,
}

/// EL0로 새는 관측/디버그 표면을 기지 상태로 정규화하는 함수입니다.
///
/// 부트로더가 남긴 리셋값을 신뢰하지 않습니다. `CNTKCTL_EL1`(EL0의 카운터/
/// 타이머 접근)과 `MDSCR_EL1`(디버그)은 0으로 내리고, `PMUSERENR_EL0`(EL0의
/// PMU 접근)은 표준 PMUv3가 구현된 경우에만 0으로 내립니다. (PMUVer가 0
/// 또는 0xF(비표준)이면 레지스터 접근 자체가 미정의라서 건드리지 않음)
fn normalize_el0_exposure() {
    // SAFETY: 아키텍처 필수 레지스터의 EL0 접근 비트를 전부 차단, 부작용은
    //         EL0 접근 거부(트랩)뿐이고 커널(EL1) 동작에는 영향 없음
    unsafe {
        asm!(
            "msr cntkctl_el1, xzr",
            "msr mdscr_el1, xzr",
            options(nomem, nostack),
        );
    }

    let dfr0: u64;
    // SAFETY: ID 레지스터 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, id_aa64dfr0_el1", out(reg) dfr0, options(nomem, nostack)) };
    let pmuver = (dfr0 >> 8) & 0xF;
    if pmuver != 0 && pmuver != 0xF {
        // SAFETY: 표준 PMUv3 확인 후에만 접근함
        unsafe { asm!("msr pmuserenr_el0, xzr", options(nomem, nostack)) };
    }

    // SAFETY: 시스템 레지스터 변경 반영
    unsafe { asm!("isb", options(nomem, nostack)) };
}

/// FEAT_RNG의 RNDR로 하드웨어 난수 하나를 시도하는 함수입니다.
///
/// # Errors
/// 미지원 코어(ID_AA64ISAR0.RNDR == 0)나 난수 생성 실패(Z 플래그)면 `None`
pub fn rndr() -> Option<u64> {
    let isar0: u64;
    // SAFETY: ID 레지스터 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, id_aa64isar0_el1", out(reg) isar0, options(nomem, nostack)) };
    if (isar0 >> 60) & 0xF == 0 {
        return None;
    }

    let (v, ok): (u64, u64);
    // SAFETY: RNDR 지원 확인 후 접근, 실패는 Z 플래그로 보고되기 때문에 같은
    //         asm 블록 안에서 cset으로 회수함 (S3_3_C2_C4_0 = RNDR)
    unsafe {
        asm!(
            "mrs {v}, S3_3_C2_C4_0",
            "cset {ok}, ne",
            v = out(reg) v,
            ok = out(reg) ok,
            options(nomem, nostack),
        );
    }
    (ok == 1).then_some(v)
}

/// 지원되는 하드닝 기능을 실측하고 활성화하는 함수입니다.
///
/// # Arguments
/// `pac_keys` - 파생을 마친 PAC 키 10워드(IA/IB/DA/DB 쌍 8개 + GA 쌍 2개),
/// PAC 미지원 코어에서는 사용되지 않음
pub fn enable(pac_keys: &[u64; 10]) -> Hardening {
    let (isar1, pfr1, mmfr1): (u64, u64, u64);
    // SAFETY: ID 레지스터 읽기는 부작용이 없음
    unsafe {
        asm!(
            "mrs {i}, id_aa64isar1_el1",
            "mrs {p}, id_aa64pfr1_el1",
            "mrs {m}, id_aa64mmfr1_el1",
            i = out(reg) isar1,
            p = out(reg) pfr1,
            m = out(reg) mmfr1,
            options(nomem, nostack),
        );
    }

    let pac_addr = (isar1 >> 4) & 0xF != 0 || (isar1 >> 8) & 0xF != 0; // APA | API
    let pac_generic = (isar1 >> 24) & 0xF != 0 || (isar1 >> 28) & 0xF != 0; // GPA | GPI
    let bti = pfr1 & 0xF != 0; // BT
    let pan_level = (mmfr1 >> 20) & 0xF; // PAN (1=v8.1, 2=PAN2, 3=PAN3)
    let pan = pan_level >= 1;
    let pan2 = pan_level >= 2;

    normalize_el0_exposure();

    if pac_addr {
        // 주소 인증 키 4쌍 (IA / IB / DA / DB)
        // SAFETY: pac_addr 확인 후에만 접근하기 때문에 키 레지스터가 존재함
        unsafe {
            asm!(
                "msr S3_0_C2_C1_0, {0}", "msr S3_0_C2_C1_1, {1}",
                "msr S3_0_C2_C1_2, {2}", "msr S3_0_C2_C1_3, {3}",
                "msr S3_0_C2_C2_0, {4}", "msr S3_0_C2_C2_1, {5}",
                "msr S3_0_C2_C2_2, {6}", "msr S3_0_C2_C2_3, {7}",
                in(reg) pac_keys[0], in(reg) pac_keys[1],
                in(reg) pac_keys[2], in(reg) pac_keys[3],
                in(reg) pac_keys[4], in(reg) pac_keys[5],
                in(reg) pac_keys[6], in(reg) pac_keys[7],
                options(nomem, nostack),
            );
        }
        if pac_generic {
            // SAFETY: pac_generic 확인 후에만 접근 (APGAKey)
            unsafe {
                asm!(
                    "msr S3_0_C2_C3_0, {0}", "msr S3_0_C2_C3_1, {1}",
                    in(reg) pac_keys[8], in(reg) pac_keys[9],
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

    if pan {
        // SPAN(23)=0: 이후 모든 EL1 예외 진입에서 PSTATE.PAN 자동 설정
        // .inst는 `msr pan, #1`의 raw 인코딩, 베이스라인(v8.0) 어셈블러가
        // named PSTATE를 거부해서 사용함 (실행은 pan 확인 뒤라 안전)
        // SAFETY: FEAT_PAN 확인 후에만 실행되고 커널은 사용자 VA를 역참조하지 않음
        unsafe {
            let mut sctlr: u64;
            asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack));
            sctlr &= !(1u64 << 23);
            asm!("msr sctlr_el1, {}", "isb", in(reg) sctlr, options(nomem, nostack));
            asm!(".inst 0xd500419f", options(nomem, nostack)); // msr pan, #1
        }
    }

    Hardening {
        pac: pac_addr,
        bti,
        pan,
        pan2,
    }
}
