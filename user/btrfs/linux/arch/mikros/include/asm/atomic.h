/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The atomic operations of the kernel with the compiler's builtins: the
 * fully ordered forms only, from which <linux/atomic.h> derives the rest.
 */
#ifndef _ASM_MIKROS_ATOMIC_H
#define _ASM_MIKROS_ATOMIC_H

#include <linux/types.h>
#include <asm/cmpxchg.h>
#include <asm/barrier.h>

#define ATOMIC64_INIT(i)	{ (i) }

#define arch_atomic_read(v)	__atomic_load_n(&(v)->counter, __ATOMIC_RELAXED)
#define arch_atomic_set(v, i)	__atomic_store_n(&(v)->counter, (i), __ATOMIC_RELAXED)
#define arch_atomic64_read(v)	__atomic_load_n(&(v)->counter, __ATOMIC_RELAXED)
#define arch_atomic64_set(v, i)	__atomic_store_n(&(v)->counter, (i), __ATOMIC_RELAXED)

#define ATOMIC_OPS(name, op, type, prefix, atype)				\
static __always_inline void prefix##_##name(type i, atype *v)	\
{									\
	__atomic_fetch_##op(&v->counter, i, __ATOMIC_SEQ_CST);		\
}									\
static __always_inline type prefix##_fetch_##name(type i, atype *v) \
{									\
	return __atomic_fetch_##op(&v->counter, i, __ATOMIC_SEQ_CST);	\
}									\
static __always_inline type prefix##_##name##_return(type i, atype *v) \
{									\
	return __atomic_##op##_fetch(&v->counter, i, __ATOMIC_SEQ_CST);	\
}

#define ATOMIC_BITS(name, op, type, prefix, atype)				\
static __always_inline void prefix##_##name(type i, atype *v)	\
{									\
	__atomic_fetch_##op(&v->counter, i, __ATOMIC_SEQ_CST);		\
}									\
static __always_inline type prefix##_fetch_##name(type i, atype *v) \
{									\
	return __atomic_fetch_##op(&v->counter, i, __ATOMIC_SEQ_CST);	\
}

ATOMIC_OPS(add, add, int, arch_atomic, atomic_t)
ATOMIC_OPS(sub, sub, int, arch_atomic, atomic_t)
ATOMIC_BITS(and, and, int, arch_atomic, atomic_t)
ATOMIC_BITS(or, or, int, arch_atomic, atomic_t)
ATOMIC_BITS(xor, xor, int, arch_atomic, atomic_t)
ATOMIC_OPS(add, add, s64, arch_atomic64, atomic64_t)
ATOMIC_OPS(sub, sub, s64, arch_atomic64, atomic64_t)
ATOMIC_BITS(and, and, s64, arch_atomic64, atomic64_t)
ATOMIC_BITS(or, or, s64, arch_atomic64, atomic64_t)
ATOMIC_BITS(xor, xor, s64, arch_atomic64, atomic64_t)

#undef ATOMIC_OPS
#undef ATOMIC_BITS

#define arch_atomic_add_return		arch_atomic_add_return
#define arch_atomic_sub_return		arch_atomic_sub_return
#define arch_atomic_fetch_add		arch_atomic_fetch_add
#define arch_atomic_fetch_sub		arch_atomic_fetch_sub
#define arch_atomic_fetch_and		arch_atomic_fetch_and
#define arch_atomic_fetch_or		arch_atomic_fetch_or
#define arch_atomic_fetch_xor		arch_atomic_fetch_xor
#define arch_atomic64_add_return	arch_atomic64_add_return
#define arch_atomic64_sub_return	arch_atomic64_sub_return
#define arch_atomic64_fetch_add		arch_atomic64_fetch_add
#define arch_atomic64_fetch_sub		arch_atomic64_fetch_sub
#define arch_atomic64_fetch_and		arch_atomic64_fetch_and
#define arch_atomic64_fetch_or		arch_atomic64_fetch_or
#define arch_atomic64_fetch_xor		arch_atomic64_fetch_xor

#define arch_atomic_xchg(v, n)		arch_xchg(&(v)->counter, (n))
#define arch_atomic_cmpxchg(v, o, n)	arch_cmpxchg(&(v)->counter, (o), (n))
#define arch_atomic_try_cmpxchg(v, o, n) arch_try_cmpxchg(&(v)->counter, (o), (n))
#define arch_atomic64_xchg(v, n)	arch_xchg(&(v)->counter, (n))
#define arch_atomic64_cmpxchg(v, o, n)	arch_cmpxchg(&(v)->counter, (o), (n))
#define arch_atomic64_try_cmpxchg(v, o, n) arch_try_cmpxchg(&(v)->counter, (o), (n))

#endif
