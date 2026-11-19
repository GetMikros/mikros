/* A C program for the C library tests of memory descriptors. It checks
 * memfd_create's byte size through writes, ftruncate and fstat, reads
 * that stop at the byte size, shared and private mappings, seals, and a
 * descriptor passed to a child, which it starts as itself with the
 * arguments `child <fd>`. It exits with 0 when every check passed, and
 * otherwise with the number of the first check that failed. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static long size_of(int fd)
{
	struct stat st;
	if (fstat(fd, &st) || !S_ISREG(st.st_mode))
		return -1;
	return st.st_size;
}

/* The child: the descriptor it got has the object's size, whole pages,
 * as its byte size, and the parent's bytes. */
static int child(int fd)
{
	char c;
	if (size_of(fd) != 4096)
		return 60;
	if (pread(fd, &c, 1, 0) != 1 || c != 'X')
		return 61;
	return 0;
}

int main(int argc, char **argv)
{
	char buf[16];
	if (argc == 3 && strcmp(argv[1], "child") == 0)
		return child(atoi(argv[2]));

	int fd = memfd_create("memfd-c", MFD_ALLOW_SEALING);
	if (fd < 0)
		return 10;
	if (size_of(fd) != 0)
		return 11;
	if (write(fd, "hello", 5) != 5 || size_of(fd) != 5)
		return 12;
	if (lseek(fd, 0, SEEK_SET) != 0 || read(fd, buf, sizeof buf) != 5 ||
	    memcmp(buf, "hello", 5) != 0)
		return 13;
	/* Reads stop at the byte size. */
	if (pread(fd, buf, sizeof buf, 3) != 2 || pread(fd, buf, 1, 5) != 0)
		return 14;
	if (lseek(fd, 0, SEEK_END) != 5)
		return 15;

	/* Growing gives zeros, and the object whole pages. */
	if (ftruncate(fd, 10000) || size_of(fd) != 10000)
		return 20;
	if (pread(fd, buf, 10, 9990) != 10 || buf[0] || buf[9])
		return 21;

	/* A shared mapping is the object; a private one a copy. */
	char *shared = mmap(NULL, 12288, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (shared == MAP_FAILED)
		return 30;
	shared[0] = 'X';
	if (pread(fd, buf, 1, 0) != 1 || buf[0] != 'X')
		return 31;
	char *private = mmap(NULL, 12288, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
	if (private == MAP_FAILED || private[0] != 'X')
		return 32;
	private[0] = 'Y';
	if (pread(fd, buf, 1, 0) != 1 || buf[0] != 'X' || shared[0] != 'X')
		return 33;
	munmap(private, 12288);

	/* Shrinking clears the rest of the last page. */
	if (ftruncate(fd, 3) || size_of(fd) != 3)
		return 40;
	if (ftruncate(fd, 10) || pread(fd, buf, 10, 0) != 10 || buf[0] != 'X' ||
	    buf[3] || buf[9])
		return 41;
	munmap(shared, 12288);

	/* Seals stop resizing, and no seal is added after F_SEAL_SEAL. */
	if (fcntl(fd, F_GET_SEALS) != 0)
		return 50;
	if (fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK) || fcntl(fd, F_GET_SEALS) != F_SEAL_SHRINK)
		return 51;
	if (ftruncate(fd, 1) == 0 || errno != EPERM)
		return 52;
	if (fcntl(fd, F_ADD_SEALS, F_SEAL_GROW) || ftruncate(fd, 20) == 0 || errno != EPERM)
		return 53;
	if (fcntl(fd, F_ADD_SEALS, 1 << 10) == 0 || errno != EINVAL)
		return 54;
	int unsealable = memfd_create("memfd-c", 0);
	if (unsealable < 0 || fcntl(unsealable, F_GET_SEALS) != F_SEAL_SEAL ||
	    fcntl(unsealable, F_ADD_SEALS, F_SEAL_SHRINK) == 0)
		return 55;
	close(unsealable);

	/* A child gets the descriptor with the object's size. */
	char number[16];
	snprintf(number, sizeof number, "%d", fd);
	char *args[] = {argv[0], "child", number, NULL};
	pid_t pid;
	int status;
	if (posix_spawn(&pid, argv[0], NULL, NULL, args, environ))
		return 62;
	if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status))
		return 63;
	if (WEXITSTATUS(status) != 0)
		return WEXITSTATUS(status);
	return 0;
}
