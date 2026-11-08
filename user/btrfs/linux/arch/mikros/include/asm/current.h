/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The task of the running thread, which each thread of the program keeps
 * in a thread-local variable.
 */
#ifndef _ASM_MIKROS_CURRENT_H
#define _ASM_MIKROS_CURRENT_H

#ifndef __ASSEMBLY__
struct task_struct;
extern __thread struct task_struct *mikros_current_task;

static __always_inline struct task_struct *get_current(void)
{
	return mikros_current_task;
}

#define current get_current()
#endif

#endif
