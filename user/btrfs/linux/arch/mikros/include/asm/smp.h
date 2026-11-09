/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Each thread of the program is a CPU of its own, with the number it got
 * when it started, so that per-CPU data is per-thread data.
 */
#ifndef _ASM_MIKROS_SMP_H
#define _ASM_MIKROS_SMP_H

extern __thread int mikros_cpu;

#define raw_smp_processor_id()	(mikros_cpu)

#endif
