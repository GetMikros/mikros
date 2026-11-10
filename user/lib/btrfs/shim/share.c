// SPDX-License-Identifier: GPL-2.0
/*
 * Files whose page cache the program shares: it lends the frames of their
 * folios to a paged memory object of its own, which the host names by a
 * cookie, so that clients that map the object and the page cache see each
 * other's writes. The shim holds a folio reference for every lent page,
 * so that the folio's memory is not reused while lent, and gives the
 * inode a copy of its address space operations with two of them wrapped:
 *
 * - free_folio, which filemap_free_folio() calls once a folio has left the
 *   page cache, by truncation, hole punching or invalidation: the host
 *   discards the object's pages of the folio before its reference goes;
 * - writepages: while btrfs writes a shared file back, no page that it
 *   writes may change, as btrfs checksums data before the disk worker
 *   copies it. A writeback window holds off new lends, the host evicts
 *   the object's clean pages, so that every page still lent is dirty, and
 *   begins a stable writeback of them, which makes writes to them wait;
 *   their folios are marked dirty, btrfs writes every dirty folio and the
 *   shim waits for it, and the writeback ends.
 *
 * Reclaim lets go of the folios of pages that the objects no longer have:
 * a shrinker evicts the clean pages of each object it can lock without
 * waiting, and drops the references of the lent pages that are not dirty,
 * as the object then has only those.
 *
 * Each call runs as a task of the shim while it lasts, unless its thread
 * runs as one already.
 */
#include <linux/fs.h>
#include <linux/mount.h>
#include <linux/pagemap.h>
#include <linux/writeback.h>
#include <linux/xarray.h>
#include <linux/slab.h>
#include <linux/wait.h>
#include <linux/sched.h>
#include <linux/shrinker.h>
#include <linux/err.h>
#include "mm/internal.h"
#include "file.h"
#include "shim.h"

/* The ops of mikros_host_share_op(), those of pager_op. */
#define SHARE_EVICT		2
#define SHARE_WRITEBACK_BEGIN	3
#define SHARE_WRITEBACK_END	4
#define SHARE_WRITEBACK_ABORT	5
#define SHARE_DISCARD		6
/* The most dirty ranges asked for at once. */
#define DIRTY_RANGES		32
/* The pages read together when a page to lend is not cached, from it on,
 * which the host lends after it, as faults that come in order want. */
#define LEND_WINDOW		16

/* A shared file. */
struct mikros_share {
	struct address_space *mapping;
	/* The host's name of the paged memory object. */
	u64 cookie;
	/* The lent pages, by index: their folios, each holding a reference
	 * for each lent page of it. */
	struct xarray lent;
	struct mutex lock;
	/* Whether a writeback window is open, and how many lends are under
	 * way, which a window waits for. */
	bool window;
	unsigned int lending;
	wait_queue_head_t wait;
};

/* The shares, by mapping, and by the frame number of each folio that
 * holds a lent page; and the address space operations of btrfs, which
 * shared files get a copy of. */
static DEFINE_XARRAY(shares);
static DEFINE_XARRAY(lent_folios);
static const struct address_space_operations *btrfs_ops;
static struct address_space_operations shared_ops;
/* Held while shares are added or removed, and while reclaim walks them. */
static DEFINE_MUTEX(ops_lock);
/* The lent pages whose folios the shares hold, for reclaim. */
static atomic_long_t lent_pages = ATOMIC_LONG_INIT(0);

void mikros_btrfs_lent(struct mikros_node *node);

static unsigned long mapping_key(struct address_space *mapping)
{
	return (unsigned long)mapping / sizeof(*mapping);
}

static struct mikros_share *share_of(struct address_space *mapping)
{
	return xa_load(&shares, mapping_key(mapping));
}

/* Runs op on `count` pages of the share's object from `first`, retrying
 * while a writeback of them is outstanding. */
static int share_op(struct mikros_share *share, u32 op, u64 first, u64 count)
{
	int ret;

	if (!count)
		return 0;
	while ((ret = mikros_host_share_op(share->cookie, op, first, count)) == -EBUSY)
		schedule_timeout_uninterruptible(1);
	return ret;
}

/* Marks the folios of the object's dirty pages from `first` dirty, so that
 * btrfs writes them back. */
