/* A static C program for the C library tests. It prints a greeting
 * for its first argument to standard output, and exits with 0 when it
 * was given exactly one argument and every check below passed, and with
 * the number of the first check that failed otherwise. */

#include <errno.h>
#include <mikros/syscalls.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

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

/* Without a time page, the time of day is not known yet. */
static int realtime_waits_for_the_time_page(void)
{
	struct timespec ts;
	errno = 0;
	return clock_gettime(CLOCK_REALTIME, &ts) == -1 && errno == EAGAIN;
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
	struct timespec ts;
	if (clock_gettime(42, &ts) != -1 || errno != EINVAL)
		return 6;
	char *copy = malloc(64);
	if (!copy)
		return 7;
	snprintf(copy, 64, "hello, %s", argv[1]);
	puts(copy);
	free(copy);
	return 0;
}
