/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The program provides memcpy, memmove, memset and memcmp itself; the
 * rest of <linux/string.h> comes from lib/string.c.
 */
#ifndef _ASM_MIKROS_STRING_H
#define _ASM_MIKROS_STRING_H

#define __HAVE_ARCH_MEMCPY
#define __HAVE_ARCH_MEMMOVE
#define __HAVE_ARCH_MEMSET
#define __HAVE_ARCH_MEMCMP

#ifndef __ASSEMBLY__
#include <linux/types.h>

extern void *memcpy(void *dest, const void *src, __kernel_size_t n);
extern void *memmove(void *dest, const void *src, __kernel_size_t n);
extern void *memset(void *s, int c, __kernel_size_t n);
extern int memcmp(const void *a, const void *b, __kernel_size_t n);
#endif

#endif
