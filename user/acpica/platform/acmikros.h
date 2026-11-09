/* SPDX-License-Identifier: BSD-3-Clause */
/*
 * ACPICA's platform header for Mikros, where it runs in the ACPI server,
 * a user-space driver. The release's acenv.h includes it in place of the
 * line that stops a build for a platform it does not know.
 *
 * ACPICA is compiled freestanding with its own C library routines; the
 * server's operating system layer, the AcpiOs functions, provides memory,
 * locks, semaphores, threads and time.
 */
#ifndef __ACMIKROS_H__
#define __ACMIKROS_H__

/* x86_64 only, with 64-bit integers of the compiler's own. */
#define ACPI_MACHINE_WIDTH          64
#define COMPILER_DEPENDENT_INT64    long
#define COMPILER_DEPENDENT_UINT64   unsigned long
#define ACPI_USE_NATIVE_DIVIDE
#define ACPI_USE_NATIVE_MATH64

/* ACPICA keeps its object caches itself. */
#define ACPI_USE_LOCAL_CACHE
#define ACPI_CACHE_T                ACPI_MEMORY_LIST

/* Mutexes are the operating system layer's own, as are spinlocks. */
#define ACPI_MUTEX_TYPE             ACPI_OSL_MUTEX
#define ACPI_SPINLOCK               void *
#define ACPI_CPU_FLAGS              unsigned long

#define ACPI_USE_DO_WHILE_0
#define ACPI_INLINE                 __inline__

#endif /* __ACMIKROS_H__ */
