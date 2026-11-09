/* SPDX-License-Identifier: GPL-2.0 */
/* What the shim's files share with each other. */
#ifndef _MIKROS_SHIM_H
#define _MIKROS_SHIM_H

#include <linux/types.h>
#include <linux/compiler.h>
#include <linux/stdarg.h>
#include "host.h"

struct task_struct;

/* Setting up each part, in the order mikros_btrfs_start() calls them. */
void mikros_mm_init(void);
void mikros_percpu_init(void);
void mikros_node_init(void);
void mikros_sched_init(void);
void mikros_time_init(void);
void mikros_rcu_init(void);
void mikros_workqueue_init(void);
void mikros_vfs_init(void);
void mikros_block_init(void);

/* Makes each change of a counter of the memory node reach its total. */
void mikros_node_exact(void);

/* Counts `pages` more free pages, or fewer when negative. */
void mikros_node_account(long pages);

/* A task for a thread that is not running yet, named after `fmt`. */
struct task_struct *mikros_task_create(const char *fmt, va_list args);
__printf(1, 2) struct task_struct *mikros_task_createf(const char *fmt, ...);

/* Makes the calling thread run as `task`, on a CPU of its own. */
void mikros_task_enter(struct task_struct *task);

/* Makes the calling thread, unless it runs as a task already, run as a
 * new one named `name`: whether it did, and the thread should end it with
 * mikros_task_exit() once it leaves the shim. */
bool mikros_task_adopt(const char *name);

/* Ends the calling thread's task, which gives up its CPU. */
void mikros_task_exit(void);

/* Calls `fn(task)` for each task, under the lock of the task list. */
void mikros_for_each_task(void (*fn)(struct task_struct *task, void *arg), void *arg);

/* Starts a thread that runs `entry(arg)` as `task`. */
int mikros_task_start(struct task_struct *task, void (*entry)(void *arg), void *arg);

/* Logs a formatted line. */
__printf(1, 2) void mikros_log(const char *fmt, ...);

/* Waits on, and wakes, a word with the futexes of the host. */
static inline void mikros_wait(u32 *word, u32 value, s64 deadline)
{
	mikros_host_wait(word, value, deadline);
}

static inline void mikros_wake(u32 *word, int count)
{
	mikros_host_wake(word, count);
}

#endif
