/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _ASM_MIKROS_THREAD_INFO_H
#define _ASM_MIKROS_THREAD_INFO_H

#include <asm-generic/thread_info_tif.h>

#define THREAD_SIZE_ORDER	2
#define THREAD_SIZE		(PAGE_SIZE << THREAD_SIZE_ORDER)

#ifndef __ASSEMBLY__
struct thread_info {
	unsigned long		flags;
	int			preempt_count;
	unsigned int		cpu;
};

#define INIT_THREAD_INFO(tsk)	{ .flags = 0, .preempt_count = INIT_PREEMPT_COUNT }
#endif

#endif
