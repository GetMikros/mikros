/* A static C program for the C library tests. It prints a greeting
 * for its first argument to standard output, and exits with 0 when it
 * was given exactly one argument and the monotonic clock reads through
 * the C library's system call function, and with another code
 * otherwise. */

#include <mikros/syscalls.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv)
{
	int64_t now = 0;
	if (mikros_clock_get_monotonic(&now) != 0 || now <= 0)
		return 3;
	char *copy = malloc(64);
	if (!copy)
		return 2;
	snprintf(copy, 64, "hello, %s", argc > 1 ? argv[1] : "nobody");
	puts(copy);
	free(copy);
	return argc == 2 ? 0 : 1;
}
