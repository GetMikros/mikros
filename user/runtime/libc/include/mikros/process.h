/* The calling process as the process manager knows it, and the kernel
   handles behind its children and descriptors. */

#ifndef MIKROS_PROCESS_H
#define MIKROS_PROCESS_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

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

/* A process as the process manager describes it. app_id and path end
   in a NUL byte. */
struct mikros_process_info {
	pid_t pid;
	pid_t parent;
	pid_t process_group;
	pid_t session;
	uint32_t login_session;
	uid_t user;
	uint64_t koid;
	char app_id[129];
	char path[1025];
};

/* Stores the first max of the process IDs that the process manager
   lists to the caller in pids, and returns how many it listed, or -1
   with errno set. */
ssize_t mikros_process_list(pid_t *pids, size_t max);

/* Fills info with what the process manager knows of process pid, and
   returns 0, or -1 with errno set: ESRCH when there is no such process
   or the caller may not see it. */
int mikros_process_info(pid_t pid, struct mikros_process_info *info);

#ifdef __cplusplus
}
#endif

#endif
