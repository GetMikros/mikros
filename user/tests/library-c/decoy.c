/* A library with the same name as libgreet, in a directory that only a
   DT_RPATH entry names, which the dynamic linker must not search. */
#include <stdio.h>

int greet(char *buf, size_t size)
{
	return snprintf(buf, size, "the decoy");
}
