/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _ASM_MIKROS_SPINLOCK_H
#define _ASM_MIKROS_SPINLOCK_H

#include <asm/spinlock_types.h>
#include <asm/processor.h>

void mikros_spin_lock_slow(arch_spinlock_t *lock);
void mikros_spin_unlock_slow(arch_spinlock_t *lock);
void mikros_read_lock_slow(arch_rwlock_t *lock);
void mikros_write_lock_slow(arch_rwlock_t *lock);
void mikros_rw_unlock_slow(arch_rwlock_t *lock);

static __always_inline int arch_spin_trylock(arch_spinlock_t *lock)
{
	u32 free = 0;

	return __atomic_compare_exchange_n(&lock->val, &free, 1, false,
					   __ATOMIC_ACQUIRE, __ATOMIC_RELAXED);
}

static __always_inline void arch_spin_lock(arch_spinlock_t *lock)
{
	if (!arch_spin_trylock(lock))
		mikros_spin_lock_slow(lock);
}

static __always_inline void arch_spin_unlock(arch_spinlock_t *lock)
{
	if (__atomic_exchange_n(&lock->val, 0, __ATOMIC_RELEASE) == 2)
		mikros_spin_unlock_slow(lock);
}

static __always_inline int arch_spin_is_locked(arch_spinlock_t *lock)
{
	return __atomic_load_n(&lock->val, __ATOMIC_RELAXED) != 0;
}

static __always_inline int arch_spin_is_contended(arch_spinlock_t *lock)
{
	return __atomic_load_n(&lock->val, __ATOMIC_RELAXED) == 2;
}
#define arch_spin_is_contended	arch_spin_is_contended

static __always_inline int arch_spin_value_unlocked(arch_spinlock_t lock)
{
	return lock.val == 0;
}

static __always_inline int arch_read_trylock(arch_rwlock_t *lock)
{
	u32 val = __atomic_load_n(&lock->val, __ATOMIC_RELAXED);

	while (!(val & (MIKROS_RW_WRITER | MIKROS_RW_WAITERS))) {
		if (__atomic_compare_exchange_n(&lock->val, &val, val + 1, true,
						__ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
			return 1;
	}
	return 0;
}

static __always_inline int arch_write_trylock(arch_rwlock_t *lock)
{
	u32 free = 0;

	return __atomic_compare_exchange_n(&lock->val, &free, MIKROS_RW_WRITER,
					   false, __ATOMIC_ACQUIRE,
					   __ATOMIC_RELAXED);
}

static __always_inline void arch_read_lock(arch_rwlock_t *lock)
{
	if (!arch_read_trylock(lock))
		mikros_read_lock_slow(lock);
}

static __always_inline void arch_write_lock(arch_rwlock_t *lock)
{
	if (!arch_write_trylock(lock))
		mikros_write_lock_slow(lock);
}

static __always_inline void arch_read_unlock(arch_rwlock_t *lock)
{
	if (__atomic_sub_fetch(&lock->val, 1, __ATOMIC_RELEASE) == MIKROS_RW_WAITERS)
		mikros_rw_unlock_slow(lock);
}

static __always_inline void arch_write_unlock(arch_rwlock_t *lock)
{
	if (__atomic_exchange_n(&lock->val, 0, __ATOMIC_RELEASE) & MIKROS_RW_WAITERS)
		mikros_rw_unlock_slow(lock);
}

#endif
