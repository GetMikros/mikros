/* The startup handles of the calling process that the C library did not
   install as descriptors, its Directory connection and its own handle. */

#ifndef MIKROS_STARTUP_H
#define MIKROS_STARTUP_H

#include <stddef.h>
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

/* Stores in path, with a NUL after it, the path of the process's
   namespace entry with that index, from 0 in the order the process got
   them, and in directory the entry's connection, which the C library
   keeps owning: the caller must not close it. Returns 0, or -1 with
   errno ENOENT for an index of no entry and ERANGE when the path and its
   NUL do not fit in len bytes. Safe to call from any thread. */
int mikros_namespace_entry(size_t index, char *path, size_t len,
                           mikros_handle_t *directory);

/* Makes a descriptor of the kind (1 file or directory, 2 pipe, 3 timer,
   4 memory, 9 channel, 10 handle) that owns handle, and returns its
   number. Returns -1 with errno EBADF when handle is not a handle,
   EINVAL for another kind or an object of another type, and EMFILE with
   no free descriptor, leaving the handle with the caller. */
int mikros_fd_from_handle(mikros_handle_t handle, uint32_t kind);

/* Stores in handle a duplicate, with the same rights, of the handle of a
   pipe, memory, channel or handle descriptor, which the caller then
   owns, and returns 0. Returns -1 with errno EBADF for no descriptor,
   and EINVAL for another kind or a handle that cannot be duplicated. */
int mikros_handle_from_fd(int fd, mikros_handle_t *handle);

#ifdef __cplusplus
}
#endif

#endif
