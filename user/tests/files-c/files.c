/* A C program for the C library tests: it makes, changes, links,
 * renames and removes files and folders in a folder of its own below
 * the folder named by its argument, which must be writable, and exits
 * with 0 when every check below passed, and with the number of the
 * first check that failed otherwise, after printing what failed. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <time.h>
#include <unistd.h>

static int step;

#define CHECK(cond)                                                          \
	do {                                                                 \
		step++;                                                      \
		if (!(cond)) {                                               \
			printf("files-c: check %d failed: %s (errno %d)\n", \
			       step, #cond, errno);                          \
			return step;                                         \
		}                                                            \
	} while (0)

static int fails_with(int result, int error)
{
	return result == -1 && errno == error;
}

int main(int argc, char **argv)
{
	char dir[256], a[300], b[300], c[300], s[300], sub[300], target[64];
	struct stat st;
	struct statvfs vfs;
	if (argc != 2)
		return 100;
	snprintf(dir, sizeof dir, "%s/files-c", argv[1]);
	snprintf(a, sizeof a, "%s/a", dir);
	snprintf(b, sizeof b, "%s/b", dir);
	snprintf(c, sizeof c, "%s/c", dir);
	snprintf(s, sizeof s, "%s/s", dir);
	snprintf(sub, sizeof sub, "%s/sub", dir);

	/* Folders. */
	CHECK(mkdir(dir, 0755) == 0);
	CHECK(fails_with(mkdir(dir, 0755), EEXIST));
	CHECK(stat(dir, &st) == 0 && S_ISDIR(st.st_mode));
	CHECK(mkdir(sub, 0700) == 0);
	CHECK(rmdir(sub) == 0);
	CHECK(fails_with(rmdir(sub), ENOENT));

	/* A file, its size, mode and times. */
	int fd = open(a, O_CREAT | O_WRONLY | O_TRUNC, 0644);
	CHECK(fd >= 0 && write(fd, "hello", 5) == 5);
	CHECK(truncate(a, 2) == 0 && stat(a, &st) == 0 && st.st_size == 2);
	CHECK(ftruncate(fd, 4) == 0 && fstat(fd, &st) == 0 && st.st_size == 4);
	CHECK(chmod(a, 0600) == 0 && stat(a, &st) == 0 && (st.st_mode & 07777) == 0600);
	CHECK(fchmod(fd, 0640) == 0 && fstat(fd, &st) == 0 && (st.st_mode & 07777) == 0640);
	struct timespec times[2] = {{1000, 0}, {2000, 500}};
	CHECK(utimensat(AT_FDCWD, a, times, 0) == 0);
	CHECK(stat(a, &st) == 0 && st.st_atime == 1000 && st.st_mtime == 2000);
	struct timespec omit[2] = {{0, UTIME_OMIT}, {3000, 0}};
	CHECK(utimensat(AT_FDCWD, a, omit, 0) == 0);
	CHECK(stat(a, &st) == 0 && st.st_atime == 1000 && st.st_mtime == 3000);
	/* Now is known once the time service gave the time. */
	struct timespec now;
	if (clock_gettime(CLOCK_REALTIME, &now) == 0)
		CHECK(futimens(fd, NULL) == 0 && fstat(fd, &st) == 0 && st.st_mtime >= now.tv_sec);
	else
		CHECK(fails_with(futimens(fd, NULL), EAGAIN));
	struct timespec later[2] = {{4000, 0}, {5000, 0}};
	CHECK(futimens(fd, later) == 0 && fstat(fd, &st) == 0 && st.st_mtime == 5000);
	CHECK(close(fd) == 0);

	/* Owners do not change. */
	CHECK(stat(a, &st) == 0 && chown(a, st.st_uid, (gid_t)-1) == 0);
	CHECK(fails_with(chown(a, st.st_uid + 1, (gid_t)-1), EPERM));

	/* Links and renames. */
	CHECK(link(a, b) == 0 && stat(a, &st) == 0 && st.st_nlink == 2);
	CHECK(symlink("a", s) == 0);
	ssize_t n = readlink(s, target, sizeof target - 1);
	CHECK(n == 1 && target[0] == 'a');
	CHECK(lstat(s, &st) == 0 && S_ISLNK(st.st_mode));
	CHECK(rename(b, c) == 0 && fails_with(stat(b, &st), ENOENT) && stat(c, &st) == 0);
	CHECK(fails_with(rmdir(dir), ENOTEMPTY));
	CHECK(fails_with(unlink(dir), EISDIR) || fails_with(unlink(dir), EPERM));

	/* The file system. */
	CHECK(statvfs(dir, &vfs) == 0 && vfs.f_bsize > 0 && vfs.f_blocks > 0);

	/* Removing. */
	CHECK(unlink(s) == 0 && unlink(c) == 0 && unlink(a) == 0);
	CHECK(fails_with(unlink(a), ENOENT));
	CHECK(rmdir(dir) == 0);

	/* The folders above namespace entries cannot be changed. */
	CHECK(fails_with(mkdir("/files-c", 0755), EROFS));
	CHECK(fails_with(mkdir("/Users", 0755), EEXIST));
	CHECK(fails_with(rmdir("/Users"), EROFS));

	printf("files, world\n");
	return 0;
}
