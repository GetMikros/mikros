/* A shared library for the dynamic linker tests: it uses the C library
   and thread-local storage. */
#include <stdio.h>

static _Thread_local int calls;

int greet(char *buf, size_t size)
{
	calls++;
	return snprintf(buf, size, "hello from a library, call %d", calls);
}
