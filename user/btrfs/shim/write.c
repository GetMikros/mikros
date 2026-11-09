// SPDX-License-Identifier: GPL-2.0
/*
 * Changing nodes: writing and appending to files, truncating them,
 * making ranges of them share data, setting attributes, and syncing a
 * node to stable storage. Each call
 * runs as a task of the shim while it lasts, unless its thread runs as
 * one already.
 */
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/mount.h>
#include <linux/uio.h>
#include <linux/fsnotify.h>
#include <linux/err.h>
#include "file.h"
#include "shim.h"

/* Writes `count` bytes of `buf` to `file` at *pos, or at its end if
 * `append`, as __kernel_write() does: how many it wrote, with *pos after
 * them, or a negative errno. */
static ssize_t write_at(struct file *file, const void *buf, size_t count, loff_t *pos,
			bool append)
{
	struct kvec iov = { .iov_base = (void *)buf, .iov_len = min_t(size_t, count, MAX_RW_COUNT) };
	struct iov_iter iter;
	struct kiocb kiocb;
	ssize_t ret;

	if (!(file->f_mode & FMODE_WRITE))
		return __mnt_is_readonly(file->f_path.mnt) ? -EROFS : -EPERM;
	if (!append) {
		ret = rw_verify_area(WRITE, file, pos, iov.iov_len);
		if (ret)
			return ret;
	}
	init_sync_kiocb(&kiocb, file);
	kiocb.ki_pos = *pos;
	if (append)
		kiocb.ki_flags |= IOCB_APPEND;
	iov_iter_kvec(&iter, ITER_SOURCE, &iov, 1, iov.iov_len);
	file_start_write(file);
	ret = file->f_op->write_iter(&kiocb, &iter);
	if (ret > 0) {
		*pos = kiocb.ki_pos;
		fsnotify_modify(file);
	}
	file_end_write(file);
	return ret;
}

