// SPDX-License-Identifier: GPL-2.0
/*
 * Nodes of a mounted file system, which the program walks one name at a
 * time: looking a name up in a directory, a node's attributes, reading a
 * file, reading a symbolic link and listing a directory. A node is a path
 * of the VFS, with a file that is opened for reading the first time the
 * node is read or listed. Each call runs as a task of the shim while it
 * lasts, unless its thread runs as one already.
 */
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/namei.h>
#include <linux/mount.h>
#include <linux/stat.h>
#include <linux/slab.h>
#include <linux/err.h>
#include "shim.h"

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

/* The attributes of a node, as the program sees them. */
struct mikros_attributes {
	u32 mode;
	u32 owner;
	u32 group;
	u32 unused;
	u64 size;
	u64 allocated;
	u64 links;
	u64 inode;
	struct mikros_time created;
	struct mikros_time modified;
	struct mikros_time accessed;
	struct mikros_time changed;
};

static struct mikros_node *new_node(struct vfsmount *mnt, struct dentry *dentry)
{
	struct mikros_node *node = kzalloc(sizeof(*node), GFP_KERNEL);

	if (!node)
		return NULL;
	node->path.mnt = mntget(mnt);
	node->path.dentry = dentry;
	mutex_init(&node->lock);
	return node;
}

/* The root directory of a mount of mikros_btrfs_mount(): 0 and the node in
 * *nodep, or a negative errno. */
int mikros_btrfs_root(struct vfsmount *mnt, struct mikros_node **nodep)
{
	bool adopted = mikros_task_adopt("root");
	struct mikros_node *node = new_node(mnt, dget(mnt->mnt_root));

	if (node)
		*nodep = node;
	else
		dput(mnt->mnt_root);
	if (adopted)
		mikros_task_exit();
	return node ? 0 : -ENOMEM;
}

/* The node that `name`, of `len` bytes, names in the directory `dir`,
 * without following it if it is a symbolic link: 0 and the node in
 * *nodep, or a negative errno, such as -ENOENT, -ENOTDIR, or -EACCES for
 * a name that is "." or "..", or holds a slash or a NUL. */
int mikros_btrfs_lookup(struct mikros_node *dir, const char *name, unsigned int len,
			struct mikros_node **nodep)
{
	bool adopted = mikros_task_adopt("lookup");
	struct dentry *dentry;
	struct mikros_node *node;
	int ret;

	ret = -ENOTDIR;
	if (!d_can_lookup(dir->path.dentry))
		goto out;
	ret = -EACCES;
	if (memchr(name, '\0', len))
		goto out;
	dentry = lookup_one_positive_unlocked(mnt_idmap(dir->path.mnt), &QSTR_LEN(name, len),
					      dir->path.dentry);
	ret = PTR_ERR_OR_ZERO(dentry);
	if (ret)
		goto out;
	node = new_node(dir->path.mnt, dentry);
	ret = -ENOMEM;
	if (!node) {
		dput(dentry);
		goto out;
	}
	*nodep = node;
	ret = 0;
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Lets go of a node. */
void mikros_btrfs_put(struct mikros_node *node)
{
	bool adopted = mikros_task_adopt("put");

	/* Closed now rather than by deferred work, so that the mount can
	 * end as soon as it is unmounted. */
	if (node->file)
		__fput_sync(node->file);
	path_put(&node->path);
	kfree(node);
	if (adopted)
		mikros_task_exit();
}

static void set_time(struct mikros_time *out, struct timespec64 ts)
{
	out->seconds = ts.tv_sec;
	out->nanoseconds = ts.tv_nsec;
	out->unused = 0;
}

int mikros_btrfs_get_attributes(struct mikros_node *node, struct mikros_attributes *out)
{
	bool adopted = mikros_task_adopt("get_attributes");
	struct kstat st;
	int ret;

	ret = vfs_getattr(&node->path, &st, STATX_BASIC_STATS | STATX_BTIME,
			  AT_STATX_SYNC_AS_STAT);
	if (!ret) {
		memset(out, 0, sizeof(*out));
		out->mode = st.mode;
		out->owner = from_kuid(&init_user_ns, st.uid);
		out->group = from_kgid(&init_user_ns, st.gid);
		out->size = st.size;
		out->allocated = st.blocks * 512;
		out->links = st.nlink;
		out->inode = st.ino;
		if (st.result_mask & STATX_BTIME)
			set_time(&out->created, st.btime);
		set_time(&out->modified, st.mtime);
		set_time(&out->accessed, st.atime);
		set_time(&out->changed, st.ctime);
	}
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* The node's file, opened for reading unless it is: a regular file or
 * a directory, as nothing else can be read or listed. */
static struct file *node_file(struct mikros_node *node)
{
	struct inode *inode = d_inode(node->path.dentry);
	struct file *file;

	if (!S_ISREG(inode->i_mode) && !S_ISDIR(inode->i_mode))
		return ERR_PTR(-EINVAL);
	mutex_lock(&node->lock);
	file = node->file;
	if (!file) {
		file = dentry_open(&node->path, O_RDONLY | O_LARGEFILE |
				   (S_ISDIR(inode->i_mode) ? O_DIRECTORY : 0), current_cred());
		if (!IS_ERR(file))
			node->file = file;
	}
	mutex_unlock(&node->lock);
	return file;
}

/* Reads up to `count` bytes of a file from `offset` into `buf`: how many
 * it read, 0 at the end, or a negative errno, -EISDIR for a directory. */
long mikros_btrfs_read(struct mikros_node *node, u64 offset, void *buf, unsigned long count)
{
	bool adopted = mikros_task_adopt("read");
	loff_t pos = offset;
	struct file *file;
	long ret;

	if (d_is_dir(node->path.dentry)) {
		ret = -EISDIR;
		goto out;
	}
	file = node_file(node);
	ret = PTR_ERR_OR_ZERO(file);
	if (!ret)
		ret = kernel_read(file, buf, count, &pos);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Copies the target of a symbolic link, without a NUL, into the `size`
 * bytes at `buf`: its length, or a negative errno, -EINVAL for a node
 * that is not a link. */
int mikros_btrfs_read_link(struct mikros_node *node, char *buf, int size)
{
	bool adopted = mikros_task_adopt("read_link");
	int ret = vfs_readlink(node->path.dentry, (char __user *)buf, size);

	if (adopted)
		mikros_task_exit();
	return ret;
}

typedef bool (*mikros_entry_fn)(void *arg, const char *name, int len, u64 inode,
				unsigned int type);

struct listing {
	struct dir_context ctx;
	mikros_entry_fn entry;
	void *arg;
};

static bool fill(struct dir_context *ctx, const char *name, int len, loff_t pos, u64 inode,
		 unsigned int type)
{
	struct listing *listing = container_of(ctx, struct listing, ctx);

	return listing->entry(listing->arg, name, len, inode, type);
}

/* Lists the directory from position *cookie, 0 at its start, calling
 * `entry(arg, name, len, inode, type)` for each entry, "." and ".." too,
 * with the type as a DT_ value, until `entry` returns false for an entry,
 * which is then not taken, or the directory ends. *cookie becomes the
 * position to go on from. 0, or a negative errno. */
int mikros_btrfs_read_dir(struct mikros_node *dir, u64 *cookie, mikros_entry_fn entry, void *arg)
{
	bool adopted = mikros_task_adopt("read_dir");
	struct listing listing = { .ctx.actor = fill, .entry = entry, .arg = arg };
	struct file *file;
	loff_t pos;
	int ret;

	ret = -ENOTDIR;
	if (!d_is_dir(dir->path.dentry))
		goto out;
	file = node_file(dir);
	ret = PTR_ERR_OR_ZERO(file);
	if (ret)
		goto out;
	mutex_lock(&dir->lock);
	pos = vfs_llseek(file, *cookie, SEEK_SET);
	ret = pos < 0 ? pos : 0;
	if (!ret)
		ret = iterate_dir(file, &listing.ctx);
	if (!ret)
		*cookie = file->f_pos;
	mutex_unlock(&dir->lock);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}
