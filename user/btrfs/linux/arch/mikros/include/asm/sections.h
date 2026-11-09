/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The program's read-only data is what its read-only segments hold, which
 * the shim finds in its program headers, rather than what lies between
 * the symbols of the release's linker script, which links no program here.
 */
#ifndef _ASM_MIKROS_SECTIONS_H
#define _ASM_MIKROS_SECTIONS_H

#define is_kernel_rodata generic_is_kernel_rodata
#include <asm-generic/sections.h>
#undef is_kernel_rodata

bool mikros_is_rodata(unsigned long addr);

static inline bool is_kernel_rodata(unsigned long addr)
{
	return mikros_is_rodata(addr);
}

#endif
