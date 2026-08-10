// K0-AArch64 예외 벡터 테이블 (진입 페이즈 2 + 3)
//
// 커널이 실제로 쓰는 조합만 복귀 가능한 경로를 가집니다.
//   - Current EL SP_ELx의 IRQ / FIQ: 호출자 보존 레지스터를 저장하고
//     Rust 핸들러(irq_current / fiq_current)를 부른 뒤 eret로 복귀
//   - Lower EL AArch64의 Sync / IRQ / FIQ: 사용자 컨텍스트 전체를
//     __current_context가 가리키는 TCB 컨텍스트에 저장하고 Rust 핸들러를
//     부른 뒤 __user_restore로 EL0에 복귀 (진입 페이즈 3)
//   - 그 외 벡터: fail-secure 진단 경로(exception_fatal)로 수렴,
//     망가졌을 수 있는 SP를 신뢰하지 않고 전용 예외 스택으로 전환
//   - 핸들러 실행 중 ELR/SPSR을 저장하지 않는 대신 커널(EL1) 예외의 중첩을
//     허용하지 않습니다(핸들러 안의 동기 폴트는 fatal 경로로 빠져 복귀 없음).
//     EL0 예외는 ELR/SPSR이 컨텍스트에 저장되므로 핸들러 실행 중의 커널
//     IRQ 언마스크도 안전하지만, 현재는 마스크한 채로 처리합니다.

.macro FATAL_ENTRY idx
    .p2align 7                          // 각 엔트리는 128바이트 슬롯
    mov     x0, #\idx
    b       __exc_fatal_entry
.endm

// EL0에서 진입하는 예외: SP는 SP_EL1(커널 스택 최상단), 사용자 레지스터를
// 컨텍스트에 전부 저장해야 하므로 x0/x1만 스택에 피신하고 공통 경로로 감
.macro LOWER_ENTRY handler
    .p2align 7
    sub     sp, sp, #16
    stp     x0, x1, [sp]
    adrp    x0, \handler
    add     x0, x0, :lo12:\handler
    b       __lower_common
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
    LOWER_ENTRY el0_sync                // Lower EL, AArch64: Sync (svc 포함)
    LOWER_ENTRY irq_lower               // Lower EL, AArch64: IRQ
    LOWER_ENTRY fiq_lower               // Lower EL, AArch64: FIQ
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

// EL0 예외 공통 저장 경로
// 진입 시 x0 = Rust 핸들러 주소, [sp] = 피신해 둔 원래 x0/x1
__lower_common:
    adrp    x1, __current_context
    ldr     x1, [x1, :lo12:__current_context]
    stp     x2, x3, [x1, #16]
    stp     x4, x5, [x1, #32]
    stp     x6, x7, [x1, #48]
    stp     x8, x9, [x1, #64]
    stp     x10, x11, [x1, #80]
    stp     x12, x13, [x1, #96]
    stp     x14, x15, [x1, #112]
    stp     x16, x17, [x1, #128]
    stp     x18, x19, [x1, #144]
    stp     x20, x21, [x1, #160]
    stp     x22, x23, [x1, #176]
    stp     x24, x25, [x1, #192]
    stp     x26, x27, [x1, #208]
    stp     x28, x29, [x1, #224]
    str     x30, [x1, #240]
    ldp     x2, x3, [sp], #16           // 도망간(피신한) 상태인 원래의 x0/x1 회수
    stp     x2, x3, [x1, #0]
    mrs     x2, sp_el0
    mrs     x3, elr_el1
    stp     x2, x3, [x1, #248]          // 248: sp, 256: elr
    mrs     x2, spsr_el1
    str     x2, [x1, #264]
    mov     x9, x0
    mov     x0, x1                      // 핸들러 인자 = 컨텍스트 포인터
    blr     x9
    // 핸들러가 복귀하면 아래로 흘러 사용자로 재진입

// __current_context의 컨텍스트를 복원하고 EL0로 eret
// 최초 진입(k0-sched의 handoff)도 이 경로를 그대로 사용
.global __user_restore
__user_restore:
    adrp    x0, __current_context
    ldr     x0, [x0, :lo12:__current_context]
    ldp     x2, x3, [x0, #248]
    msr     sp_el0, x2
    msr     elr_el1, x3
    ldr     x2, [x0, #264]
    msr     spsr_el1, x2
    ldp     x2, x3, [x0, #16]
    ldp     x4, x5, [x0, #32]
    ldp     x6, x7, [x0, #48]
    ldp     x8, x9, [x0, #64]
    ldp     x10, x11, [x0, #80]
    ldp     x12, x13, [x0, #96]
    ldp     x14, x15, [x0, #112]
    ldp     x16, x17, [x0, #128]
    ldp     x18, x19, [x0, #144]
    ldp     x20, x21, [x0, #160]
    ldp     x22, x23, [x0, #176]
    ldp     x24, x25, [x0, #192]
    ldp     x26, x27, [x0, #208]
    ldp     x28, x29, [x0, #224]
    ldr     x30, [x0, #240]
    ldp     x0, x1, [x0, #0]
    eret

// 예외 전용 스택 (부트 스택과 분리, .bss라 부팅 시 zeroize됨)
.section .bss
.p2align 4
__exc_stack_bottom:
    .skip 8192
__exc_stack_top:
