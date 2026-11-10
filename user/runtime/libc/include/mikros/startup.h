/* The startup handles of the calling process that the C library did not
   install as descriptors, its Directory connection and its own handle. */

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

/* Returns the calling process's own handle, with the MAP right, so that
   memory objects can be mapped with vm_map. The C library keeps owning
   it: the caller must not close it. */
mikros_handle_t mikros_process_self(void);

#ifdef __cplusplus
}
#endif

#endif
