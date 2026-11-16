/* A program of an installed package that needs three libraries: one in
   the package's lib/, through its DT_RUNPATH, one in the package's
   libraries/, and org.example.greet:greet of a library package installed
   for its user. It also opens the last with dlopen by that name. The exit
   code names the first check that failed. */
#include <dlfcn.h>
#include <string.h>

const char *from_runpath(void);
const char *from_package(void);
const char *from_library_package(void);

int main(void)
{
	if (strcmp(from_runpath(), "runpath") != 0)
		return 61;
	if (strcmp(from_package(), "package") != 0)
		return 62;
	if (strcmp(from_library_package(), "library package") != 0)
		return 63;
	void *lib = dlopen("org.example.greet:greet", RTLD_NOW);
	if (!lib || dlsym(lib, "from_library_package") != (void *)from_library_package)
		return 64;
	/* A name that would leave the folder it is looked for in. */
	if (dlopen("org.example.greet:../greet", RTLD_NOW))
		return 65;
	return 0;
}
