/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _ASM_MIKROS_PTRACE_H
#define _ASM_MIKROS_PTRACE_H

#ifndef __ASSEMBLY__
struct pt_regs {
	unsigned long ip;
	unsigned long sp;
	unsigned long flags;
};

#define user_mode(regs)			0
#define instruction_pointer(regs)	((regs)->ip)
#define user_stack_pointer(regs)	((regs)->sp)
#define profile_pc(regs)		instruction_pointer(regs)
#define current_pt_regs()		((struct pt_regs *)NULL)
#define task_pt_regs(tsk)		((struct pt_regs *)NULL)
#endif

#endif
