/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The services of the program that the shim runs in, which its host
 * layer provides. Mikros and the build host each have their own.
 */
#ifndef _MIKROS_SHIM_HOST_H
#define _MIKROS_SHIM_HOST_H

#include <linux/types.h>
#include <linux/compiler.h>

/* No deadline, for mikros_host_wait(). */
#define MIKROS_HOST_FOREVER	(-1LL)

/* `size` bytes of address space, page-aligned, that read as zero until
 * written and take memory only once touched; NULL when there are none. */
void *mikros_host_reserve(unsigned long size);

/* How many bytes of memory the shim may hold, which it sizes its caches
 * and the page cache's dirty limits by. */
unsigned long mikros_host_memory(void);

/* Starts a thread that runs `entry(arg)`; 0, or a negative errno. */
int mikros_host_thread(void (*entry)(void *arg), void *arg);

/* Waits, unless *word no longer holds `value`, until mikros_host_wake()
 * on `word` or the monotonic clock reaches `deadline`, or spuriously. */
void mikros_host_wait(u32 *word, u32 value, s64 deadline);

/* Wakes up to `count` threads waiting on `word`. */
void mikros_host_wake(u32 *word, int count);

/* The clocks, in nanoseconds: since an unspecified start, and since
 * 1970 in UTC. */
s64 mikros_host_monotonic(void);
s64 mikros_host_realtime(void);

void mikros_host_random(void *buf, unsigned long len);

/* Writes a line of the log, without its newline. */
void mikros_host_log(const char *text, unsigned long len);

/* Ends the program after logging `text`. */
void __noreturn mikros_host_abort(const char *text, unsigned long len);

#endif
