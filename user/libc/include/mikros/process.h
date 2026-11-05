/* The calling process as the process manager knows it. */

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

#ifdef __cplusplus
}
#endif

#endif
