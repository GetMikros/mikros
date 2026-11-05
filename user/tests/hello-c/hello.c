/* A static C program for the C library tests. It prints a greeting
 * for its first argument to standard output, and exits with 0 when it
 * was given exactly one argument and every check below passed, and with
 * the number of the first check that failed otherwise. */

#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <mikros/abi.h>
#include <mikros/syscalls.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
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

/* stat, fstat and fstatat agree on a file and its directory, and a
   stream is a pipe. */
static int stat_reports_attributes(void)
{
	struct stat st, dir, opened, at, out;
	if (stat(GREETING, &st) != 0 || !S_ISREG(st.st_mode)
	    || (st.st_mode & 07777) != 0755
	    || st.st_size != (off_t)strlen(GREETING_TEXT) || st.st_nlink < 1
	    || st.st_ino == 0 || st.st_blksize != 4096)
		return 0;
	if (stat("/usr/share/hello", &dir) != 0 || !S_ISDIR(dir.st_mode)
	    || dir.st_dev != st.st_dev || dir.st_ino == st.st_ino)
		return 0;
	int fd = open(GREETING, O_RDONLY);
	int dirfd = open("/usr/share/hello", O_RDONLY | O_DIRECTORY);
	int ok = fd >= 0 && dirfd >= 0 && fstat(fd, &opened) == 0
		&& opened.st_ino == st.st_ino && opened.st_dev == st.st_dev
		&& opened.st_size == st.st_size
		&& fstatat(dirfd, "greeting.txt", &at, 0) == 0
		&& at.st_ino == st.st_ino && lstat(GREETING, &at) == 0
		&& S_ISREG(at.st_mode) && fstat(1, &out) == 0
		&& S_ISFIFO(out.st_mode);
	close(fd);
	close(dirfd);
	return ok && stat("/usr/share/hello/missing", &at) == -1
		&& errno == ENOENT;
}

/* A listing has each entry once, without . and .., with its type, and
   seekdir returns to where telldir was. */
static int directory_lists_its_entries(void)
{
	DIR *d = opendir("/usr/share/hello");
	if (!d)
		return 0;
	int seen = 0, count = 0;
	long second = -1;
	char again[256] = "";
	struct dirent *e;
	while ((e = readdir(d))) {
		if (count++ == 0)
			second = telldir(d);
		if (strcmp(e->d_name, "greeting.txt") == 0 && e->d_type == DT_REG)
			seen |= 1;
		else if (strcmp(e->d_name, "near") == 0 && e->d_type == DT_LNK)
			seen |= 2;
		else if (strcmp(e->d_name, "far") == 0 && e->d_type == DT_LNK)
			seen |= 4;
		else
			seen |= 8;
		if (count == 2)
			strcpy(again, e->d_name);
	}
	seekdir(d, second);
	e = readdir(d);
	int ok = seen == 7 && count == 3 && e && strcmp(e->d_name, again) == 0;
	closedir(d);
	return ok;
}

/* The current directory resolves relative paths, and changes only to a
   directory. */
static int current_directory_moves(void)
{
	char cwd[64];
	if (!getcwd(cwd, sizeof cwd) || strcmp(cwd, "/") != 0)
		return 0;
	int dirfd = open("/usr/share/hello", O_RDONLY | O_DIRECTORY);
	if (dirfd < 0 || chdir("/usr/share/hello/") != 0
	    || !getcwd(cwd, sizeof cwd) || strcmp(cwd, "/usr/share/hello") != 0)
		return 0;
	struct stat st;
	int ok = stat("greeting.txt", &st) == 0 && S_ISREG(st.st_mode)
		&& chdir("greeting.txt") == -1 && errno == ENOTDIR
		&& getcwd(cwd, 4) == 0 && errno == ERANGE && chdir("..") == 0
		&& getcwd(cwd, sizeof cwd) && strcmp(cwd, "/usr/share") == 0
		&& fchdir(dirfd) == 0 && getcwd(cwd, sizeof cwd)
		&& strcmp(cwd, "/usr/share/hello") == 0;
	close(dirfd);
	return chdir("/") == 0 && ok;
}

/* readlink returns a link's target, and stat follows links that the
   server follows and links it redirects. */
static int links_resolve(void)
{
	char target[32];
	struct stat file, near, far, link;
	ssize_t n = readlink("/usr/share/hello/near", target, sizeof target);
	if (n != 12 || memcmp(target, "greeting.txt", 12) != 0)
		return 0;
	if (readlink("/usr/share/hello/far", target, 4) != 4
	    || memcmp(target, "/usr", 4) != 0)
		return 0;
	if (readlink(GREETING, target, sizeof target) != -1 || errno != EINVAL)
		return 0;
	return stat(GREETING, &file) == 0 && stat("/usr/share/hello/near", &near) == 0
		&& stat("/usr/share/hello/far", &far) == 0
		&& near.st_ino == file.st_ino && far.st_ino == file.st_ino
		&& lstat("/usr/share/hello/far", &link) == 0 && S_ISLNK(link.st_mode)
		&& link.st_ino != file.st_ino;
}

/* A file maps privately, a private copy keeps its changes from the file,
   and a mapping longer than the file reserves the rest, which can be
   mapped over. */
static int files_map(void)
{
	size_t n = strlen(GREETING_TEXT);
	int fd = open(GREETING, O_RDONLY);
	int dirfd = open("/usr/share/hello", O_RDONLY | O_DIRECTORY);
	if (fd < 0 || dirfd < 0)
		return 0;
	char *view = mmap(0, n, PROT_READ, MAP_PRIVATE, fd, 0);
	char *copy = mmap(0, n, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
	char *span = mmap(0, 3 * 4096, PROT_READ, MAP_PRIVATE, fd, 0);
	if (view == MAP_FAILED || copy == MAP_FAILED || span == MAP_FAILED)
		return 0;
	copy[0] = 'A';
	char first = 0;
	char *over = mmap(span + 4096, 4096, PROT_READ | PROT_WRITE,
			  MAP_PRIVATE | MAP_FIXED | MAP_ANONYMOUS, -1, 0);
	int ok = memcmp(view, GREETING_TEXT, n) == 0
		&& memcmp(span, GREETING_TEXT, n) == 0 && copy[0] == 'A'
		&& view[0] == 'a' && pread(fd, &first, 1, 0) == 1 && first == 'a'
		&& over == span + 4096 && (over[0] = 1, over[0] == 1);
	ok = ok && mmap(0, n, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) == MAP_FAILED
		&& errno == EACCES
		&& mmap(0, n, PROT_READ, MAP_PRIVATE, fd, 1) == MAP_FAILED
		&& errno == EINVAL
		&& mmap(0, n, PROT_READ, MAP_PRIVATE, dirfd, 0) == MAP_FAILED
		&& errno == ENODEV;
	munmap(view, n);
	munmap(copy, n);
	munmap(span, 3 * 4096);
	close(fd);
	close(dirfd);
	return ok;
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
	if (!stat_reports_attributes())
		return 13;
	if (!directory_lists_its_entries())
		return 14;
	if (!current_directory_moves())
		return 15;
	if (!links_resolve())
		return 16;
	if (!files_map())
		return 17;
	char *copy = malloc(64);
	if (!copy)
		return 18;
	snprintf(copy, 64, "hello, %s", argv[1]);
	puts(copy);
	free(copy);
	return 0;
}
