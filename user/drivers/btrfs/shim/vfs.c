// SPDX-License-Identifier: GPL-2.0
/*
 * The VFS of the release, set up as start_kernel() sets it up: its caches
 * and hash tables, the root file system, which is ramfs, as without tmpfs,
 * and /dev, which holds the nodes of the shim's disks. And what the VFS
 * needs from parts of the release that the shim leaves out: memory
 * mappings, which nothing makes without an MMU, the new mount API's file
 * descriptors, and pidfs.
 */
#include <linux/fs.h>
#include <linux/fs_struct.h>
#include <linux/fdtable.h>
#include <linux/init_syscalls.h>
#include <linux/mount.h>
#include <linux/mm.h>
#include <linux/ramfs.h>
#include <linux/slab.h>
#include <linux/cred.h>
#include <linux/seq_file.h>
#include "fs/internal.h"
#include "shim.h"

struct kmem_cache *fs_cachep;
struct kmem_cache *files_cachep;

const struct file_operations fscontext_fops;
const struct dentry_operations pidfs_dentry_operations;
const struct vm_operations_struct generic_file_vm_ops;

static int rootfs_init_fs_context(struct fs_context *fc)
{
	return ramfs_init_fs_context(fc);
}

struct file_system_type rootfs_fs_type = {
	.name		= "rootfs",
	.init_fs_context = rootfs_init_fs_context,
	.kill_sb	= kill_litter_super,
};

void init_rootfs(void)
{
}

void shmem_init(void)
{
}

bool path_noexec(const struct path *path)
{
	return (path->mnt->mnt_flags & MNT_NOEXEC) ||
	       (path->mnt->mnt_sb->s_iflags & SB_I_NOEXEC);
}

/* ramfs keeps the pages of its files without an MMU to map them. */
int nommu_shrink_inode_mappings(struct inode *inode, size_t size, size_t newsize)
{
	return 0;
}

void mikros_vfs_init(void)
{
	int ret;

	cred_init();
	fs_cachep = kmem_cache_create("fs_cache", sizeof(struct fs_struct), 0,
				      SLAB_HWCACHE_ALIGN | SLAB_PANIC, NULL);
	files_cachep = kmem_cache_create("files_cache", sizeof(struct files_struct), 0,
					 SLAB_HWCACHE_ALIGN | SLAB_PANIC, NULL);
	vfs_caches_init_early();
	vfs_caches_init();
	pagecache_init();
	seq_file_init();
	ret = init_mkdir("/dev", 0755);
	if (ret)
		panic("cannot make /dev: %d", ret);
}
