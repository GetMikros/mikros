/* The start code that the toolchain links into every C program. It
 * passes only the bootstrap channel, which the kernel starts the first
 * thread with in RDI, and the address of main to the C library. */

	.text
	.globl _start
	.type _start,@function
_start:
	xor %ebp,%ebp
	lea main(%rip),%rsi
	call __mikros_start
	ud2
