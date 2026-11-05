/* A program that the C hello world starts with posix_spawn. It checks
 * its arguments, environment and descriptors, writes to standard
 * output, and exits with 7 when every check passed, and otherwise with
 * the number of the first check that failed. */

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define GREETING_TEXT "a greeting from the boot file system\n"

int main(int argc, char **argv)
{
	char buf[64];
	if (argc != 3 || strcmp(argv[1], "one") != 0 || strcmp(argv[2], "two") != 0)
		return 1;
	const char *test = getenv("SPAWN_TEST");
	if (!test || strcmp(test, "yes") != 0)
		return 2;
	/* Descriptor 5 is the greeting, which the parent opened for it. */
	ssize_t n = read(5, buf, sizeof buf);
	if (n != (ssize_t)strlen(GREETING_TEXT) || memcmp(buf, GREETING_TEXT, n) != 0)
		return 3;
	/* Descriptor 6 closed on exec in the parent. */
	if (fcntl(6, F_GETFD) != -1 || errno != EBADF)
		return 4;
	if (printf("child output\n") < 0 || fflush(stdout))
		return 5;
	return 7;
}
