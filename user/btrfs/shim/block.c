// SPDX-License-Identifier: GPL-2.0
/*
 * The block layer's core, for disks whose drivers take bios, which the
 * shim's are; and the shim's disks, which the host gives: each is a disk
 * of the release's block/bdev.c with a node in /dev, whose bios workers
 * of the shim carry out with the host's I/O calls, as a driver's
 * interrupts would complete them.
 */
#include <linux/blkdev.h>
#include <linux/bio.h>
#include <linux/backing-dev.h>
#include <linux/init_syscalls.h>
#include <linux/kdev_t.h>
#include <linux/pagemap.h>
#include <linux/slab.h>
#include <linux/workqueue.h>
#include "block/blk.h"
#include "shim.h"

/* The major number of the shim's disks, one that Linux leaves for local
 * use. */
#define DISK_MAJOR		240
#define DISK_MAX_SECTORS	2048

static const struct {
	int errno;
	const char *name;
} blk_errors[] = {
	[BLK_STS_OK]		= { 0,		"" },
	[BLK_STS_NOTSUPP]	= { -EOPNOTSUPP, "operation not supported" },
	[BLK_STS_TIMEOUT]	= { -ETIMEDOUT,	"timeout" },
	[BLK_STS_NOSPC]		= { -ENOSPC,	"critical space allocation" },
	[BLK_STS_TRANSPORT]	= { -ENOLINK,	"recoverable transport" },
	[BLK_STS_TARGET]	= { -EREMOTEIO,	"critical target" },
	[BLK_STS_RESV_CONFLICT]	= { -EBADE,	"reservation conflict" },
	[BLK_STS_MEDIUM]	= { -ENODATA,	"critical medium" },
	[BLK_STS_PROTECTION]	= { -EILSEQ,	"protection" },
	[BLK_STS_RESOURCE]	= { -ENOMEM,	"kernel resource" },
	[BLK_STS_DEV_RESOURCE]	= { -EBUSY,	"device resource" },
	[BLK_STS_AGAIN]		= { -EAGAIN,	"nonblocking retry" },
	[BLK_STS_OFFLINE]	= { -ENODEV,	"device offline" },
	[BLK_STS_DM_REQUEUE]	= { -EREMCHG,	"dm internal retry" },
	[BLK_STS_ZONE_OPEN_RESOURCE]	= { -ETOOMANYREFS, "open zones exceeded" },
	[BLK_STS_ZONE_ACTIVE_RESOURCE]	= { -EOVERFLOW,	"active zones exceeded" },
	[BLK_STS_DURATION_LIMIT]	= { -ETIME,	"duration limit exceeded" },
	[BLK_STS_INVAL]		= { -EINVAL,	"invalid" },
	[BLK_STS_IOERR]		= { -EIO,	"I/O" },
};

blk_status_t errno_to_blk_status(int errno)
{
	int i;

	for (i = 0; i < ARRAY_SIZE(blk_errors); i++)
		if (blk_errors[i].errno == errno)
			return (__force blk_status_t)i;
	return BLK_STS_IOERR;
}

int blk_status_to_errno(blk_status_t status)
{
	int idx = (__force int)status;

	if (WARN_ON_ONCE(idx >= ARRAY_SIZE(blk_errors)))
		return -EIO;
	return blk_errors[idx].errno;
}

/* Plugs hold no requests, as there are no request queues, but they run
 * the callbacks that were added to them when they are finished. */
void blk_start_plug(struct blk_plug *plug)
{
	if (current->plug)
		return;
	memset(plug, 0, sizeof(*plug));
	INIT_LIST_HEAD(&plug->cb_list);
	current->plug = plug;
}