static void harvest(struct mikros_share *share, u64 first, u64 count)
{
	u64 ranges[2 * DIRTY_RANGES];
	u64 end = first + count;

	while (first < end) {
		long n = mikros_host_share_dirty(share->cookie, first, end - first, ranges,
						 DIRTY_RANGES);
		long i;

		if (n <= 0)
			return;
		for (i = 0; i < n; i++) {
			u64 index;

			for (index = ranges[2 * i]; index < ranges[2 * i] + ranges[2 * i + 1];
			     index++) {
				struct folio *folio = filemap_get_folio(share->mapping, index);

				if (IS_ERR(folio))
					continue;
				folio_mark_dirty(folio);
				folio_put(folio);
			}
		}
		if (n < DIRTY_RANGES)
			return;
		first = ranges[2 * (n - 1)] + ranges[2 * (n - 1) + 1];
	}
}

static void shared_free_folio(struct folio *folio)
{
	struct mikros_share *share = xa_erase(&lent_folios, folio_pfn(folio));
	long i;

	if (btrfs_ops->free_folio)
		btrfs_ops->free_folio(folio);
	if (!share)
		return;
	for (i = 0; i < folio_nr_pages(folio); i++) {
		u64 index = folio->index + i;

		if (xa_load(&share->lent, index) != folio)
			continue;
		share_op(share, SHARE_DISCARD, index, 1);
		/* Reclaim may have let go of the page meanwhile. */
		if (xa_cmpxchg(&share->lent, index, folio, NULL, 0) != folio)
			continue;
		atomic_long_dec(&lent_pages);
		folio_put(folio);
	}
}

static int shared_writepages(struct address_space *mapping, struct writeback_control *wbc)
{
	struct mikros_share *share = share_of(mapping);
	long budget = wbc->nr_to_write;
	u64 count;
	int ret, waited, begun;

	if (!share)
		return btrfs_ops->writepages(mapping, wbc);
	mutex_lock(&share->lock);
	share->window = true;
	mutex_unlock(&share->lock);
	wait_event(share->wait, READ_ONCE(share->lending) == 0);
	count = DIV_ROUND_UP(i_size_read(mapping->host), PAGE_SIZE);
	share_op(share, SHARE_EVICT, 0, count);
	begun = share_op(share, SHARE_WRITEBACK_BEGIN, 0, count);
	harvest(share, 0, count);
	/* Every dirty folio goes, so that no page stays recorded unwritten. */
	wbc->nr_to_write = LONG_MAX;
	ret = btrfs_ops->writepages(mapping, wbc);
	wbc->nr_to_write = budget - (LONG_MAX - wbc->nr_to_write);
	waited = filemap_fdatawait_range(mapping, 0, LLONG_MAX);
	if (!begun)
		share_op(share, ret || waited ? SHARE_WRITEBACK_ABORT : SHARE_WRITEBACK_END, 0,
			 count);
	mutex_lock(&share->lock);
	share->window = false;
	mutex_unlock(&share->lock);
	wake_up_all(&share->wait);
	return ret;
}

/* Shares the file of `node` as the host's object `cookie`: 0, or a
 * negative errno, -EINVAL for a node that is not a file. */
