// K0-AArch64 진입 페이즈 0 코드 (QEMU virt 및 일반 ARM64)
//
// 기능에 다음 규칙이 적용됩니다.
//   - 부트로더는 Linux boot protocol에 따라 x0에 DTB 물리 주소를 담아
//     MMU/캐시 OFF 상태로 EL1 또는 EL2에서 진입
//   - 이 코드는 x0을 절대 변경하지 않은 채(x1, x2만 사용)
//     EL1h 스택 위에서 Rust의 `kernel_init(x0)`로 점프
//   - 지원하지 않는 상태(EL0/EL3 진입, kernel_init 복귀)는 전부
//     fail-secure: 저전력 파킹 루프(wfe)에서 영원히 대기

.section .text.boot, "ax"
.global _start

// Linux ARM64 Image 헤더(64바이트)
// QEMU와 m1n1 모두 이 헤더를 인식해 Linux 부트 프로토콜(x0=DTB)로 진입합니다
// __text_offset / __image_size / __image_flags 값은 링커 스크립트가 채웁니다
_start:
    b       phase0_entry                // code0: 헤더 선두의 실행 가능한 분기
    .long   0                           // code1
    .quad   __text_offset               // text_offset: RAM 시작 대비 로드 오프셋
    .quad   __image_size                // image_size: BSS와 부트 스택 포함
    .quad   __image_flags               // flags: LE | 페이지 크기 힌트
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
    // 1. 부트 코어 선별: 세컨더리 코어는 파킹
    //
    // 단일 코어로 시작(검증 범위 최소화)하며, SMP는 이후 명시적 wake로 확장됩니다.
    mrs     x1, MPIDR_EL1
    and     x1, x1, #0xff               // Aff0 (코어 번호)
    cbz     x1, 1f
0:  wfe                                 // fail-secure 파킹 루프
    b       0b

    //
    // 2. 현재 EL 판별
    //
1:  mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #1
    b.eq    el1_entry                   // 이미 EL1이면 바로 진행
    cmp     x1, #2
    b.ne    0b                          // EL0/EL3 진입은 미지원 -> 파킹

    //
    // 3. EL2 -> EL1 강하
    //
el2_to_el1:
    mov     x1, xzr
    orr     x1, x1, #(1 << 31)          // HCR_EL2.RW=1: EL1은 AArch64 모드

    // PAC 지원 코어면 EL1의 키 레지스터와 PAC 명령이 EL2로 트랩되지 않게 허용
    mrs     x2, ID_AA64ISAR1_EL1
    tst     x2, #0xFF0                  // APA | API
    b.eq    5f
    orr     x1, x1, #(3 << 40)          // HCR_EL2.APK | HCR_EL2.API
5:  msr     HCR_EL2, x1

    mrs     x1, CNTHCTL_EL2             // EL1의 물리 타이머/카운터 접근 허용
    orr     x1, x1, #0b11               // EL1PCEN | EL1PCTEN
    msr     CNTHCTL_EL2, x1
    msr     CNTVOFF_EL2, xzr            // 가상 카운터 오프셋 0

    mov     x1, #0x33ff                 // CPTR_EL2: FP/SIMD 트랩 비활성
    msr     CPTR_EL2, x1
    msr     HSTR_EL2, xzr               // EL1 시스템 레지스터 트랩 없음

    // SPSR = DAIF 마스크(0x3C0) | EL1h(0x5)
    ldr     x1, =0x3c5
    msr     SPSR_EL2, x1
    adr     x1, el1_entry
    msr     ELR_EL2, x1
    eret

    //
    // 4. EL1: SCTLR_EL1 정규화
    //
    // 진입 경로(EL1 직접 / EL2 강하)와 무관하게 부트로더가 남긴 상태를
    // 신뢰하지 않고 기지(known) 상태로 통일합니다. RES1 비트만 세팅되고
    // MMU/캐시 OFF, little-endian, EL0 WFx 트랩(nTWI/nTWE=0), EL0 DAIF
    // 접근 차단(UMA=0). W^X와 MMU 구성은 진입 페이즈 1의 Rust 코드가
    // 명시적으로 수행합니다.
el1_entry:
    ldr     x1, =0x30d00800
    msr     SCTLR_EL1, x1
    isb

    //
    // 5. 부트 스택 설정
    //
    adrp    x1, __boot_stack_top
    add     x1, x1, :lo12:__boot_stack_top
    mov     sp, x1

    //
    // 6. .bss 제로화
    //
    // Rust는 static 초기값 0을 가정하므로 kernel_init 진입 전 필수
    // 링커 스크립트가 __bss_start/__bss_end를 16바이트 정렬로 보장
    adrp    x1, __bss_start
    add     x1, x1, :lo12:__bss_start
    adrp    x2, __bss_end
    add     x2, x2, :lo12:__bss_end
2:  cmp     x1, x2
    b.hs    3f
    stp     xzr, xzr, [x1], #16
    b       2b

    //
    // 7. Rust로 이양
    //
    // x0 = DTB 물리 주소
3:  bl      kernel_init

    // kernel_init은 발산(-> !) 함수지만 방어적으로 파킹
4:  wfe
    b       4b
