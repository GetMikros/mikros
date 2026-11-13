// SPDX-License-Identifier: GPL-2.0
/*
 * Linux's wait queues, wait-on-bit, simple waits and completions, which
 * the release builds into kernel/sched/build_utility.c, built on the
 * shim's tasks with only the headers they need.
 */
#include <linux/sched.h>
#include <linux/sched/signal.h>
#include <linux/sched/debug.h>
#include <linux/wait_api.h>
#include <linux/swait_api.h>
#include <linux/wait_bit.h>
#include <linux/completion.h>
#include <linux/spinlock_api.h>
#include <linux/hash.h>

#include "kernel/sched/wait.c"
#include "kernel/sched/wait_bit.c"
#include "kernel/sched/swait.c"
#include "kernel/sched/completion.c"
