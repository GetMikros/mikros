/* A program that the C hello world starts with posix_spawn. It checks
 * its arguments, environment and descriptors, writes to standard
 * output, and exits with 7 when every check passed, and otherwise with
 * the number of the first check that failed. Started with the argument
 * "exec", it replaces itself with execve, passing its process ID, which
 * the new program checks it kept. */

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define GREETING_TEXT "a greeting from the boot file system\n"

/* Fails to run a missing program, then runs this one again with a
   descriptor that closes on exec, which must not reach it. */
static int exec_again(void)
{
	char *args[] = { "spawn-child", "one", "two", 0, 0 };
	char *env[] = { "SPAWN_TEST=yes", 0 };
	if (execve("/usr/bin/missing", args, env) != -1 || errno != ENOENT)
		return 10;
	int root = open("/", O_RDONLY | O_DIRECTORY);
	if (root < 0 || dup3(root, 7, O_CLOEXEC) != 7 || close(root))
		return 11;
	char pid[16];
	snprintf(pid, sizeof pid, "%d", (int)getpid());
	args[3] = pid;
	execve("/usr/bin/spawn-child", args, env);
	return 12;
}

int main(int argc, char **argv)
{
	char buf[64];
	if (argc == 2 && strcmp(argv[1], "exec") == 0)
		return exec_again();
	/* Started with "closed" and a descriptor, it exits with 7 when that
	   descriptor is not open. */
	if (argc == 3 && strcmp(argv[1], "closed") == 0)
		return fcntl(atoi(argv[2]), F_GETFD) == -1 && errno == EBADF ? 7 : 8;
	/* After exec the process ID is the one before, and descriptor 7
	   closed. */
	if (argc == 4 && (atoi(argv[3]) != getpid() || fcntl(7, F_GETFD) != -1
			  || errno != EBADF))
		return 6;
	if (argc < 3 || argc > 4 || strcmp(argv[1], "one") != 0 || strcmp(argv[2], "two") != 0)
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
	/* Descriptor 8 ends when the parent saw it running. */
	while (read(8, buf, sizeof buf) > 0)
		;
	return 7;
}
