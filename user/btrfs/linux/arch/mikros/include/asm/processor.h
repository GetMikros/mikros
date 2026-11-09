/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _ASM_MIKROS_PROCESSOR_H
#define _ASM_MIKROS_PROCESSOR_H

#ifndef __ASSEMBLY__
struct task_struct;

struct thread_struct { };

#define INIT_THREAD	{ }

static __always_inline void cpu_relax(void)
{
	__builtin_ia32_pause();
}

#define TASK_SIZE	(~0UL)
#define TASK_UNMAPPED_BASE	0

static inline unsigned long __get_wchan(struct task_struct *p)
{
	return 0;
}

#define KSTK_EIP(tsk)	0
#define KSTK_ESP(tsk)	0
#endif

#endif
