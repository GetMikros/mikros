/* A static C program for the C library tests. It prints a greeting
 * for its first argument to standard output, and exits with 0 when it
 * was given exactly one argument and 1 otherwise. */

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv)
{
	char *copy = malloc(64);
	if (!copy)
		return 2;
	snprintf(copy, 64, "hello, %s", argc > 1 ? argv[1] : "nobody");
	puts(copy);
	free(copy);
	return argc == 2 ? 0 : 1;
}
