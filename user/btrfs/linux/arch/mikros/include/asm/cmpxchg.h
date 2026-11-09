/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Exchanges with the compiler's atomic builtins, which are fully ordered
 * as the kernel's are.
 */
#ifndef _ASM_MIKROS_CMPXCHG_H
#define _ASM_MIKROS_CMPXCHG_H

#define arch_xchg(ptr, x)						\
	__atomic_exchange_n((ptr), (x), __ATOMIC_SEQ_CST)

#define arch_cmpxchg(ptr, old, new)					\
({									\
	__typeof__(*(ptr)) __old = (old);				\
	__atomic_compare_exchange_n((ptr), &__old, (new), false,	\
				    __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);\
	__old;								\
})

#define arch_cmpxchg64(ptr, old, new)		arch_cmpxchg((ptr), (old), (new))
#define arch_cmpxchg_local(ptr, old, new)	arch_cmpxchg((ptr), (old), (new))
#define arch_cmpxchg64_local(ptr, old, new)	arch_cmpxchg((ptr), (old), (new))

#define arch_try_cmpxchg(ptr, oldp, new)				\
	__atomic_compare_exchange_n((ptr), (oldp), (new), false,	\
				    __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)

#define arch_cmpxchg128(ptr, old, new)		arch_cmpxchg((ptr), (old), (new))
#define arch_cmpxchg128_local(ptr, old, new)	arch_cmpxchg((ptr), (old), (new))

#endif
