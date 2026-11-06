/* The kernel image: code, read-only data and writable data in separate
   segments in the kernel range, the notes and the relocations in the
   read-only segment, and the dynamic section, which tells the bootloader
   where the relocations are, in the writable one. The image is
   position-independent; it is linked where it runs unless the bootloader
   moves it. */

ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
    note PT_NOTE FLAGS(4);
    dynamic PT_DYNAMIC FLAGS(6);
}

SECTIONS
{
    . = 0xFFFFFFFF80000000;

    __kernel_start = .;
    .text : ALIGN(4096) { *(.text .text.*) } :text
    __text_end = .;

    . = ALIGN(4096);
    __rodata_start = .;
    .rodata : { *(.rodata .rodata.*) } :rodata
    .note.mikros : ALIGN(4) { KEEP(*(.note.mikros)) } :rodata :note
    .note.gnu.build-id : ALIGN(4) {
        __build_id_note = .;
        KEEP(*(.note.gnu.build-id))
        __build_id_note_end = .;
    } :rodata :note
    .dynsym : { *(.dynsym) } :rodata
    .gnu.hash : { *(.gnu.hash) } :rodata
    .hash : { *(.hash) } :rodata
    .dynstr : { *(.dynstr) } :rodata
    .rela.dyn : { *(.rela.dyn) } :rodata
    __rodata_end = .;

    . = ALIGN(4096);
    __data_start = .;
    .data : { *(.data .data.*) } :data
    /* Inside the data range, so that the kernel's own tables map it. */
    .got : { *(.got .got.*) } :data
    .dynamic : { *(.dynamic) } :data :dynamic
    .bss : { *(.bss .bss.*) *(COMMON) } :data
    __data_end = .;

    /DISCARD/ : { *(.eh_frame .eh_frame_hdr .comment .note.GNU-stack) }
}
