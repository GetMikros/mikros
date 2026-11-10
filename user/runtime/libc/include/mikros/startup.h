/* The startup handles of the calling process that the C library did not
   install as descriptors, and its Directory connection. */

#ifndef MIKROS_STARTUP_H
#define MIKROS_STARTUP_H

#include <stdint.h>
#include <mikros/syscalls.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Returns the startup handle with role and id that the C library did not
   install as a descriptor or keep, which the caller then owns. Returns 0,
   HANDLE_INVALID, after that, and for a role and id the process did not
   get. Safe to call from any thread. */
mikros_handle_t mikros_take_startup_handle(uint32_t role, uint32_t id);

/* Returns the process's Directory connection, which the C library keeps
   owning: the caller must not close it. Safe to call from any thread. */
mikros_handle_t mikros_directory(void);

#ifdef __cplusplus
}
#endif

#endif
