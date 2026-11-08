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

/* Waits, unless *word no longer holds `value`, until mikros_host_wake()
 * on `word` or the monotonic clock reaches `deadline`, or spuriously. */
void mikros_host_wait(u32 *word, u32 value, s64 deadline);

/* Wakes up to `count` threads waiting on `word`. */
void mikros_host_wake(u32 *word, int count);


#endif