static long write_node(struct mikros_node *node, u64 *offset, const void *buf,
		       unsigned long count, bool append)
{
	bool adopted = mikros_task_adopt("write");
	loff_t pos = append ? 0 : *offset;
	struct file *file;
	long ret;

	if (d_is_dir(node->path.dentry)) {
		ret = -EISDIR;
		goto out;
	}
	file = mikros_node_file(node);
	ret = PTR_ERR_OR_ZERO(file);
	if (!ret)
		ret = write_at(file, buf, count, &pos, append);
	if (ret > 0)
		*offset = pos - ret;
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Writes `count` bytes of `buf` to a file at `offset`: how many it wrote,
 * or a negative errno. */
long mikros_btrfs_write(struct mikros_node *node, u64 offset, const void *buf,
			unsigned long count)
{
	return write_node(node, &offset, buf, count, false);
}

/* Writes `count` bytes of `buf` at the end of a file, which no other
 * write moves while it lasts: how many it wrote, with the offset it wrote
 * them at in *offset, or a negative errno. */
long mikros_btrfs_append(struct mikros_node *node, const void *buf, unsigned long count,
			 u64 *offset)
{
	return write_node(node, offset, buf, count, true);
}

int mikros_btrfs_truncate(struct mikros_node *node, u64 size)
{
	bool adopted = mikros_task_adopt("truncate");
	int ret = size > LLONG_MAX ? -EFBIG : vfs_truncate(&node->path, size);

	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Makes `len` bytes of the file `dst` from `dst_offset` share the data of
 * the file `src` from `src_offset`, as the FICLONERANGE ioctl does, a
 * `len` of 0 meaning to the end of `src`: how many bytes, or a negative
 * errno. */
long long mikros_btrfs_clone_range(struct mikros_node *dst, u64 dst_offset,
				   struct mikros_node *src, u64 src_offset, u64 len)
{
	bool adopted = mikros_task_adopt("clone_range");
	struct file *in, *out;
	loff_t ret;

	ret = -EISDIR;
	if (d_is_dir(dst->path.dentry) || d_is_dir(src->path.dentry))
		goto out;
	ret = -EINVAL;
	if (dst_offset > LLONG_MAX || src_offset > LLONG_MAX || len > LLONG_MAX)
		goto out;
	in = mikros_node_file(src);
	ret = PTR_ERR_OR_ZERO(in);
	if (ret)
		goto out;
	out = mikros_node_file(dst);
	ret = PTR_ERR_OR_ZERO(out);
	if (ret)
		goto out;
	if (!(out->f_mode & FMODE_WRITE)) {
		ret = __mnt_is_readonly(out->f_path.mnt) ? -EROFS : -EPERM;
		goto out;
	}
	ret = vfs_clone_file_range(in, src_offset, out, dst_offset, len, 0);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Which attributes mikros_btrfs_set_attributes() sets. */
enum {
	MIKROS_SET_MODE = 0x1,
	MIKROS_SET_ACCESSED = 0x2,
	MIKROS_SET_MODIFIED = 0x4,
	MIKROS_SET_OWNER = 0x8,
	MIKROS_SET_GROUP = 0x10,
};

static struct timespec64 timespec(const struct mikros_time *t)
{
	return (struct timespec64){ .tv_sec = t->seconds, .tv_nsec = t->nanoseconds };
}

/* Sets the attributes of a node that `mask` selects: the permission bits
 * of `mode`, the access and modification times, and the owner and group.
 * The change time becomes the present. 0, or a negative errno. */
int mikros_btrfs_set_attributes(struct mikros_node *node, u32 mask, u32 mode,
				const struct mikros_time *accessed,
				const struct mikros_time *modified, u32 owner, u32 group)
{
	bool adopted = mikros_task_adopt("set_attributes");
	struct dentry *dentry = node->path.dentry;
	struct inode *inode = d_inode(dentry);
	struct iattr ia = { .ia_valid = ATTR_CTIME };
	int ret;

	ret = -EINVAL;
	if ((mask & MIKROS_SET_ACCESSED && accessed->nanoseconds >= NSEC_PER_SEC) ||
	    (mask & MIKROS_SET_MODIFIED && modified->nanoseconds >= NSEC_PER_SEC))
		goto out;
	if (mask & MIKROS_SET_MODE) {
		ia.ia_valid |= ATTR_MODE;
		ia.ia_mode = (mode & S_IALLUGO) | (inode->i_mode & S_IFMT);
	}
	if (mask & MIKROS_SET_ACCESSED) {
		ia.ia_valid |= ATTR_ATIME | ATTR_ATIME_SET;
		ia.ia_atime = timespec(accessed);
	}
	if (mask & MIKROS_SET_MODIFIED) {
		ia.ia_valid |= ATTR_MTIME | ATTR_MTIME_SET;
		ia.ia_mtime = timespec(modified);
	}
	if (mask & MIKROS_SET_OWNER) {
		ia.ia_valid |= ATTR_UID;
		ia.ia_vfsuid = VFSUIDT_INIT(make_kuid(&init_user_ns, owner));
	}
	if (mask & MIKROS_SET_GROUP) {
		ia.ia_valid |= ATTR_GID;
		ia.ia_vfsgid = VFSGIDT_INIT(make_kgid(&init_user_ns, group));
	}
	ret = mnt_want_write(node->path.mnt);
	if (ret)
		goto out;
	inode_lock(inode);
	ret = notify_change(&nop_mnt_idmap, dentry, &ia, NULL);
	inode_unlock(inode);
	mnt_drop_write(node->path.mnt);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Puts a node's data and attributes, and the entries that name it, on
 * stable storage: through the file system's fsync for a file or a
 * directory, and by syncing the whole file system for anything else.
 * 0, or a negative errno. */
int mikros_btrfs_sync(struct mikros_node *node)
{
	bool adopted = mikros_task_adopt("sync");
	struct super_block *sb = node->path.dentry->d_sb;
	struct file *file;
	int ret;

	if (d_is_reg(node->path.dentry) || d_is_dir(node->path.dentry)) {
		file = mikros_node_file(node);
		ret = PTR_ERR_OR_ZERO(file);
		if (!ret)
			ret = vfs_fsync(file, 0);
	} else {
		down_read(&sb->s_umount);
		ret = sync_filesystem(sb);
		up_read(&sb->s_umount);
	}
	if (adopted)
		mikros_task_exit();
	return ret;
}
