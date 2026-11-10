// SPDX-License-Identifier: GPL-2.0
/*
 * Subvolumes: making them, making snapshots of them, removing them,
 * making them read-only or writable, and listing them, through the
 * ioctls of btrfs on the files of the program's nodes, whose arguments
 * the shim's user copies take from its own memory. Each call runs as a
 * task of the shim while it lasts, unless its thread runs as one
 * already.
 */
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/fdtable.h>
#include <linux/slab.h>
#include <linux/err.h>
#include <linux/btrfs.h>
#include <linux/btrfs_tree.h>
#include "file.h"
#include "shim.h"

/* Runs the btrfs ioctl `cmd` with `arg` on the file of `node`. */
static long ioctl(struct mikros_node *node, unsigned int cmd, void *arg)
{
	struct file *file = mikros_node_file(node);

	if (IS_ERR(file))
		return PTR_ERR(file);
	if (!file->f_op->unlocked_ioctl)
		return -ENOTTY;
	return file->f_op->unlocked_ioctl(file, cmd, (unsigned long)arg);
}

/* Makes the subvolume `name`, of `len` bytes, in the directory `dir`: 0, or
 * a negative errno. */
int mikros_btrfs_subvolume_create(struct mikros_node *dir, const char *name, unsigned int len)
{
	bool adopted = mikros_task_adopt("subvolume_create");
	struct btrfs_ioctl_vol_args *args = kzalloc(sizeof(*args), GFP_KERNEL);
	int ret = -ENOMEM;

	if (!args)
		goto out;
	ret = -ENAMETOOLONG;
	if (len > BTRFS_PATH_NAME_MAX)
		goto free;
	memcpy(args->name, name, len);
	ret = ioctl(dir, BTRFS_IOC_SUBVOL_CREATE, args);
free:
	kfree(args);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Makes `name`, of `len` bytes, in the directory `dir` a snapshot of the
 * subvolume whose root `source` is, read-only if `read_only`: 0, or a
 * negative errno. The snapshot ioctl takes the source as a descriptor,
 * which the shim's tasks share the table of. */
int mikros_btrfs_subvolume_snapshot(struct mikros_node *source, struct mikros_node *dir,
				    const char *name, unsigned int len, bool read_only)
{
	bool adopted = mikros_task_adopt("subvolume_snapshot");
	struct btrfs_ioctl_vol_args_v2 *args = kzalloc(sizeof(*args), GFP_KERNEL);
	struct file *file;
	int fd, ret = -ENOMEM;

	if (!args)
		goto out;
	ret = -ENAMETOOLONG;
	if (len > BTRFS_SUBVOL_NAME_MAX)
		goto free;
	file = mikros_node_file(source);
	ret = PTR_ERR_OR_ZERO(file);
	if (ret)
		goto free;
	fd = get_unused_fd_flags(O_CLOEXEC);
	ret = fd;
	if (fd < 0)
		goto free;
	fd_install(fd, get_file(file));
	args->fd = fd;
	args->flags = read_only ? BTRFS_SUBVOL_RDONLY : 0;
	memcpy(args->name, name, len);
	ret = ioctl(dir, BTRFS_IOC_SNAP_CREATE_V2, args);
	close_fd(fd);
free:
	kfree(args);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Removes the subvolume `name`, of `len` bytes, in the directory `dir`:
 * 0, or a negative errno. */
int mikros_btrfs_subvolume_delete(struct mikros_node *dir, const char *name, unsigned int len)
{
	bool adopted = mikros_task_adopt("subvolume_delete");
	struct btrfs_ioctl_vol_args *args = kzalloc(sizeof(*args), GFP_KERNEL);
	int ret = -ENOMEM;

	if (!args)
		goto out;
	ret = -ENAMETOOLONG;
	if (len > BTRFS_PATH_NAME_MAX)
		goto free;
	memcpy(args->name, name, len);
	ret = ioctl(dir, BTRFS_IOC_SNAP_DESTROY, args);
free:
	kfree(args);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Makes the subvolume whose root `root` is read-only or writable: 0, or a
 * negative errno, -EINVAL for a node that is no subvolume's root. */
int mikros_btrfs_subvolume_set_read_only(struct mikros_node *root, bool read_only)
{
	bool adopted = mikros_task_adopt("subvolume_set_read_only");
	u64 flags;
	int ret = ioctl(root, BTRFS_IOC_SUBVOL_GETFLAGS, &flags);

	if (!ret) {
		flags = read_only ? flags | BTRFS_SUBVOL_RDONLY : flags & ~BTRFS_SUBVOL_RDONLY;
		ret = ioctl(root, BTRFS_IOC_SUBVOL_SETFLAGS, &flags);
	}
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Finds the first item of the root tree of `type` with an object ID from
 * `objectid` to `max` and an offset from `offset`, searching from the file
 * of `node`: 1 and its key and up to `size` bytes of it in `data`, 0 when
 * there is none, or a negative errno. */
static int search(struct mikros_node *node, struct btrfs_ioctl_search_args *args, u8 type,
		  u64 objectid, u64 max, u64 offset, struct btrfs_ioctl_search_header *found,
		  void *data, size_t size)
{
	struct btrfs_ioctl_search_header *header = (void *)args->buf;
	int ret;

	/* The search takes the keys from the first to the last in key order,
	 * other types between too, which are skipped. */
	for (;;) {
		memset(&args->key, 0, sizeof(args->key));
		args->key.tree_id = BTRFS_ROOT_TREE_OBJECTID;
		args->key.min_objectid = objectid;
		args->key.max_objectid = max;
		args->key.min_offset = offset;
		args->key.max_offset = (u64)-1;
		args->key.max_transid = (u64)-1;
		args->key.min_type = type;
		args->key.max_type = type;
		args->key.nr_items = 1;
		ret = ioctl(node, BTRFS_IOC_TREE_SEARCH, args);
		if (ret)
			return ret;
		if (!args->key.nr_items)
			return 0;
		if (header->type == type)
			break;
		/* An item of another type: the next one of this type comes at
		 * this object ID if the type is a smaller one, or after it. */
		if (header->type < type) {
			objectid = header->objectid;
		} else {
			if (header->objectid >= max)
				return 0;
			objectid = header->objectid + 1;
		}
		offset = 0;
	}
	*found = *header;
	memcpy(data, header + 1, min_t(size_t, size, header->len));
	return 1;
}

/* Stores the path of the subvolume `id` from the top-level subvolume in
 * `path`, of `size` bytes, as `a/b/c`: its length, or a negative errno,
 * -ENOENT for one that no directory holds, as one being removed. */
static int subvolume_path(struct mikros_node *node, struct btrfs_ioctl_search_args *args,
			  u64 id, char *path, int size)
{
	struct btrfs_ioctl_ino_lookup_args *lookup = kzalloc(sizeof(*lookup), GFP_KERNEL);
	struct btrfs_ioctl_search_header header;
	struct {
		struct btrfs_root_ref ref;
		char name[BTRFS_NAME_LEN];
	} item;
	int len = 0, ret = -ENOMEM;

	if (!lookup)
		return ret;
	path[0] = 0;
	while (id != BTRFS_FS_TREE_OBJECTID) {
		int name_len, dir_len, part;

		ret = search(node, args, BTRFS_ROOT_BACKREF_KEY, id, id, 0, &header, &item,
			     sizeof(item));
		if (ret <= 0) {
			ret = ret ?: -ENOENT;
			goto out;
		}
		name_len = min_t(int, le16_to_cpu(item.ref.name_len), BTRFS_NAME_LEN);
		/* The directory of the subvolume in its parent. */
		memset(lookup, 0, sizeof(*lookup));
		lookup->treeid = header.offset;
		lookup->objectid = le64_to_cpu(item.ref.dirid);
		ret = ioctl(node, BTRFS_IOC_INO_LOOKUP, lookup);
		if (ret)
			goto out;
		dir_len = strnlen(lookup->name, sizeof(lookup->name));
		part = dir_len + name_len + (len ? 1 : 0);
		ret = -ENAMETOOLONG;
		if (len + part >= size)
			goto out;
		memmove(path + part, path, len + 1);
		memcpy(path, lookup->name, dir_len);
		memcpy(path + dir_len, item.name, name_len);
		if (len)
			path[dir_len + name_len] = '/';
		len += part;
		id = header.offset;
	}
	ret = len;
out:
	kfree(lookup);
	return ret;
}

typedef bool (*mikros_subvolume_fn)(void *arg, const char *path, int len, u64 id,
				    bool read_only);

/* Calls `entry` for each subvolume with an ID above *cursor, in order of
 * IDs, with its path from the top-level subvolume, its ID and whether it
 * is read-only, until it returns false, searching from the file of `node`;
 * *cursor becomes the ID of the last one it took. Subvolumes being removed
 * are left out. 0, or a negative errno. */
int mikros_btrfs_subvolumes(struct mikros_node *node, u64 *cursor, mikros_subvolume_fn entry,
			    void *arg)
{
	bool adopted = mikros_task_adopt("subvolumes");
	struct btrfs_ioctl_search_args *args = kzalloc(sizeof(*args), GFP_KERNEL);
	char *path = kmalloc(PATH_MAX, GFP_KERNEL);
	struct btrfs_ioctl_search_header header;
	struct btrfs_root_item item;
	u64 next = max_t(u64, *cursor + 1, BTRFS_FIRST_FREE_OBJECTID);
	int ret = -ENOMEM;

	if (!args || !path)
		goto out;
	for (;;) {
		int len;

		ret = search(node, args, BTRFS_ROOT_ITEM_KEY, next, BTRFS_LAST_FREE_OBJECTID, 0,
			     &header, &item, sizeof(item));
		if (ret <= 0)
			break;
		next = header.objectid + 1;
		len = subvolume_path(node, args, header.objectid, path, PATH_MAX);
		if (len == -ENOENT)
			continue;
		ret = len;
		if (len < 0)
			break;
		if (!entry(arg, path, len, header.objectid,
			   le64_to_cpu(item.flags) & BTRFS_ROOT_SUBVOL_RDONLY)) {
			ret = 0;
			break;
		}
		*cursor = header.objectid;
	}
out:
	kfree(path);
	kfree(args);
	if (adopted)
		mikros_task_exit();
	return ret;
}
