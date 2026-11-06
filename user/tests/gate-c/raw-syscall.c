/* Makes a system call with its own instruction rather than through the
   C library: handle_close of handle 0, which changes nothing. As a
   system component the call returns and the program exits with 0; as an
   application, the system call gate stops it at the instruction. */
int main(void)
{
	long status;
	__asm__ volatile("syscall" : "=a"(status) : "a"(1L), "D"(0L) : "rcx", "r11", "memory");
	(void)status;
	return 0;
}
