/* The ABI levels of the image: the minimum it needs and the one it was
 * built for. The toolchain links this into every executable and shared
 * library through crti.o, and into the C library itself. */
	.section .note.mikros.abi,"a",@note
	.p2align 2
	.long 7
	.long 8
	.long 1
	.asciz "Mikros"
	.p2align 2
	.long 0
	.long 0
