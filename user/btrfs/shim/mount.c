// SPDX-License-Identifier: GPL-2.0
/*
 * What the program asks of the file systems on the shim: mounting btrfs
 * from a disk's node in /dev, unmounting it, and its statistics. Each
 * call runs as a task of the shim while it lasts, unless its thread runs
 * as one already. A mount is a kernel-internal one, which belongs to no
 * namespace and goes as soon as it is unmounted.
 */
#include <linux/fs.h>
#include <linux/mount.h>
#include <linux/statfs.h>
#include <linux/slab.h>
#include <linux/err.h>
#include "shim.h"

/* The statistics of a mounted file system, as the program sees them. */
struct mikros_statfs {
	u64 type;
	u64 block_size;
	u64 blocks;
	u64 blocks_free;
	u64 blocks_available;
	u64 files;
	u64 files_free;
	u64 name_max;
};

/* Mounts the btrfs on `source`, read-only if `read_only`, with the mount
 * options `options`, which may be NULL: 0 and the mount in *mountp, or a
 * negative errno. */
int mikros_btrfs_mount(const char *source, bool read_only, const char *options,
		       struct vfsmount **mountp)
{
	bool adopted = mikros_task_adopt("mount");
	struct file_system_type *type;
	struct vfsmount *mnt;
	char *data = NULL;
	int ret = -ENOMEM;

	/* The options are parsed in place. */
	if (options) {
		data = kstrdup(options, GFP_KERNEL);
		if (!data)
			goto out;
	}
	ret = -ENODEV;
	type = get_fs_type("btrfs");
	if (!type)
		goto out;
	mnt = vfs_kern_mount(type, SB_KERNMOUNT | (read_only ? SB_RDONLY : 0), source, data);
	put_filesystem(type);
	ret = PTR_ERR_OR_ZERO(mnt);
	if (!ret)
		*mountp = mnt;
out:
	kfree(data);
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Unmounts a mount of mikros_btrfs_mount(), once nothing else uses it. */
void mikros_btrfs_unmount(struct vfsmount *mnt)
{
	bool adopted = mikros_task_adopt("unmount");

	kern_unmount(mnt);
	if (adopted)
		mikros_task_exit();
}

int mikros_btrfs_statfs(struct vfsmount *mnt, struct mikros_statfs *out)
{
	bool adopted = mikros_task_adopt("statfs");
	struct path root = { .mnt = mnt, .dentry = mnt->mnt_root };
	struct kstatfs st;
	int ret;

	ret = vfs_statfs(&root, &st);
	if (!ret) {
		out->type = st.f_type;
		out->block_size = st.f_bsize;
		out->blocks = st.f_blocks;
		out->blocks_free = st.f_bfree;
		out->blocks_available = st.f_bavail;
		out->files = st.f_files;
		out->files_free = st.f_ffree;
		out->name_max = st.f_namelen;
	}
	if (adopted)
		mikros_task_exit();
	return ret;
}
