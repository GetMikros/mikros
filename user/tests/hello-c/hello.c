/* A static C program for the C library tests. It prints a greeting
 * for its first argument to standard output, and exits with 0 when it
 * was given exactly one argument and every check below passed, and with
 * the number of the first check that failed otherwise. */

#include <errno.h>
#include <fcntl.h>
#include <mikros/abi.h>
#include <mikros/syscalls.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/* A file of the boot image, and what it holds. */
#define GREETING "/usr/share/hello/greeting.txt"
#define GREETING_TEXT "a greeting from the boot file system\n"

static int64_t ns(struct timespec ts)
{
	return ts.tv_sec * 1000000000 + ts.tv_nsec;
}

/* The C library's monotonic clock lies between two readings of the
 * system call's. */
static int monotonic_agrees(void)
{
	int64_t before = 0, after = 0;
	struct timespec ts;
	if (mikros_clock_get_monotonic(&before) != 0 || before <= 0)
		return 0;
	if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
		return 0;
	if (mikros_clock_get_monotonic(&after) != 0)
		return 0;
	return before <= ns(ts) && ns(ts) <= after && ts.tv_nsec < 1000000000;
}

static int cpu_time_counts(clockid_t clock)
{
	struct timespec ts;
	return clock_gettime(clock, &ts) == 0 && ns(ts) > 0;
}

/* The parent's directory has no time service at the first request, so
 * the time of day is not known yet; at the next it hands out a time
 * page whose offset is UTC_OFFSET. */
#define UTC_OFFSET (1704067200 * (int64_t)1000000000)

static int realtime_waits_for_the_time_page(void)
{
	struct timespec ts;
	errno = 0;
	return clock_gettime(CLOCK_REALTIME, &ts) == -1 && errno == EAGAIN;
}

static int realtime_from_the_time_page(void)
{
	int64_t before = 0, after = 0;
	struct timespec ts;
	if (mikros_clock_get_monotonic(&before) != 0)
		return 0;
	if (clock_gettime(CLOCK_REALTIME, &ts) != 0)
		return 0;
	if (mikros_clock_get_monotonic(&after) != 0)
		return 0;
	int64_t monotonic = ns(ts) - UTC_OFFSET;
	return before <= monotonic && monotonic <= after;
}

/* Standard I/O reads the file. */
static int stdio_reads_a_file(void)
{
	char line[64];
	FILE *f = fopen(GREETING, "r");
	if (!f)
		return 0;
	int read = fgets(line, sizeof line, f) != 0;
	fclose(f);
	return read && strcmp(line, GREETING_TEXT) == 0;
}

/* The offset of a descriptor is shared with its duplicates, and pread
 * reads at an offset of its own. */
static int descriptors_share_offsets(void)
{
	char part[6] = {0};
	int fd = open("/usr/share/../share/./hello/greeting.txt", O_RDONLY);
	if (fd < 0)
		return 0;
	int copy = dup(fd);
	int ok = copy > fd
		&& lseek(fd, 0, SEEK_END) == (off_t)strlen(GREETING_TEXT)
		&& lseek(fd, 2, SEEK_SET) == 2
		&& lseek(copy, 0, SEEK_CUR) == 2
		&& pread(fd, part, 5, 2) == 5 && strcmp(part, "greet") == 0
		&& lseek(copy, 0, SEEK_CUR) == 2
		&& read(copy, part, 5) == 5 && strcmp(part, "greet") == 0
		&& lseek(fd, 0, SEEK_CUR) == 7;
	close(copy);
	close(fd);
	return ok;
}

static int open_fails_as_it_should(void)
{
	char byte;
	errno = 0;
	if (open("/usr/share/hello/missing", O_RDONLY) != -1 || errno != ENOENT)
		return 0;
	/* The boot file system is read-only. */
	errno = 0;
	if (open(GREETING, O_WRONLY) != -1 || errno != EACCES)
		return 0;
	int dir = open("/usr/share", O_RDONLY | O_DIRECTORY);
	if (dir < 0)
		return 0;
	errno = 0;
	int ok = read(dir, &byte, 1) == -1 && errno == EISDIR;
	close(dir);
	errno = 0;
	return ok && open(GREETING, O_RDONLY | O_DIRECTORY) == -1 && errno == ENOTDIR;
}

static int close_on_exec_is_kept(void)
{
	int fd = open(GREETING, O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return 0;
	int ok = fcntl(fd, F_GETFD) == FD_CLOEXEC
		&& fcntl(fd, F_SETFD, 0) == 0 && fcntl(fd, F_GETFD) == 0;
	close(fd);
	return ok && fcntl(fd, F_GETFD) == -1 && errno == EBADF;
}

int main(int argc, char **argv)
{
	if (argc != 2)
		return 1;
	if (!monotonic_agrees())
		return 2;
	if (!cpu_time_counts(CLOCK_PROCESS_CPUTIME_ID))
		return 3;
	if (!cpu_time_counts(CLOCK_THREAD_CPUTIME_ID))
		return 4;
	if (!realtime_waits_for_the_time_page())
		return 5;
	if (!realtime_from_the_time_page())
		return 6;
	struct timespec ts;
	if (clock_gettime(42, &ts) != -1 || errno != EINVAL)
		return 7;
	if (mikros_abi_level() != 0)
		return 8;
	if (!stdio_reads_a_file())
		return 9;
	if (!descriptors_share_offsets())
		return 10;
	if (!open_fails_as_it_should())
		return 11;
	if (!close_on_exec_is_kept())
		return 12;
	char *copy = malloc(64);
	if (!copy)
		return 13;
	snprintf(copy, 64, "hello, %s", argv[1]);
	puts(copy);
	free(copy);
	return 0;
}
