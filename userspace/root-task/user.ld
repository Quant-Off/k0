/*
 * 루트 태스크(EL0) 링커 스크립트입니다.
 *
 * 커널이 ELF 로더를 갖지 않도록 k0-boot의 build.rs가 이 배치를 flat
 * 이미지 + 세그먼트 메타데이터로 미리 변환합니다. 세그먼트 경계는 두
 * 플랫폼의 그래뉼을 모두 만족하게 전부 ALIGN(16K)이고, W^X는 빌드 시점에
 * 검사됩니다(RWX 세그먼트가 나오면 빌드 실패).
 */

ENTRY(_start)

/* 사용자 VA 베이스, 16KiB 정렬. 첫 그래뉼(null 가드)과 멀리 떨어뜨림 */
__user_base = 0x400000;

PHDRS
{
    text   PT_LOAD FLAGS(5);        /* R X */
    rodata PT_LOAD FLAGS(4);        /* R   */
    data   PT_LOAD FLAGS(6);        /* R W */
}

SECTIONS
{
    . = __user_base;

    .text : ALIGN(16K) {
        *(.text .text.*)
    } :text

    . = ALIGN(16K);                 /* --- 여기서부터 실행 불가 --- */
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(16K);                 /* --- 여기서부터 쓰기 가능 --- */
    .data : {
        *(.data .data.*)
    } :data

    .bss (NOLOAD) : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
    }
}
