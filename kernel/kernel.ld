/*
 * 커널 링커 스크립트입니다.
 *
 * `__granule`(변환 그래뉼 크기), `__image_flags`(Image 헤더의 flags),
 * `__image_va`(이미지의 고정 가상 주소, k0-abi의 KERNEL_IMAGE_VA)는 이 파일에
 * 없고 kernel/build.rs가 이 파일 앞에 붙여 넣습니다. 이 파일을 링커에 직접
 * 넘기면 세 심볼이 없어 실패합니다.
 *
 * 이미지는 고정 가상 주소에 링크되고 물리 주소는 정해져 있지 않습니다.
 * 부트로더가 2MiB 경계에서 `__text_offset`만큼 떨어진 임의의 물리 주소에
 * 올리면, 진입 코드가 그 위치를 PC 상대로 구해 고정 가상 주소에 매핑합니다.
 * `.text` / `.rodata` / `.data`를 그래뉼 경계로 분리해서 진입 페이즈 1의 MMU
 * 구성이 RX / RO / RW+XN을 각각 매핑(W^X)할 수 있게 합니다.
 *
 * 부트 스택은 `.bss` 뒤에 배치됩니다. `__boot_stack_bottom` 아래 한 그래뉼은
 * 데이터가 전혀 없는 전용 홀이고, MMU 활성화 후 가드 페이지(unmapped)로 두어
 * 스택 오버플로를 폴트로 잡습니다.
 */

ENTRY(_start)

/* 이미지의 링크 주소: 적재된 물리 주소와 무관한 고정 가상 주소 */
__load_base = __image_va;

/* Linux ARM64 Image 헤더(boot.S)가 참조하는 상수: 2MiB 경계 대비 적재 오프셋.
 * 헤더가 없는 것으로 취급하는 부트로더의 기본값과 같은 512KiB */
__text_offset = 0x80000;

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
