// SPDX-License-Identifier: GPL-2.0
/*
 * The slow paths of the shim's spinlocks and rwlocks: a thread that cannot
 * take one spins a little, then marks it as having waiters and waits on
 * its word, which the holder wakes when it lets go.
 */
#include <linux/spinlock.h>
#include <asm/sections.h>
#include "shim.h"

/* How often a thread spins before it waits. */
#define SPINS	100

/* in_lock_functions() compares addresses with these. */
char __lock_text_start[1], __lock_text_end[1];

void mikros_spin_lock_slow(arch_spinlock_t *lock)
{
	u32 c;
	int i;

	for (i = 0; i < SPINS; i++) {
		cpu_relax();
		if (READ_ONCE(lock->val) == 0 && arch_spin_trylock(lock))
			return;
	}
	c = __atomic_exchange_n(&lock->val, 2, __ATOMIC_ACQUIRE);
	while (c != 0) {
		mikros_wait(&lock->val, 2, MIKROS_HOST_FOREVER);
		c = __atomic_exchange_n(&lock->val, 2, __ATOMIC_ACQUIRE);
	}
}

void mikros_spin_unlock_slow(arch_spinlock_t *lock)
{
	mikros_wake(&lock->val, 1);
}

/* Takes `lock` once `free(value)`, as `take(value)`. */
static void rw_lock_slow(arch_rwlock_t *lock, bool write)
{
	u32 v, mine;
	int i;

	for (i = 0;; i++) {
		v = __atomic_load_n(&lock->val, __ATOMIC_RELAXED);
		if (write ? !(v & ~MIKROS_RW_WAITERS) : !(v & MIKROS_RW_WRITER)) {
			/* A writer that waited keeps the mark of waiters,
			 * as others may still wait. */
			mine = write ? MIKROS_RW_WRITER | (v & MIKROS_RW_WAITERS) : v + 1;
			if (__atomic_compare_exchange_n(&lock->val, &v, mine, false,
							__ATOMIC_ACQUIRE,
							__ATOMIC_RELAXED))
				return;
			continue;
		}
		if (i < SPINS) {
			cpu_relax();
			continue;
		}
		if (!(v & MIKROS_RW_WAITERS) &&
		    !__atomic_compare_exchange_n(&lock->val, &v, v | MIKROS_RW_WAITERS,
						 false, __ATOMIC_RELAXED,
						 __ATOMIC_RELAXED))
			continue;
		mikros_wait(&lock->val, v | MIKROS_RW_WAITERS, MIKROS_HOST_FOREVER);
	}
}

void mikros_read_lock_slow(arch_rwlock_t *lock)
{
	rw_lock_slow(lock, false);
}

void mikros_write_lock_slow(arch_rwlock_t *lock)
{
	rw_lock_slow(lock, true);
}

void mikros_rw_unlock_slow(arch_rwlock_t *lock)
{
	mikros_wake(&lock->val, INT_MAX);
}
