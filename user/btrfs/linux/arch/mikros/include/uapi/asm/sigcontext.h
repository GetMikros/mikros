/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
#ifndef _UAPI_ASM_MIKROS_SIGCONTEXT_H
#define _UAPI_ASM_MIKROS_SIGCONTEXT_H

struct sigcontext {
	unsigned long ip;
	unsigned long sp;
};

#endif
