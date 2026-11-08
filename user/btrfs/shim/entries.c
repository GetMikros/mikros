// SPDX-License-Identifier: GPL-2.0
/*
 * Changing the entries of directories: creating files, directories and
 * symbolic links, and removing entries. Each call takes write access to
 * the mount, locks the directory it changes as the release's system
 * calls do, and looks the name up under that lock.
 * Each runs as a task of the shim while it lasts, unless its thread runs
 * as one already.
 */
#include <linux/fs.h>
#include <linux/namei.h>
#include <linux/mount.h>
#include <linux/slab.h>
#include <linux/err.h>
#include "file.h"
#include "shim.h"

/* What mikros_btrfs_create() makes. */
enum {
	MIKROS_CREATE_FILE = 1,
	MIKROS_CREATE_DIRECTORY = 2,
	MIKROS_CREATE_SYMLINK = 3,
};

/* The entry `name` of the locked directory `dir`, negative if there is
 * none, or an ERR_PTR, -EACCES for "." or ".." or a name that holds a
 * slash or a NUL. */
static struct dentry *entry(struct dentry *dir, const char *name, unsigned int len)
{
	if (memchr(name, '\0', len))
		return ERR_PTR(-EACCES);
	return lookup_one(&nop_mnt_idmap, &QSTR_LEN(name, len), dir);
}

/* Makes `name`, of `len` bytes, in the directory `dir`: a file or a
 * directory with the permission bits of `mode`, or a symbolic link to
 * `target`. 0 and the new node in *nodep, or a negative errno, -EEXIST
 * when the name exists. */
int mikros_btrfs_create(struct mikros_node *dir, const char *name, unsigned int len, int kind,
			umode_t mode, const char *target, struct mikros_node **nodep)
{
	bool adopted = mikros_task_adopt("create");
	struct inode *inode = d_inode(dir->path.dentry);
	struct dentry *dentry;
	struct mikros_node *node;
	int ret;

	ret = -ENOTDIR;
	if (!d_can_lookup(dir->path.dentry))
		goto out;
	ret = mnt_want_write(dir->path.mnt);
	if (ret)
		goto out;
	inode_lock_nested(inode, I_MUTEX_PARENT);
	dentry = entry(dir->path.dentry, name, len);
	ret = PTR_ERR_OR_ZERO(dentry);
	if (ret)
		goto unlock;
	ret = -EEXIST;
	if (d_is_positive(dentry))
		goto put;
	mode &= S_IALLUGO;
	switch (kind) {
	case MIKROS_CREATE_FILE:
		ret = vfs_create(&nop_mnt_idmap, inode, dentry, S_IFREG | mode, true);
		break;
	case MIKROS_CREATE_DIRECTORY:
		/* vfs_mkdir() puts the dentry it fails with. */
		dentry = vfs_mkdir(&nop_mnt_idmap, inode, dentry, mode);
		ret = PTR_ERR_OR_ZERO(dentry);
		if (ret)
			goto unlock;
		break;
	case MIKROS_CREATE_SYMLINK:
		ret = vfs_symlink(&nop_mnt_idmap, inode, dentry, target);
		break;
	default:
		ret = -EINVAL;
	}
	if (ret)
		goto put;
	node = mikros_node_new(dir->path.mnt, dentry);
	ret = -ENOMEM;
	if (!node)
		goto put;
	*nodep = node;
	ret = 0;
	goto unlock;
put:
	dput(dentry);
unlock:
	inode_unlock(inode);
	mnt_drop_write(dir->path.mnt);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Removes the entry `name` of the directory `dir`, which must be an
 * empty directory if `directory`, and must not be one otherwise. 0, or a
 * negative errno. */
int mikros_btrfs_remove(struct mikros_node *dir, const char *name, unsigned int len,
			bool directory)
{
	bool adopted = mikros_task_adopt("remove");
	struct inode *inode = d_inode(dir->path.dentry);
	struct dentry *dentry;
	int ret;

	ret = -ENOTDIR;
	if (!d_can_lookup(dir->path.dentry))
		goto out;
	ret = mnt_want_write(dir->path.mnt);
	if (ret)
		goto out;
	inode_lock_nested(inode, I_MUTEX_PARENT);
	dentry = entry(dir->path.dentry, name, len);
	ret = PTR_ERR_OR_ZERO(dentry);
	if (ret)
		goto unlock;
	if (d_is_negative(dentry))
		ret = -ENOENT;
	else if (directory)
		ret = d_is_dir(dentry) ? vfs_rmdir(&nop_mnt_idmap, inode, dentry) : -ENOTDIR;
	else
		ret = d_is_dir(dentry) ? -EISDIR : vfs_unlink(&nop_mnt_idmap, inode, dentry, NULL);
	dput(dentry);
unlock:
	inode_unlock(inode);
	mnt_drop_write(dir->path.mnt);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}