void __blk_flush_plug(struct blk_plug *plug, bool from_schedule)
{
	LIST_HEAD(callbacks);

	while (!list_empty(&plug->cb_list)) {
		list_splice_init(&plug->cb_list, &callbacks);
		while (!list_empty(&callbacks)) {
			struct blk_plug_cb *cb = list_first_entry(&callbacks, struct blk_plug_cb, list);

			list_del(&cb->list);
			cb->callback(cb, from_schedule);
		}
	}
}

void blk_finish_plug(struct blk_plug *plug)
{
	if (plug == current->plug) {
		__blk_flush_plug(plug, false);
		current->plug = NULL;
	}
}

struct blk_plug_cb *blk_check_plugged(blk_plug_cb_fn unplug, void *data, int size)
{
	struct blk_plug *plug = current->plug;
	struct blk_plug_cb *cb;

	if (!plug)
		return NULL;
	list_for_each_entry(cb, &plug->cb_list, list)
		if (cb->callback == unplug && cb->data == data)
			return cb;
	BUG_ON(size < sizeof(*cb));
	cb = kzalloc(size, GFP_ATOMIC);
	if (cb) {
		cb->data = data;
		cb->callback = unplug;
		list_add(&cb->list, &plug->cb_list);
	}
	return cb;
}

void blk_io_schedule(void)
{
	io_schedule();
}

int iocb_bio_iopoll(struct kiocb *kiocb, struct io_comp_batch *iob, unsigned int flags)
{
	return 0;
}

static void end_bio(struct bio *bio, blk_status_t status)
{
	bio->bi_status = status;
	bio_endio(bio);
}

/* Checks a bio as the release's block/blk-core.c does, and gives it to its
 * disk's driver. */
void submit_bio_noacct(struct bio *bio)
{
	struct block_device *bdev = bio->bi_bdev;
	sector_t max = bdev_nr_sectors(bdev);
	unsigned int sectors = bio_sectors(bio);

	might_sleep();
	if ((bio->bi_opf & REQ_NOWAIT) && !bdev_nowait(bdev))
		return end_bio(bio, BLK_STS_NOTSUPP);
	if (sectors && (sectors > max || bio->bi_iter.bi_sector > max - sectors)) {
		pr_info_ratelimited("%pg: attempt to access beyond end of device\n", bdev);
		return end_bio(bio, BLK_STS_IOERR);
	}
	if (op_is_flush(bio->bi_opf) && !bdev_write_cache(bdev)) {
		bio->bi_opf &= ~(REQ_PREFLUSH | REQ_FUA);
		if (!sectors)
			return end_bio(bio, BLK_STS_OK);
	}
	switch (bio_op(bio)) {
	case REQ_OP_READ:
	case REQ_OP_WRITE:
		break;
	case REQ_OP_DISCARD:
		if (!bdev_max_discard_sectors(bdev))
			return end_bio(bio, BLK_STS_NOTSUPP);
		break;
	case REQ_OP_WRITE_ZEROES:
		if (!bdev_get_queue(bdev)->limits.max_write_zeroes_sectors)
			return end_bio(bio, BLK_STS_NOTSUPP);
		break;
	default:
		return end_bio(bio, BLK_STS_NOTSUPP);
	}
	bdev->bd_disk->fops->submit_bio(bio);
}

void submit_bio(struct bio *bio)
{
	submit_bio_noacct(bio);
}

int blkdev_issue_flush(struct block_device *bdev)
{
	struct bio bio;

	bio_init(&bio, bdev, NULL, 0, REQ_OP_WRITE | REQ_PREFLUSH);
	return submit_bio_wait(&bio);
}

/* The shim's disks can neither discard nor write zeroes. */
int blkdev_issue_discard(struct block_device *bdev, sector_t sector, sector_t nr_sects,
			 gfp_t gfp_mask)
{
	return -EOPNOTSUPP;
}

int blkdev_issue_zeroout(struct block_device *bdev, sector_t sector, sector_t nr_sects,
			 gfp_t gfp_mask, unsigned flags)
{
	return -EOPNOTSUPP;
}

