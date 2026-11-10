/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The preemption count of a thread, as asm-generic/preempt.h keeps it,
 * except that it changes with atomic operations, which order the
 * thread's memory accesses fully, and that each return to zero counts in
 * thread_info's quiescent count. The shim's RCU tells that a thread has
 * left the read-side sections it was in from those two.
 */
#ifndef __ASM_PREEMPT_H
#define __ASM_PREEMPT_H

#include <linux/thread_info.h>

#define PREEMPT_ENABLED	(0)

static __always_inline int preempt_count(void)
{
	return READ_ONCE(current_thread_info()->preempt_count);
}

static __always_inline volatile int *preempt_count_ptr(void)
{
	return &current_thread_info()->preempt_count;
}

static __always_inline void preempt_count_set(int pc)
{
	__atomic_store_n((int *)preempt_count_ptr(), pc, __ATOMIC_SEQ_CST);
}

#define init_task_preempt_count(p) do { \
	task_thread_info(p)->preempt_count = FORK_PREEMPT_COUNT; \
} while (0)

#define init_idle_preempt_count(p, cpu) do { \
	task_thread_info(p)->preempt_count = PREEMPT_DISABLED; \
} while (0)

static __always_inline void set_preempt_need_resched(void)
{
}

static __always_inline void clear_preempt_need_resched(void)
{
}

static __always_inline bool test_preempt_need_resched(void)
{
	return false;
}

static __always_inline void __preempt_count_add(int val)
{
	__atomic_fetch_add((int *)preempt_count_ptr(), val, __ATOMIC_SEQ_CST);
}

static __always_inline int __preempt_count_sub_return(int val)
{
	int left = __atomic_sub_fetch((int *)preempt_count_ptr(), val,
				      __ATOMIC_SEQ_CST);

	if (!left)
		__atomic_fetch_add(&current_thread_info()->quiescent, 1,
				   __ATOMIC_RELEASE);
	return left;
}

static __always_inline void __preempt_count_sub(int val)
{
	__preempt_count_sub_return(val);
}

static __always_inline bool __preempt_count_dec_and_test(void)
{
	return !__preempt_count_sub_return(1) && tif_need_resched();
}

static __always_inline bool should_resched(int preempt_offset)
{
	return unlikely(preempt_count() == preempt_offset &&
			tif_need_resched());
}

#endif /* __ASM_PREEMPT_H */
