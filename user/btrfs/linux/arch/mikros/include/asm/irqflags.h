/* SPDX-License-Identifier: GPL-2.0 */
/*
 * No interrupts come to the program's threads; each keeps whether it
 * has them off, as the code it runs expects to see.
 */
#ifndef _ASM_MIKROS_IRQFLAGS_H
#define _ASM_MIKROS_IRQFLAGS_H

#define ARCH_IRQ_ENABLED	0
#define ARCH_IRQ_DISABLED	1

#ifndef __ASSEMBLY__
extern __thread unsigned long mikros_irq_flags;

static inline unsigned long arch_local_save_flags(void)
{
	return mikros_irq_flags;
}

static inline void arch_local_irq_restore(unsigned long flags)
{
	mikros_irq_flags = flags;
}

#define arch_local_save_flags	arch_local_save_flags
#define arch_local_irq_restore	arch_local_irq_restore
#endif

#include <asm-generic/irqflags.h>

#endif
