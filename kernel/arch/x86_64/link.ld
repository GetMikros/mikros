/* The kernel image: code, read-only data and writable data in separate
   segments in the kernel range, and the notes in the read-only segment. */

ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
    note PT_NOTE FLAGS(4);
}

SECTIONS
{
    . = 0xFFFFFFFF80000000;

    .text : ALIGN(4096) { *(.text .text.*) } :text

    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata
    .notes : ALIGN(4) {
        KEEP(*(.note.mikros))
        KEEP(*(.note.gnu.build-id))
    } :rodata :note

    . = ALIGN(4096);
    .data : { *(.data .data.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) } :data

    /DISCARD/ : { *(.eh_frame .eh_frame_hdr .comment .note.GNU-stack) }
}
