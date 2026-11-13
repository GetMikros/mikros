/* SPDX-License-Identifier: GPL-2.0 */
/* The nodes of file.c, which the files that change nodes share. */
#ifndef _MIKROS_SHIM_FILE_H
#define _MIKROS_SHIM_FILE_H

#include <linux/path.h>
#include <linux/mutex.h>

struct mikros_node {
	struct path path;
	/* Opened at the first read, write or listing, under the lock,
	 * which also keeps listings of the node from moving each other's
	 * position. */
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

/* The node's file, opened for reading, and for writing too where the
 * mount and the file allow, unless it is: a regular file or a
 * directory, as nothing else can be read, written or listed. */
struct file *mikros_node_file(struct mikros_node *node);

#endif