int mikros_btrfs_share(struct mikros_node *node, u64 cookie)
{
	bool adopted = mikros_task_adopt("share");
	struct inode *inode = d_inode(node->path.dentry);
	struct address_space *mapping = inode->i_mapping;
	struct mikros_share *share;
	int ret = -EINVAL;

	if (!S_ISREG(inode->i_mode))
		goto out;
	mutex_lock(&ops_lock);
	if (!btrfs_ops) {
		btrfs_ops = mapping->a_ops;
		shared_ops = *btrfs_ops;
		shared_ops.free_folio = shared_free_folio;
		shared_ops.writepages = shared_writepages;
	}
	ret = -EEXIST;
	if (share_of(mapping) || mapping->a_ops != btrfs_ops)
		goto unlock;
	ret = -ENOMEM;
	share = kzalloc(sizeof(*share), GFP_KERNEL);
	if (!share)
		goto unlock;
	share->mapping = mapping;
	share->cookie = cookie;
	xa_init(&share->lent);
	mutex_init(&share->lock);
	init_waitqueue_head(&share->wait);
	ret = xa_err(xa_store(&shares, mapping_key(mapping), share, GFP_KERNEL));
	if (ret) {
		kfree(share);
		goto unlock;
	}
	/* The share stays as long as the inode, which it keeps. */
	ihold(inode);
	WRITE_ONCE(mapping->a_ops, &shared_ops);
unlock:
	mutex_unlock(&ops_lock);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Gets page `index` of the shared file of `node` ready to lend: reads it,
 * holds its folio and stores the page's address in *page. The host then
 * lends the page and calls mikros_btrfs_lent(). 0, -ENXIO past the end of
 * the file, or another negative errno. */
int mikros_btrfs_lend_page(struct mikros_node *node, u64 index, void **page)
{
	bool adopted = mikros_task_adopt("lend_page");
	struct address_space *mapping = d_inode(node->path.dentry)->i_mapping;
	struct mikros_share *share = share_of(mapping);
	struct folio *folio, *old;
	int ret = -EINVAL;

	*page = NULL;
	if (!share)
		goto out;
	mutex_lock(&share->lock);
	while (share->window) {
		mutex_unlock(&share->lock);
		wait_event(share->wait, !READ_ONCE(share->window));
		mutex_lock(&share->lock);
	}
	share->lending++;
	mutex_unlock(&share->lock);
	ret = -ENXIO;
	if (index >= DIV_ROUND_UP(i_size_read(mapping->host), PAGE_SIZE))
		goto done;
	folio = filemap_get_folio(mapping, index);
	if (IS_ERR(folio)) {
		struct file_ra_state ra = { .ra_pages = LEND_WINDOW };
		DEFINE_READAHEAD(ractl, NULL, &ra, mapping, index);

		force_page_cache_ra(&ractl, LEND_WINDOW);
	} else {
		folio_put(folio);
	}
	folio = read_mapping_folio(mapping, index, NULL);
	ret = PTR_ERR_OR_ZERO(folio);
	if (ret)
		goto done;
	old = xa_load(&share->lent, index);
	if (old == folio) {
		/* Lent before and evicted since: it holds a reference already. */
		folio_put(folio);
	} else {
		ret = xa_err(xa_store(&lent_folios, folio_pfn(folio), share, GFP_KERNEL));
		if (!ret)
			ret = xa_err(xa_store(&share->lent, index, folio, GFP_KERNEL));
		if (ret) {
			folio_put(folio);
			goto done;
		}
		atomic_long_inc(&lent_pages);
		/* A folio that left the page cache since it was lent took its
		 * reference with it in shared_free_folio(). */
		WARN_ON_ONCE(old);
	}
	*page = page_address(folio_page(folio, index - folio->index));
	ret = 0;
	goto out;
done:
	mikros_btrfs_lent(node);
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Ends a lend that mikros_btrfs_lend_page() got ready, after the host has
 * lent the page or given up. */
void mikros_btrfs_lent(struct mikros_node *node)
{
	bool adopted = mikros_task_adopt("lent");
	struct mikros_share *share = share_of(d_inode(node->path.dentry)->i_mapping);

	if (share) {
		mutex_lock(&share->lock);
		share->lending--;
		mutex_unlock(&share->lock);
		wake_up_all(&share->wait);
	}
	if (adopted)
		mikros_task_exit();
}

/* Whether the file of `node` can never change: it is on a read-only mount
 * or in a read-only subvolume, which writes fail on with -EROFS. */
bool mikros_btrfs_unchangeable(struct mikros_node *node)
{
	bool adopted = mikros_task_adopt("unchangeable");
	struct inode *inode = d_inode(node->path.dentry);
	bool ret = __mnt_is_readonly(node->path.mnt) ||
		   inode_permission(&nop_mnt_idmap, inode, MAY_WRITE) == -EROFS;

	if (adopted)
		mikros_task_exit();
	return ret;
}

/* Marks the folios of the dirty pages of the shared file of `node` dirty,
 * so that btrfs writes the writes of clients back. */
void mikros_btrfs_harvest(struct mikros_node *node)
{
	bool adopted = mikros_task_adopt("harvest");
	struct address_space *mapping = d_inode(node->path.dentry)->i_mapping;
	struct mikros_share *share = share_of(mapping);

	if (share)
		harvest(share, 0, DIV_ROUND_UP(i_size_read(mapping->host), PAGE_SIZE));
	if (adopted)
		mikros_task_exit();
}

/* Stops sharing the file of `node`, once nothing of its object is lent
 * any more nor lends under way: drops the folio references of the pages
 * lent, gives the inode back btrfs's operations and lets it go. */
void mikros_btrfs_unshare(struct mikros_node *node)
{
	bool adopted = mikros_task_adopt("unshare");
	struct address_space *mapping = d_inode(node->path.dentry)->i_mapping;
	struct mikros_share *share;
	struct folio *folio;
	unsigned long index;

	mutex_lock(&ops_lock);
	share = xa_erase(&shares, mapping_key(mapping));
	if (!share)
		goto unlock;
	WRITE_ONCE(mapping->a_ops, btrfs_ops);
	xa_for_each(&share->lent, index, folio) {
		xa_cmpxchg(&lent_folios, folio_pfn(folio), share, NULL, GFP_KERNEL);
		atomic_long_dec(&lent_pages);
		folio_put(folio);
	}
	xa_destroy(&share->lent);
	iput(mapping->host);
	kfree(share);
unlock:
	mutex_unlock(&ops_lock);
	if (adopted)
		mikros_task_exit();
}

/* The dirty ranges of a share's object from page 0 to `end`, fetched as a
 * walk over the pages in order needs them. */
struct dirty_walk {
	struct mikros_share *share;
	u64 ranges[2 * DIRTY_RANGES];
	long n, at;
	/* Where the next fetch starts. */
	u64 next, end;
};

/* Whether page `index` of the object is dirty, for indices that do not
 * decrease from one call to the next. A failed fetch counts every page
 * after it as dirty. */
static bool walk_dirty(struct dirty_walk *w, u64 index)
{
	for (;;) {
		while (w->at < w->n) {
			u64 start = w->ranges[2 * w->at], len = w->ranges[2 * w->at + 1];

			if (index < start)
				return false;
			if (index < start + len)
				return true;
			w->at++;
		}
		if (w->next >= w->end)
			return false;
		w->n = mikros_host_share_dirty(w->share->cookie, w->next, w->end - w->next,
					       w->ranges, DIRTY_RANGES);
		w->at = 0;
		if (w->n < 0) {
			w->n = 0;
			return true;
		}
		if (w->n < DIRTY_RANGES)
			w->next = w->end;
		else
			w->next = w->ranges[2 * (w->n - 1)] + w->ranges[2 * (w->n - 1) + 1];
	}
}

/* Lets go of the folios of at most `max` lent pages of `share` that its
 * object no longer has, after evicting the object's clean pages: the
 * pages that are not dirty, as the object keeps only those. It does
 * nothing rather than wait for the share's lock, and nothing while lends
 * are under way or a writeback window is open, as a writeback that ends
 * meanwhile makes pages clean that the object still has. The lock keeps
 * new lends and windows out while it works. How many it let go of. */
static unsigned long unpin(struct mikros_share *share, unsigned long max)
{
	struct dirty_walk walk = { .share = share };
	unsigned long index, freed = 0;
	struct folio *folio;

	if (!mutex_trylock(&share->lock))
		return 0;
	walk.end = DIV_ROUND_UP(i_size_read(share->mapping->host), PAGE_SIZE);
	if (share->window || share->lending || xa_empty(&share->lent) || !walk.end)
		goto out;
	if (mikros_host_share_op(share->cookie, SHARE_EVICT, 0, walk.end))
		goto out;
	xa_for_each(&share->lent, index, folio) {
		if (freed >= max)
			break;
		if (walk_dirty(&walk, index))
			continue;
		if (xa_cmpxchg(&share->lent, index, folio, NULL, 0) != folio)
			continue;
		atomic_long_dec(&lent_pages);
		folio_put(folio);
		freed++;
	}
out:
	mutex_unlock(&share->lock);
	return freed;
}

static unsigned long shares_count(struct shrinker *shrinker, struct shrink_control *sc)
{
	long count = atomic_long_read(&lent_pages);

	return count > 0 ? count : SHRINK_EMPTY;
}

static unsigned long shares_scan(struct shrinker *shrinker, struct shrink_control *sc)
{
	struct mikros_share *share;
	unsigned long key, freed = 0;

	if (!mutex_trylock(&ops_lock))
		return SHRINK_STOP;
	xa_for_each(&shares, key, share) {
		if (freed >= sc->nr_to_scan)
			break;
		freed += unpin(share, sc->nr_to_scan - freed);
	}
	mutex_unlock(&ops_lock);
	return freed ? freed : SHRINK_STOP;
}

static int __init share_init(void)
{
	struct shrinker *shrinker = shrinker_alloc(0, "mikros-shares");

	if (!shrinker)
		return -ENOMEM;
	shrinker->count_objects = shares_count;
	shrinker->scan_objects = shares_scan;
	shrinker_register(shrinker);
	return 0;
}
device_initcall(share_init);

/* Lets go of the folios of at most `max` lent pages of the shared file of
 * `node` that its object no longer has, as reclaim does: how many. */
unsigned long mikros_btrfs_unpin(struct mikros_node *node, unsigned long max)
{
	bool adopted = mikros_task_adopt("unpin");
	struct mikros_share *share;
	unsigned long freed = 0;

	mutex_lock(&ops_lock);
	share = share_of(d_inode(node->path.dentry)->i_mapping);
	if (share)
		freed = unpin(share, max);
	mutex_unlock(&ops_lock);
	if (adopted)
		mikros_task_exit();
	return freed;
}
