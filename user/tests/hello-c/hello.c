/* A static C program for the C library tests. It prints a greeting
 * for its first argument to standard output, and exits with 0 when it
 * was given exactly one argument and every check below passed, and with
 * the number of the first check that failed otherwise. */

#include <errno.h>
#include <mikros/abi.h>
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
	char *copy = malloc(64);
	if (!copy)
		return 9;
	snprintf(copy, 64, "hello, %s", argv[1]);
	puts(copy);
	free(copy);
	return 0;
}
