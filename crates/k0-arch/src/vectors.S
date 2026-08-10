// K0-AArch64 예외 벡터 테이블 (진입 페이즈 2)
//
// 커널이 실제로 쓰는 조합만 복귀 가능한 경로를 가집니다.
//   - Current EL SP_ELx의 IRQ / FIQ: 호출자 보존 레지스터를 저장하고
//     Rust 핸들러(irq_current / fiq_current)를 부른 뒤 eret로 복귀
//   - 그 외 14개 벡터: fail-secure 진단 경로(exception_fatal)로 수렴,
//     망가졌을 수 있는 SP를 신뢰하지 않고 전용 예외 스택으로 전환
//   - 핸들러 실행 중 ELR/SPSR을 저장하지 않는 대신 중첩 예외를 허용하지
//     않습니다(핸들러 안의 동기 폴트는 fatal 경로로 빠져 복귀하지 않음).

.macro FATAL_ENTRY idx
    .p2align 7                          // 각 엔트리는 128바이트 슬롯
    mov     x0, #\idx
    b       __exc_fatal_entry
.endm

.macro IRQ_ENTRY handler
    .p2align 7
    sub     sp, sp, #176
    stp     x0, x1, [sp, #0]
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x29, [sp, #144]
    str     x30, [sp, #160]
    bl      \handler
    b       __exc_restore
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
    IRQ_ENTRY   irq_current             // Current EL, SP_ELx: IRQ
    IRQ_ENTRY   fiq_current             // Current EL, SP_ELx: FIQ
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
__exc_restore:
    ldp     x0, x1, [sp, #0]
    ldp     x2, x3, [sp, #16]
    ldp     x4, x5, [sp, #32]
    ldp     x6, x7, [sp, #48]
    ldp     x8, x9, [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x29, [sp, #144]
    ldr     x30, [sp, #160]
    add     sp, sp, #176
    eret

__exc_fatal_entry:
    adrp    x1, __exc_stack_top
    add     x1, x1, :lo12:__exc_stack_top
    mov     sp, x1
    bl      exception_fatal
0:  wfe                                 // exception_fatal은 발산하지만 방어적 파킹
    b       0b

// 예외 전용 스택 (부트 스택과 분리, .bss라 부팅 시 zeroize됨)
.section .bss
.p2align 4
__exc_stack_bottom:
    .skip 8192
__exc_stack_top:
