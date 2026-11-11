// K0-AArch64 진입 페이즈 0 코드
//
// 기능에 다음 규칙이 적용됩니다.
//   - 부트로더는 Linux boot protocol에 따라 x0에 DTB 물리 주소를 담아
//     MMU/캐시 OFF 상태로 EL1 또는 EL2에서 진입
//   - 진입하는 CPU는 하나뿐이어야 함(규약). 어느 CPU인지는 가리지 않음
//   - 이 코드는 x0을 절대 변경하지 않은 채(x1..x5만 사용)
//     EL1h 스택 위에서 Rust의 `kernel_init(x0)`로 점프
//   - 이미지는 어느 물리 주소에 적재돼도 되므로 모든 주소는 PC 상대로 구함
//   - 지원하지 않는 상태(EL0/EL3 진입, kernel_init 복귀)는 전부
//     fail-secure: 저전력 파킹 루프(wfe)에서 영원히 대기

.section .text.boot, "ax"
.global _start

// Linux ARM64 Image 헤더(64바이트)
// 부트로더는 이 헤더를 인식해 Linux 부트 프로토콜(x0=DTB)로 진입합니다
// __text_offset / __image_size / __image_flags 값은 링커 스크립트가 채웁니다
_start:
    b       phase0_entry                // code0: 헤더 선두의 실행 가능한 분기
    .long   0                           // code1
    .quad   __text_offset               // text_offset: 2MiB 경계 대비 적재 오프셋
    .quad   __image_size                // image_size: BSS와 부트 스택 포함
    .quad   __image_flags               // flags: LE | 페이지 크기 | 임의 물리 배치
    .quad   0                           // res2
    .quad   0                           // res3
    .quad   0                           // res4
    .long   0x644d5241                  // magic: "ARM\x64" (LE)
    .long   0                           // res5

phase0_entry:
    //
    // 0. 모든 예외 마스크 (D/A/I/F)
    //
    // 예외 벡터(VBAR_EL1)가 설치되기 전의 예외는 정의되지 않은 동작이므로
    // 가장 먼저 전부 마스크합니다.
    msr     DAIFSet, #0xf

    //
    // 1. 적재 주소 확인과 현재 EL 판별
    //
    // 4KiB 경계에 맞지 않게 적재되면 PC 상대 주소 계산(adrp)이 전부 어긋나
    // 어떤 코드도 믿을 수 없으므로 아무것도 하지 않고 정지합니다. 그래뉼
    // 경계 검사는 Rust 쪽이 사유를 남기며 수행합니다. EL은 x19에 보관했다가
    // .bss 소거 뒤에 기록합니다.
    adr     x1, _start
    tst     x1, #0xfff
    b.ne    0f
    mrs     x19, CurrentEL
    lsr     x19, x19, #2
    cmp     x19, #1
    b.eq    el1_entry
    cmp     x19, #2
    b.eq    el2_entry
0:  wfe                                 // EL0/EL3 진입은 미지원 -> 파킹
    b       0b

    //
    // 2. EL2 초기화 후 EL1로 강하
    //
    // 커널은 하이퍼바이저가 아니므로 EL2에 다시 돌아오지 않습니다. 강하한 뒤
    // 커널(EL1)이 실제로 하는 동작과 사용자(EL0)가 할 수 있는 동작이 EL2로
    // 트랩되지 않도록, 그에 관여하는 EL2 레지스터는 펌웨어가 남긴 값(리셋 값이
    // UNKNOWN인 것 포함)을 신뢰하지 않고 직접 씁니다. 커널이 쓰지 않는 기능의
    // 레지스터(MPAM, SPE, BRBE 등)는 건드리지 않으며, 그런 기능을 EL1이
    // 건드리면 EL2 벡터에서 정지합니다. 즉치는 movz/movk로 만들어 리터럴 풀을
    // 읽지 않습니다.
el2_entry:
    // 2-1. 벡터: 이후 어떤 이유로든 EL2에 들어오면 원인을 기록하고 정지
    adr     x1, el2_vectors
    msr     VBAR_EL2, x1
    isb

    // 2-2. HCR_EL2: EL1은 AArch64(RW), hvc는 EL1에서 미정의 명령(HCD, 커널은
    // hvc를 쓰지 않음), PAC 키 레지스터와 명령은 트랩 없음(APK|API). 나머지
    // 트랩과 라우팅은 전부 0
    mov     x1, #(1 << 31)
    orr     x1, x1, #(1 << 29)
    orr     x1, x1, #(3 << 40)
    // E2H가 RES1인 코어(ID_AA64MMFR4_EL1.E2H0 < 0)는 E2H를 켠 값으로 씀
    mrs     x2, S3_0_C0_C7_4
    sbfx    x2, x2, #24, #4
    cmp     x2, #0
    b.ge    1f
    orr     x1, x1, #(1 << 34)
1:  msr     HCR_EL2, x1
    isb

    // 2-3. E2H의 실제 상태 판별 (x3 = 1이면 E2H가 켜진 배치)
    // E2H를 끄려 해도 꺼지지 않는 코어가 있고 읽어 보기로는 알 수 없으므로,
    // EL1 이름의 레지스터 접근이 EL2 레지스터로 돌려지는지를 FAR로 확인함
    mov     x3, #0
    tbnz    x1, #34, 2f
    mov     x2, #1
    msr     FAR_EL1, x2
    isb
    mov     x2, #2
    msr     FAR_EL2, x2
    isb
    mrs     x2, FAR_EL1
    cmp     x2, #2
    b.ne    3f
    orr     x1, x1, #(1 << 34)
    msr     HCR_EL2, x1
    isb
2:  mov     x3, #1
3:
    // 2-4. SCTLR_EL2: RES1 비트만, MMU OFF, little-endian
    movz    x2, #0x0830
    movk    x2, #0x30c5, lsl #16
    msr     SCTLR_EL2, x2
    isb

    // 2-5. 타이머: EL1의 물리 타이머/카운터 접근을 허용하고 나머지 트랩 비트는
    // 0으로 전부 덮어씀. 비트 위치는 E2H에 따라 다름. 가상 카운터 오프셋 0
    mov     x2, #3
    cbz     x3, 4f
    lsl     x2, x2, #10
4:  msr     CNTHCTL_EL2, x2
    msr     CNTVOFF_EL2, xzr

    // 2-6. EL1이 읽는 MIDR/MPIDR의 실체는 VPIDR/VMPIDR이라 실제 값을 복사
    mrs     x2, MIDR_EL1
    msr     VPIDR_EL2, x2
    mrs     x2, MPIDR_EL1
    msr     VMPIDR_EL2, x2

    // 2-7. CPTR_EL2: FP/SVE/SME를 EL2에서는 트랩하지 않음. EL1의 CPACR_EL1 = 0이
    // 이들을 EL1로 트랩하고 그 태스크만 종료됨. 배치는 E2H에 따라 다름
    mrs     x4, S3_0_C0_C4_0            // ID_AA64PFR0_EL1
    mrs     x5, S3_0_C0_C4_1            // ID_AA64PFR1_EL1
    cbnz    x3, 6f
    mov     x2, #0x33ff
    ubfx    x1, x4, #32, #4             // SVE
    cbz     x1, 5f
    bic     x2, x2, #(1 << 8)           // TZ
5:  ubfx    x1, x5, #24, #4             // SME
    cbz     x1, 8f
    bic     x2, x2, #(1 << 12)          // TSM
    b       8f
6:  mov     x2, #(3 << 20)              // FPEN
    ubfx    x1, x4, #32, #4
    cbz     x1, 7f
    orr     x2, x2, #(3 << 16)          // ZEN
7:  ubfx    x1, x5, #24, #4
    cbz     x1, 8f
    orr     x2, x2, #(3 << 24)          // SMEN
8:  msr     CPTR_EL2, x2

    // 2-8. MDCR_EL2: 디버그/PMU의 EL2 트랩과 라우팅 전부 해제. HPMN은 PMU가
    // 있으면 카운터 수(PMCR_EL0.N)로 채움
    mov     x2, xzr
    mrs     x1, S3_0_C0_C5_0            // ID_AA64DFR0_EL1
    ubfx    x1, x1, #8, #4              // PMUVer
    cbz     x1, 9f
    cmp     x1, #0xf
    b.eq    9f
    mrs     x2, PMCR_EL0
    ubfx    x2, x2, #11, #5
9:  msr     MDCR_EL2, x2
    msr     HSTR_EL2, xzr               // EL1 시스템 레지스터 트랩 없음
    msr     VTTBR_EL2, xzr              // stage 2 없음

    // 2-9. GICv3 CPU 인터페이스: EL1이 ICC 시스템 레지스터를 쓸 수 있게 SRE와
    // Enable을 켬. SRE가 켜지지 않으면(펌웨어가 GICv2 방식으로 운용) 그대로 둠
    ubfx    x1, x4, #24, #4             // ID_AA64PFR0_EL1.GIC
    cbz     x1, 10f
    mrs     x2, S3_4_C12_C9_5           // ICC_SRE_EL2
    orr     x2, x2, #1
    orr     x2, x2, #(1 << 3)
    msr     S3_4_C12_C9_5, x2
    isb
    mrs     x2, S3_4_C12_C9_5
    tbz     x2, #0, 10f
    msr     S3_4_C12_C11_0, xzr         // ICH_HCR_EL2
10:
    // 2-10. HCRX_EL2 = 0: TCR2_EL1/SCTLR2_EL1은 0으로 취급되고, 여기서 켜는
    // 기능은 없음. EL0의 해당 명령들은 SCTLR_EL1의 허용 비트가 0이라 EL1로 감
    mrs     x1, S3_0_C0_C7_1            // ID_AA64MMFR1_EL1
    ubfx    x1, x1, #40, #4             // HCX
    cbz     x1, 11f
    msr     S3_4_C1_C2_2, xzr
11:
    // 2-11. 세부 트랩(FGT): 1이면 트랩인 비트는 전부 0. 0이면 트랩인 비트는
    // EL0에서 닿을 수 있는 레지스터와 그 짝(SME, PIE, POE, GCS)에 한해, 기능이
    // 구현돼 있을 때 1로 써서 EL2 트랩을 끔. 커널이 쓰지 않는 EL1 전용
    // 레지스터의 트랩은 그대로 남음
    mrs     x1, S3_0_C0_C7_0            // ID_AA64MMFR0_EL1
    ubfx    x1, x1, #56, #4             // FGT
    cbz     x1, 17f
    mov     x2, xzr
    ubfx    x4, x5, #24, #4             // SME
    cbz     x4, 12f
    orr     x2, x2, #(3 << 54)          // nSMPRI_EL1 | nTPIDR2_EL0
12: mrs     x4, S3_0_C0_C7_3            // ID_AA64MMFR3_EL1
    ubfx    x4, x4, #8, #4              // S1PIE
    cbz     x4, 13f
    orr     x2, x2, #(3 << 57)          // nPIRE0_EL1 | nPIR_EL1
13: mrs     x4, S3_0_C0_C7_3
    ubfx    x4, x4, #16, #4             // S1POE
    cbz     x4, 14f
    orr     x2, x2, #(1 << 59)          // nPOR_EL0
14: ubfx    x4, x5, #44, #4             // GCS
    cbz     x4, 15f
    orr     x2, x2, #(3 << 52)          // nGCS_EL0 | nGCS_EL1
15: msr     S3_4_C1_C1_4, x2            // HFGRTR_EL2
    msr     S3_4_C1_C1_5, x2            // HFGWTR_EL2
    msr     S3_4_C1_C1_6, xzr           // HFGITR_EL2
    msr     S3_4_C3_C1_4, xzr           // HDFGRTR_EL2
    msr     S3_4_C3_C1_5, xzr           // HDFGWTR_EL2
    mrs     x4, S3_0_C0_C4_0
    ubfx    x4, x4, #44, #4             // AMU
    cbz     x4, 16f
    msr     S3_4_C3_C1_6, xzr           // HAFGRTR_EL2
16: cmp     x1, #2
    b.lt    17f
    msr     S3_4_C3_C1_2, xzr           // HFGRTR2_EL2
    msr     S3_4_C3_C1_3, xzr           // HFGWTR2_EL2
    msr     S3_4_C3_C1_7, xzr           // HFGITR2_EL2
    msr     S3_4_C3_C1_0, xzr           // HDFGRTR2_EL2
    msr     S3_4_C3_C1_1, xzr           // HDFGWTR2_EL2
17:
    // 2-12. EL1의 SCTLR을 강하 전에 기지 상태로: MMU 허용 비트가 UNKNOWN인 채
    // EL1에 들어가지 않게 함. E2H 배치에서는 EL1 이름이 EL2로 돌려지므로
    // SCTLR_EL12로 씀
    movz    x2, #0x0818
    movk    x2, #0x30d0, lsl #16
    cbnz    x3, 18f
    msr     SCTLR_EL1, x2
    b       19f
18: msr     S3_5_C1_C0_0, x2            // SCTLR_EL12
19:
    // SPSR = DAIF 마스크(0x3C0) | EL1h(0x5)
    mov     x2, #0x3c5
    msr     SPSR_EL2, x2
    adr     x2, el1_entry
    msr     ELR_EL2, x2
    isb
    eret

    //
    // 3. EL1: PSTATE.SP / SCTLR_EL1 / CPACR_EL1 정규화
    //
    // 진입 경로(EL1 직접 / EL2 강하)와 무관하게 부트로더가 남긴 상태를
    // 신뢰하지 않고 기지(known) 상태로 통일합니다. RES1 비트 외에는 스택
    // 정렬 검사(SA/SA0=1, EL1과 EL0 모두 sp가 16바이트 정렬이 아니면
    // 즉시 폴트)만 켜고 MMU/캐시 OFF, little-endian, EL0 WFx 트랩
    // (nTWI/nTWE=0), EL0 DAIF 접근 차단(UMA=0). EL0에 기능을 열어 주는 비트
    // (EnALS, EnASR, EnAS0, EnTP2, EnFPM, UCI, UCT, DZE)는 전부 0이고 TSCXT는
    // 1이라, 그런 명령과 레지스터는 EL0에서 EL1로 트랩되거나 미정의 명령이
    // 됩니다. W^X와 MMU 구성은 진입 페이즈 1의 Rust 코드가 명시적으로
    // 수행합니다.
    //
    // CPACR_EL1 = 0: FP/SIMD(FPEN), SVE(ZEN), SME(SMEN) 접근을 EL0/EL1
    // 모두 트랩. 커널은 softfloat이고 태스크 컨텍스트에 벡터 레지스터
    // 파일이 없으므로, 부트로더가 켜 두었더라도 EL0가 벡터 상태를 다른
    // 태스크에 흘리지 못하게 명시적으로 내립니다. EL0의 사용은 폴트로
    // 트랩되어 그 태스크만 종료됩니다.
el1_entry:
    msr     SPSel, #1                   // EL1t로 진입시킨 부트로더 대비
    movz    x1, #0x0818
    movk    x1, #0x30d0, lsl #16
    msr     SCTLR_EL1, x1
    msr     CPACR_EL1, xzr
    isb

    //
    // 4. 부트 스택 설정
    //
    adrp    x1, __boot_stack_top
    add     x1, x1, :lo12:__boot_stack_top
    mov     sp, x1

    //
    // 5. .bss 제로화
    //
    // Rust는 static 초기값 0을 가정하므로 kernel_init 진입 전 필수
    // 링커 스크립트가 __bss_start/__bss_end를 16바이트 정렬로 보장
    //
    // 먼저 .bss부터 이미지 끝(부트 스택 포함)까지의 데이터 캐시 라인을
    // 무효화합니다. 캐시가 꺼진 동안의 쓰기는 메모리로 바로 가는데, 펌웨어가
    // 이 구간에 남긴 낡은 라인이 나중에 밀려 나오며 덮어쓰지 못하게 합니다.
    adrp    x1, __bss_start
    add     x1, x1, :lo12:__bss_start
    adrp    x2, __kernel_end
    add     x2, x2, :lo12:__kernel_end
    mrs     x3, CTR_EL0
    ubfx    x3, x3, #16, #4             // DminLine: 워드 수의 log2
    mov     x4, #4
    lsl     x4, x4, x3                  // 캐시 라인 바이트 수
    sub     x5, x4, #1
    bic     x1, x1, x5
24: dc      ivac, x1
    add     x1, x1, x4
    cmp     x1, x2
    b.lo    24b
    dsb     sy
    isb

    adrp    x1, __bss_start
    add     x1, x1, :lo12:__bss_start
    adrp    x2, __bss_end
    add     x2, x2, :lo12:__bss_end
20: cmp     x1, x2
    b.hs    21f
    stp     xzr, xzr, [x1], #16
    b       20b

    //
    // 6. 진입 EL을 기록하고 Rust로 이양
    //
    // x0 = DTB 물리 주소
21: adrp    x1, K0_ENTRY_EL
    str     x19, [x1, :lo12:K0_ENTRY_EL]
    bl      kernel_init

    // kernel_init은 발산(-> !) 함수지만 방어적으로 파킹
22: wfe
    b       22b

    //
    // EL2 벡터
    //
    // 강하 뒤에 EL2로 들어오는 경로는 설계상 없습니다. 그래도 들어왔다면
    // (모르는 기능의 트랩, 펌웨어의 설정) 계속 실행할 근거가 없으므로 원인을
    // 기록하고 정지합니다. 기록은 EL2의 PC 상대 주소(물리 주소)로 남기며
    // 디버거나 메모리 덤프로 읽습니다.
el2_trap:
    adrp    x1, K0_EL2_TRAP
    add     x1, x1, :lo12:K0_EL2_TRAP
    mrs     x2, ESR_EL2
    str     x2, [x1]
    mrs     x2, ELR_EL2
    str     x2, [x1, #8]
    mrs     x2, FAR_EL2
    str     x2, [x1, #16]
23: wfe
    b       23b

    .balign 2048
el2_vectors:
    .rept 16
    b       el2_trap
    .balign 128
    .endr
