// K0-AArch64 예외 벡터 테이블
//
// 현재는 16개 벡터 전부가 fail-secure 진단 경로로 수렴합니다.
//   - 어떤 예외든 전용 예외 스택으로 전환한 뒤 exception_fatal(kind)를 호출
//   - 스택 오버플로(가드 페이지 폴트) 중에도 보고가 가능해야 하므로
//     망가졌을 수 있는 기존 SP를 신뢰하지 않습니다
//   - IRQ 라우팅은 GIC 초기화(진입 페이즈 2 후속)와 함께 확장됩니다

.macro FATAL_ENTRY idx
    .p2align 7                          // 각 엔트리는 128바이트 슬롯
    mov     x0, #\idx
    b       __exc_fatal_entry
.endm

.section .text.vectors, "ax"
.p2align 11                             // VBAR_EL1은 2KiB 정렬 요구
.global __vectors
__vectors:
    FATAL_ENTRY 0                       // Current EL, SP_EL0: Sync
    FATAL_ENTRY 1                       // Current EL, SP_EL0: IRQ
    FATAL_ENTRY 2                       // Current EL, SP_EL0: FIQ
    FATAL_ENTRY 3                       // Current EL, SP_EL0: SError
    FATAL_ENTRY 4                       // Current EL, SP_ELx: Sync
    FATAL_ENTRY 5                       // Current EL, SP_ELx: IRQ
    FATAL_ENTRY 6                       // Current EL, SP_ELx: FIQ
    FATAL_ENTRY 7                       // Current EL, SP_ELx: SError
    FATAL_ENTRY 8                       // Lower EL, AArch64: Sync
    FATAL_ENTRY 9                       // Lower EL, AArch64: IRQ
    FATAL_ENTRY 10                      // Lower EL, AArch64: FIQ
    FATAL_ENTRY 11                      // Lower EL, AArch64: SError
    FATAL_ENTRY 12                      // Lower EL, AArch32: Sync
    FATAL_ENTRY 13                      // Lower EL, AArch32: IRQ
    FATAL_ENTRY 14                      // Lower EL, AArch32: FIQ
    FATAL_ENTRY 15                      // Lower EL, AArch32: SError

.p2align 2
__exc_fatal_entry:
    adrp    x1, __exc_stack_top
    add     x1, x1, :lo12:__exc_stack_top
    mov     sp, x1
    bl      exception_fatal
0:  wfe                                 // exception_fatal은 발산하지만 방어적인 파킹
    b       0b

// 예외 전용 스택(부트 스택과 분리, .bss라서 부팅 시 소거됨)
.section .bss
.p2align 4
__exc_stack_bottom:
    .skip 8192
__exc_stack_top:
