/*
 * 커널 링커 스크립트입니다.
 *
 * `__granule`(변환 그래뉼 크기)과 `__image_flags`(Image 헤더의 페이지 크기
 * 힌트)는 이 파일에 없고, kernel/build.rs가 그래뉼 feature에 맞춰 이 파일
 * 앞에 붙여 넣습니다. 이 파일을 링커에 직접 넘기면 두 심볼이 없어 실패합니다.
 *
 * QEMU virt의 RAM은 0x4000_0000부터 시작하고, 추가로 오프셋 512KiB에 로드를
 * 시작합니다. `.text` / `.rodata` / `.data`를 그래뉼 경계로 분리해서
 * 진입 페이즈 1의 MMU 구성이 RX / RO / RW+XN을 각각 매핑(W^X)할 수 있게 합니다.
 *
 * 부트 스택은 `.bss` 뒤에 배치됩니다. `__boot_stack_bottom` 아래 한 그래뉼은
 * 데이터가 전혀 없는 전용 홀이고, MMU 활성화 후 가드 페이지(unmapped)로 두어
 * 스택 오버플로를 폴트로 잡습니다.
 */

ENTRY(_start)

/* 물리 로드 주소: Image는 여기 적재되고 진입 직후엔 identity로 실행됨 */
__load_pa = 0x40080000;

/* higher-half 가상 링크 오프셋: k0-mm의 KERNEL_VA_OFFSET과 반드시 일치 */
__virt_offset = 0xFFFF000000000000;
__load_base = __virt_offset + __load_pa;

/* Linux ARM64 Image 헤더(boot.S)가 참조하는 상수 */
__text_offset = __load_pa - 0x40000000;     /* RAM 시작 대비 로드 오프셋 */

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

    .text : ALIGN(__granule) {
        KEEP(*(.text.boot))         /* _start가 반드시 이미지 선두 */
        *(.text .text.*)
    } :text

    . = ALIGN(__granule);           /* --- 여기서부터 실행 불가(XN) --- */
    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(__granule);           /* --- 여기서부터 쓰기 가능(RW) --- */
    __data_start = .;
    .data : {
        *(.data .data.*)
    } :data

    . = ALIGN(__granule);
    .bss (NOLOAD) : {
        . = ALIGN(16);              /* boot.S의 stp 16바이트 제로화 전제 */
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;

        . = ALIGN(__granule);
        . += __granule;             /* 가드 페이지 전용 홀(.bss 꼬리와 분리) */
        __boot_stack_bottom = .;    /* 이 아래 한 그래뉼은 매핑하지 않음 */
        . += 64K;                   /* 부트 코어 커널 스택 64KiB */
        __boot_stack_top = .;
    } :data

    . = ALIGN(__granule);
    __kernel_end = .;
    __image_size = __kernel_end - __load_base;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
    }
}
