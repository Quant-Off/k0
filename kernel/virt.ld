/*
 * QEMU virt 링커 스크립트입니다.
 *
 * QEMU virt의 RAM은 0x4000_0000부터 시작하고, 추가로 오프셋 512KiB에 로드를
 * 시작합니다. `.text` / `.rodata` / `.data`를 4KiB 페이지 경계로 분리해서
 * 진입 페이즈 1의 MMU 구성이 RX / RO / RW+XN을 각각 매핑(W^X)할 수 있게 합니다.
 *
 * 부트 스택은 `.bss` 뒤에 배치됩니다. `__boot_stack_bottom` 아래는 MMU 활성화
 * 후 가드 페이지(unmapped)로 두어 스택 오버플로를 폴트로 잡습니다.
 */

ENTRY(_start)

__load_base = 0x40080000;

/* Linux ARM64 Image 헤더(boot.S)가 참조하는 상수들 */
__text_offset = __load_base - 0x40000000;   /* RAM 시작 대비 로드 오프셋 */
__image_flags = 2;                          /* LE, 4KiB 페이지 */

/* ELF 프로그램 헤더 수준에서도 W^X를 명시합니다. */
PHDRS
{
    text   PT_LOAD FLAGS(5);        /* R X */
    rodata PT_LOAD FLAGS(4);        /* R   */
    data   PT_LOAD FLAGS(6);        /* R W */
}

SECTIONS
{
    . = __load_base;
    __kernel_start = .;

    .text : ALIGN(4K) {
        KEEP(*(.text.boot))         /* _start가 반드시 이미지 선두 */
        *(.text .text.*)
    } :text

    . = ALIGN(4K);                  /* --- 여기서부터 실행 불가(XN) --- */
    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(4K);                  /* --- 여기서부터 쓰기 가능(RW) --- */
    __data_start = .;
    .data : {
        *(.data .data.*)
    } :data

    . = ALIGN(4K);
    .bss (NOLOAD) : {
        . = ALIGN(16);              /* boot.S의 stp 16바이트 제로화 전제 */
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;

        . = ALIGN(4K);
        . += 4K;                    /* 가드 페이지 전용 홀(.bss 꼬리와 분리) */
        __boot_stack_bottom = .;    /* 이 아래 4KiB는 매핑하지 않음 */
        . += 64K;                   /* 부트 코어 커널 스택 64KiB */
        __boot_stack_top = .;
    } :data

    . = ALIGN(4K);
    __kernel_end = .;
    __image_size = __kernel_end - __load_base;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
    }
}
