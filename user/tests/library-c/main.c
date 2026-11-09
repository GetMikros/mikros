/* Loads libgreet at start and with dlopen, and prints its greeting, and
   is refused libraries that are missing or have no ABI note. The exit
   code names the first check that failed. */
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

int greet(char *buf, size_t size);

int main(void)
{
	char first[64], second[64];
	greet(first, sizeof first);
	if (strcmp(first, "hello from a library, call 1") != 0)
		return 1;
	void *lib = dlopen("libgreet.1.dlib", RTLD_NOW);
	if (!lib)
		return 2;
	int (*found)(char *, size_t) = (int (*)(char *, size_t))dlsym(lib, "greet");
	if (found != greet)
		return 3;
	found(second, sizeof second);
	if (strcmp(second, "hello from a library, call 2") != 0)
		return 4;
	if (dlopen("libmissing.1.dlib", RTLD_NOW) || !dlerror())
		return 5;
	/* A library linked without crti.o has no ABI note, and is refused. */
	if (dlopen("libnonote.1.dlib", RTLD_NOW))
		return 6;
	const char *why = dlerror();
	if (!why || !strstr(why, "Exec format error"))
		return 7;
	puts(first);
	return 0;
}
