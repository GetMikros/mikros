/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Barriers of a Mikros thread: the compiler's fences, which order the
 * memory accesses of threads on any processor.
 */
#ifndef _ASM_MIKROS_BARRIER_H
#define _ASM_MIKROS_BARRIER_H

#define mb()	__atomic_thread_fence(__ATOMIC_SEQ_CST)
#define rmb()	__atomic_thread_fence(__ATOMIC_ACQUIRE)
#define wmb()	__atomic_thread_fence(__ATOMIC_RELEASE)

#define __smp_mb()	mb()
#define __smp_rmb()	rmb()
#define __smp_wmb()	wmb()

#include <asm-generic/barrier.h>

#endif
