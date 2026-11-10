/* SPDX-License-Identifier: GPL-2.0 */
/* The program loads no ELF images; these only describe the target. */
#ifndef _ASM_MIKROS_ELF_H
#define _ASM_MIKROS_ELF_H

#define ELF_CLASS		ELFCLASS64
#define ELF_DATA		ELFDATA2LSB
#define ELF_ARCH		EM_X86_64
#define ELF_EXEC_PAGESIZE	4096
#define ELF_PLATFORM		NULL
#define ELF_HWCAP		0
#define ELF_ET_DYN_BASE		0
#define elf_check_arch(x)	((x)->e_machine == EM_X86_64)

typedef unsigned long elf_greg_t;
#define ELF_NGREG	1
typedef elf_greg_t elf_gregset_t[ELF_NGREG];
typedef struct { } elf_fpregset_t;

#endif
