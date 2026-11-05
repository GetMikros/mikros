/* The start code that the toolchain links into every C program. It
 * passes only the bootstrap channel, which the kernel starts the first
 * thread with in RDI, and the address of main to the C library. */

/* The ABI levels of the program: the minimum it needs and the one it
 * was built for. */
	.section .note.mikros.abi,"a",@note
	.p2align 2
	.long 7
	.long 8
	.long 1
	.asciz "Mikros"
	.p2align 2
	.long 0
	.long 0

	.text
	.globl _start
	.type _start,@function
_start:
	xor %ebp,%ebp
	lea main(%rip),%rsi
	call __mikros_start
	ud2
