/* SPDX-License-Identifier: GPL-2.0 */
/* The nodes of file.c, which the files that change nodes share. */
#ifndef _MIKROS_SHIM_FILE_H
#define _MIKROS_SHIM_FILE_H

#include <linux/path.h>
#include <linux/mutex.h>

struct mikros_node {
	struct path path;
	/* Opened at the first read or listing, under the lock, which also
	 * keeps listings of the node from moving each other's position. */
	struct file *file;
	struct mutex lock;
};

/* A time, as the program sees it. */
struct mikros_time {
	s64 seconds;
	u32 nanoseconds;
	u32 unused;
};

/* A node for `dentry` of `mnt`, whose reference it takes over. */
struct mikros_node *mikros_node_new(struct vfsmount *mnt, struct dentry *dentry);

#endif
