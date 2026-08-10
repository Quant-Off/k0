/*
 * Apple Silicon(m1n1 체인로드) 링커 스크립트입니다.
 *
 * Apple Silicon의 DRAM은 `0x8_0000_0000`부터 시작합니다. `__load_base`는 m1n1에
 * 지정할 로드 주소 `0x8_1000_0000`과 반드시 일치해야 합니다. 불일치하면 정적
 * 재배치(adrp 계열)가 전부 불일치납니다.
 *
 * 진입 페이즈 1의 MMU 구성이 16KiB 그래뉼(granule)을 사용하므로 `.text` / `.rodata` /
 * `.data`의 W^X 분리 경계와 스택 가드 경계를 전부 ALIGN(16K)로 맞췄습니다.
 * (4KiB 경계인 virt.ld와의 유일한 구조적 차이)
 *
 * 부트 스택은 `.bss` 뒤에 배치했고 `__boot_stack_bottom` 바로 아래 16KiB는
 * 데이터가 전혀 없는 전용 홀로 비워 두고, MMU 활성화 후 가드 페이지(unmapped)로
 * 삼아 스택 오버플로를 폴트로 잡습니다.
 */

ENTRY(_start)

/* 물리 로드 주소(m1n1에 지정할 주소). 진입 직후엔 identity로 실행됨 */
__load_pa = 0x810000000;

/* higher-half 가상 링크 오프셋 (k0-mm의 KERNEL_VA_OFFSET과 반드시 일치) */
__virt_offset = 0xFFFF000000000000;
__load_base = __virt_offset + __load_pa;

/* Linux ARM64 Image 헤더(boot.S)가 참조하는 상수들 */
__text_offset = __load_pa - 0x800000000;    /* DRAM 시작 대비 로드 오프셋 */
__image_flags = 4;                          /* LE, 16KiB 페이지 */

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

    .text : ALIGN(16K) {
        KEEP(*(.text.boot))         /* _start가 반드시 이미지 선두 */
        *(.text .text.*)
    } :text

    . = ALIGN(16K);                 /* --- 여기서부터 실행 불가(XN) --- */
    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(16K);                 /* --- 여기서부터 쓰기 가능(RW) --- */
    __data_start = .;
    .data : {
        *(.data .data.*)
    } :data

    . = ALIGN(16K);
    .bss (NOLOAD) : {
        . = ALIGN(16);              /* boot.S의 stp 16바이트 제로화 전제 */
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;

        . = ALIGN(16K);
        . += 16K;                   /* 가드 페이지 전용 홀(.bss 꼬리와 분리) */
        __boot_stack_bottom = .;    /* 이 아래 16KiB는 매핑하지 않음 */
        . += 64K;                   /* 부트 코어 커널 스택 64KiB(16K 페이지 4장) */
        __boot_stack_top = .;
    } :data

    . = ALIGN(16K);
    __kernel_end = .;
    __image_size = __kernel_end - __load_base;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
    }
}
