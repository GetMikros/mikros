/* The calling process as the process manager knows it, and the kernel
   handles behind its children and descriptors. */

#ifndef MIKROS_PROCESS_H
#define MIKROS_PROCESS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Copies at most len bytes of the executable path of the calling
   process, without a terminating NUL, to buf, and returns the whole
   length of the path, or a negated error number. */
int64_t mikros_executable_path(char *buf, size_t len);

/* Takes the process handle of the child pid that posix_spawn started,
   which the caller then owns, or returns -ECHILD when pid is not such a
   child, was waited for, or its handle was taken already. */
int64_t mikros_process_handle(int pid);

/* Returns a new handle, which the caller owns, to the object behind
   descriptor fd: the stream of a pipe, or the connection to a file or
   directory, or a negated error number. */
int64_t mikros_fd_handle(int fd);

#ifdef __cplusplus
}
#endif

#endif
