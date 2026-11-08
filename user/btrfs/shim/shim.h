/* SPDX-License-Identifier: GPL-2.0 */
/* What the shim's files share with each other. */
#ifndef _MIKROS_SHIM_H
#define _MIKROS_SHIM_H

#include <linux/types.h>
#include <linux/compiler.h>
#include <linux/stdarg.h>
#include "host.h"

/* Waits on, and wakes, a word with the futexes of the host. */
static inline void mikros_wait(u32 *word, u32 value, s64 deadline)
{
	mikros_host_wait(word, value, deadline);
}

static inline void mikros_wake(u32 *word, int count)
{
	mikros_host_wake(word, count);
}

#endif
