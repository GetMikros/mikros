/* SPDX-License-Identifier: GPL-2.0 */
/* Without an MMU, there are no page tables to describe. */
#ifndef _ASM_MIKROS_PGTABLE_H
#define _ASM_MIKROS_PGTABLE_H

#include <asm/page.h>
#include <asm-generic/pgtable-nopmd.h>

#define PAGE_NONE	__pgprot(0)
#define PAGE_SHARED	__pgprot(0)
#define PAGE_COPY	__pgprot(0)
#define PAGE_READONLY	__pgprot(0)
#define PAGE_KERNEL	__pgprot(0)

#define swapper_pg_dir	((pgd_t *)NULL)

#define VMALLOC_START	0UL
#define VMALLOC_END	(~0UL)

#ifndef __ASSEMBLY__
extern unsigned long empty_zero_page[PAGE_SIZE / sizeof(unsigned long)];
#define ZERO_PAGE(vaddr)	(virt_to_page(empty_zero_page))
#endif

#endif
