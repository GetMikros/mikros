/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Locks of the program's threads: a word that a thread that cannot take
 * the lock waits on with a futex, rather than spinning while the holder
 * may not run.
 */
#ifndef _ASM_MIKROS_SPINLOCK_TYPES_H
#define _ASM_MIKROS_SPINLOCK_TYPES_H

#include <linux/types.h>

/* 0 free, 1 held, 2 held with waiters. */
typedef struct {
	u32 val;
} arch_spinlock_t;

#define __ARCH_SPIN_LOCK_UNLOCKED	{ 0 }

/* The readers that hold it, or MIKROS_RW_WRITER; MIKROS_RW_WAITERS when
 * any thread waits. */
typedef struct {
	u32 val;
} arch_rwlock_t;

#define MIKROS_RW_WRITER	0x40000000u
#define MIKROS_RW_WAITERS	0x80000000u

#define __ARCH_RW_LOCK_UNLOCKED		{ 0 }

#endif