/* Disks have no events, nor ioctls of their own. */
void disk_block_events(struct gendisk *disk)
{
}

void disk_unblock_events(struct gendisk *disk)
{
}

void disk_flush_events(struct gendisk *disk, unsigned int mask)
{
}

long blkdev_ioctl(struct file *file, unsigned cmd, unsigned long arg)
{
	return -ENOTTY;
}

int blkdev_uring_cmd(struct io_uring_cmd *cmd, unsigned int issue_flags)
{
	return -EOPNOTSUPP;
}

/* The shim's disks. */

static struct workqueue_struct *disk_wq;
static atomic_t next_minor = ATOMIC_INIT(0);

struct disk_io {
	struct work_struct work;
	struct bio *bio;
};

/* Reads or writes `len` bytes at `buf`, at `offset` of the disk. */
static int transfer(void *disk, bool write, u64 offset, void *buf, unsigned long len)
{
	if (write)
		return mikros_host_disk_write(disk, offset, buf, len);
	return mikros_host_disk_read(disk, offset, buf, len);
}

static void disk_io(struct work_struct *work)
{
	struct disk_io *io = container_of(work, struct disk_io, work);
	struct bio *bio = io->bio;
	void *disk = bio->bi_bdev->bd_disk->private_data;
	bool write = op_is_write(bio_op(bio));
	u64 offset = (u64)bio->bi_iter.bi_sector << SECTOR_SHIFT;
	struct bvec_iter iter;
	struct bio_vec bv;
	char *run = NULL;
	unsigned long len = 0;
	int ret = 0;

	kfree(io);
	if (bio->bi_opf & REQ_PREFLUSH)
		ret = mikros_host_disk_flush(disk);
	if (!ret && (bio_op(bio) == REQ_OP_READ || bio_op(bio) == REQ_OP_WRITE)) {
		/* Segments that follow each other in memory go together. */
		bio_for_each_bvec(bv, bio, iter) {
			char *p = bvec_virt(&bv);

			if (run && run + len == p) {
				len += bv.bv_len;
				continue;
			}
			if (run) {
				ret = transfer(disk, write, offset, run, len);
				if (ret)
					break;
				offset += len;
			}
			run = p;
			len = bv.bv_len;
		}
		if (!ret && run)
			ret = transfer(disk, write, offset, run, len);
	} else if (!ret) {
		ret = -EOPNOTSUPP;
	}
	if (!ret && (bio->bi_opf & REQ_FUA))
		ret = mikros_host_disk_flush(disk);
	end_bio(bio, errno_to_blk_status(ret));
}

static void disk_submit_bio(struct bio *bio)
{
	struct disk_io *io = kmalloc(sizeof(*io), GFP_NOIO | __GFP_NOFAIL);

	INIT_WORK(&io->work, disk_io);
	io->bio = bio;
	queue_work(disk_wq, &io->work);
}

static const struct block_device_operations disk_fops = {
	.submit_bio = disk_submit_bio,
};

/* The shim's disks stay until the program ends. */
static void disk_release(struct device *dev)
{
}

static void set_limits(struct queue_limits *lim, unsigned int block_size)
{
	lim->features = BLK_FEAT_WRITE_CACHE | BLK_FEAT_FUA;
	lim->seg_boundary_mask = BLK_SEG_BOUNDARY_MASK;
	lim->max_hw_sectors = DISK_MAX_SECTORS;
	lim->max_dev_sectors = DISK_MAX_SECTORS;
	lim->max_sectors = DISK_MAX_SECTORS;
	lim->max_user_sectors = DISK_MAX_SECTORS;
	lim->max_segment_size = UINT_MAX;
	lim->max_segments = BIO_MAX_VECS;
	lim->logical_block_size = block_size;
	lim->physical_block_size = block_size;
	lim->io_min = block_size;
	lim->dma_alignment = SECTOR_SIZE - 1;
}

/* Makes the disk, as the release's block/genhd.c makes one for a driver. */
static struct gendisk *alloc_disk(const char *name, void *handle, unsigned int block_size)
{
	struct request_queue *q = kzalloc(sizeof(*q), GFP_KERNEL);
	struct gendisk *disk = kzalloc(sizeof(*disk), GFP_KERNEL);

	if (!q || !disk)
		goto free;
	set_limits(&q->limits, block_size);
	mutex_init(&q->limits_lock);
	q->disk = disk;
	disk->fops = &disk_fops;
	disk->queue = q;
	disk->private_data = handle;
	strscpy(disk->disk_name, name);
	mutex_init(&disk->open_mutex);
	xa_init(&disk->part_tbl);
	disk->bdi = bdi_alloc(NUMA_NO_NODE);
	if (!disk->bdi)
		goto free;
	disk->bdi->ra_pages = VM_READAHEAD_PAGES;
	disk->bdi->io_pages = DISK_MAX_SECTORS >> (PAGE_SHIFT - SECTOR_SHIFT);
	disk->part0 = bdev_alloc(disk, 0);
	if (!disk->part0)
		goto put_bdi;
	if (xa_insert(&disk->part_tbl, 0, disk->part0, GFP_KERNEL))
		goto drop_part0;
	bdev_set_flag(disk->part0, BD_HAS_SUBMIT_BIO);
	disk_to_dev(disk)->release = disk_release;
	device_initialize(disk_to_dev(disk));
	if (dev_set_name(disk_to_dev(disk), "%s", name))
		goto drop_part0;
	return disk;

drop_part0:
	xa_destroy(&disk->part_tbl);
	disk->part0->bd_disk = NULL;
	bdev_drop(disk->part0);
put_bdi:
	bdi_put(disk->bdi);
free:
	kfree(disk);
	kfree(q);
	return NULL;
}

/* Adds the host's disk `handle` of `size` bytes, as /dev/<name>, with
 * blocks of `block_size` bytes. 0, or a negative errno. */
int mikros_btrfs_add_disk(const char *name, void *handle, u64 size, unsigned int block_size,
			  bool read_only)
{
	char path[sizeof("/dev/") + DISK_NAME_LEN];
	bool adopted = mikros_task_adopt("add_disk");
	struct gendisk *disk;
	dev_t dev;
	int ret;

	ret = -EINVAL;
	if (strlen(name) >= DISK_NAME_LEN || strchr(name, '/') || !is_power_of_2(block_size) ||
	    block_size < SECTOR_SIZE || block_size > PAGE_SIZE)
		goto out;
	ret = -ENOMEM;
	disk = alloc_disk(name, handle, block_size);
	if (!disk)
		goto out;
	dev = MKDEV(DISK_MAJOR, atomic_inc_return(&next_minor) - 1);
	disk->major = MAJOR(dev);
	disk->first_minor = MINOR(dev);
	disk->minors = 1;
	disk_to_dev(disk)->devt = dev;
	if (read_only)
		set_bit(GD_READ_ONLY, &disk->state);
	bdev_set_nr_sectors(disk->part0, size >> SECTOR_SHIFT);
	/* From here on the disk stays, even if it gets no node. */
	ret = bdi_register(disk->bdi, "%u:%u", MAJOR(dev), MINOR(dev));
	if (ret)
		goto out;
	bdev_add(disk->part0, dev);
	set_bit(GD_ADDED, &disk->state);
	snprintf(path, sizeof(path), "/dev/%s", name);
	ret = init_mknod(path, S_IFBLK | 0600, new_encode_dev(dev));
out:
	if (adopted)
		mikros_task_exit();
	return ret;
}

void mikros_block_init(void)
{
	disk_wq = alloc_workqueue("disk", WQ_UNBOUND | WQ_MEM_RECLAIM, 0);
	if (!disk_wq)
		panic("no memory for the disks' workqueue");
}
