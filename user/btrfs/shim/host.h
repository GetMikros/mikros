/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The services of the program that the shim runs in, which its host
 * layer provides. Mikros and the build host each have their own.
 */
#ifndef _MIKROS_SHIM_HOST_H
#define _MIKROS_SHIM_HOST_H

#include <linux/types.h>
#include <linux/compiler.h>

/* No deadline, for mikros_host_wait(). */
#define MIKROS_HOST_FOREVER	(-1LL)

/* `size` bytes of address space, page-aligned, that read as zero until
 * written and take memory only once touched; NULL when there are none. */
void *mikros_host_reserve(unsigned long size);

/* How many bytes of memory the shim may hold, which it sizes its caches
 * and the page cache's dirty limits by. */
unsigned long mikros_host_memory(void);

/* Starts a thread that runs `entry(arg)`; 0, or a negative errno. */
int mikros_host_thread(void (*entry)(void *arg), void *arg);

/* Waits, unless *word no longer holds `value`, until mikros_host_wake()
 * on `word` or the monotonic clock reaches `deadline`, or spuriously. */
void mikros_host_wait(u32 *word, u32 value, s64 deadline);

/* Wakes up to `count` threads waiting on `word`. */
void mikros_host_wake(u32 *word, int count);

/* The clocks, in nanoseconds: since an unspecified start, and since
 * 1970 in UTC. */
s64 mikros_host_monotonic(void);
s64 mikros_host_realtime(void);

void mikros_host_random(void *buf, unsigned long len);

/* Reads, writes, flushes or discards the host's disk `disk`, a handle that
 * the host gave mikros_btrfs_add_disk(), before returning: 0, or a
 * negative errno. Only disks added as discarding are discarded, in whole
 * blocks. */
int mikros_host_disk_read(void *disk, u64 offset, void *buf, unsigned long len);
int mikros_host_disk_write(void *disk, u64 offset, const void *buf, unsigned long len);
int mikros_host_disk_flush(void *disk);
int mikros_host_disk_discard(void *disk, u64 offset, u64 len);

/* Runs the pager op `op` on `count` pages from `first` of the paged memory
 * object that the host names `cookie`, the object of a shared file
 * (share.c): 0, -EBUSY while a writeback of the pages is outstanding, or
 * another negative errno. */
int mikros_host_share_op(u64 cookie, u32 op, u64 first, u64 count);
/* Stores up to `cap` ranges of dirty pages of the object `cookie` among
 * the `count` pages from `first`, as pairs of the first page and the
 * number of pages, in `ranges`: how many, or a negative errno. */
long mikros_host_share_dirty(u64 cookie, u64 first, u64 count, u64 *ranges, long cap);

/* Writes a line of the log, without its newline. */
void mikros_host_log(const char *text, unsigned long len);

/* Ends the program after logging `text`. */
void __noreturn mikros_host_abort(const char *text, unsigned long len);

#endif
