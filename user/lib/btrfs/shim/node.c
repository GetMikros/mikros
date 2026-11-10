// SPDX-License-Identifier: GPL-2.0
/*
 * The memory node: the one node and zone that hold the arena, with the
 * counters and LRU lists of Linux's mm/ code, which the page cache keeps
 * up to date; the amount of memory, which the host gives and the page
 * cache's dirty limits, the VFS's hash tables and the watermarks of
 * reclaim are sized by.
 */
#include <linux/mm.h>
#include <linux/mmzone.h>
#include <linux/memblock.h>
#include <linux/math.h>
#include <linux/pagemap.h>
#include <linux/swap.h>
#include <linux/vmstat.h>
#include <linux/sched.h>
#include <linux/wait.h>
#include "mm/internal.h"
#include "shim.h"

struct pglist_data contig_page_data;
atomic_long_t _totalram_pages;
unsigned long totalreserve_pages;
s32 vm_committed_as_batch = 32;
/* Reclaim's view of the page allocator's fragmentation avoidance, which
 * the shim's does not have. */
int defrag_mode;

/* The zone's watermarks, as the release's mm/page_alloc.c sets those of a
 * machine with one zone: the minimum, which allocations that may reclaim
 * do not go below, the square root of 16 times the memory in KiB, in KiB,
 * from 128 KiB to 256 MiB; the low and high marks, between which kswapd
 * keeps the free memory, each a thousandth of the memory or a quarter of
 * the minimum above the last. */
static void set_watermarks(struct zone *zone, unsigned long pages)
{
	unsigned long kbytes = pages << (PAGE_SHIFT - 10);
	unsigned long min = clamp(int_sqrt(kbytes * 16), 128UL, 262144UL) >> (PAGE_SHIFT - 10);
	unsigned long step = max(min >> 2, mult_frac(pages, 10, 10000));

	zone->watermark_boost = 0;
	zone->_watermark[WMARK_MIN] = min;
	zone->_watermark[WMARK_LOW] = min + step;
	zone->_watermark[WMARK_HIGH] = min + 2 * step;
	zone->_watermark[WMARK_PROMO] = min + 3 * step;
}

/* The node's one zone, which the zonelists of reclaim list. */
static void set_zonelists(struct pglist_data *pgdat, struct zone *zone)
{
	struct zoneref *refs = pgdat->node_zonelists[ZONELIST_FALLBACK]._zonerefs;

	refs[0].zone = zone;
	refs[0].zone_idx = zone_idx(zone);
	refs[1].zone = NULL;
	refs[1].zone_idx = 0;
}

void mikros_node_init(void)
{
	struct pglist_data *pgdat = NODE_DATA(0);
	struct zone *zone = &pgdat->node_zones[ZONE_NORMAL];
	unsigned long pages = mikros_host_memory() >> PAGE_SHIFT;
	int i;

	atomic_long_set(&_totalram_pages, pages);
	pgdat->node_id = 0;
	pgdat->nr_zones = ZONE_NORMAL + 1;
	pgdat->node_start_pfn = ARCH_PFN_OFFSET;
	pgdat->node_present_pages = pages;
	pgdat->node_spanned_pages = max_mapnr;
	init_waitqueue_head(&pgdat->kswapd_wait);
	init_waitqueue_head(&pgdat->pfmemalloc_wait);
	for (i = 0; i < NR_VMSCAN_THROTTLE; i++)
		init_waitqueue_head(&pgdat->reclaim_wait[i]);
	lruvec_init(&pgdat->__lruvec);
	pgdat->per_cpu_nodestats = alloc_percpu(struct per_cpu_nodestat);

	zone->name = "Normal";
	zone->zone_pgdat = pgdat;
	zone->zone_start_pfn = ARCH_PFN_OFFSET;
	zone->spanned_pages = max_mapnr;
	zone->present_pages = pages;
	atomic_long_set(&zone->managed_pages, pages);
	spin_lock_init(&zone->lock);
	zone_seqlock_init(zone);
	zone->per_cpu_zonestats = alloc_percpu(struct per_cpu_zonestat);
	zone->per_cpu_pageset = alloc_percpu(struct per_cpu_pages);
	if (!pgdat->per_cpu_nodestats || !zone->per_cpu_zonestats || !zone->per_cpu_pageset)
		panic("no memory for the counters of the memory node");
	zone_page_state_add(pages, zone, NR_FREE_PAGES);
	set_watermarks(zone, pages);
	set_zonelists(pgdat, zone);
}

void mikros_node_exact(void)
{
	struct pglist_data *pgdat = NODE_DATA(0);
	struct zone *zone = &pgdat->node_zones[ZONE_NORMAL];
	int cpu;

	for_each_possible_cpu(cpu) {
		per_cpu_ptr(zone->per_cpu_zonestats, cpu)->stat_threshold = 0;
		per_cpu_ptr(pgdat->per_cpu_nodestats, cpu)->stat_threshold = 0;
	}
	zone->percpu_drift_mark = 0;
}

/* The page allocator's share of the counters. */
void mikros_node_account(long pages)
{
	zone_page_state_add(pages, &NODE_DATA(0)->node_zones[ZONE_NORMAL], NR_FREE_PAGES);
}

/* The page allocator keeps no lists of free pages for each CPU. */
void drain_local_pages(struct zone *zone)
{
}

bool decay_pcp_high(struct zone *zone, struct per_cpu_pages *pcp)
{
	return false;
}

/* A hash table of the VFS, sized as the release's mm/mm_init.c sizes them:
 * a bucket for each 2^scale bytes of memory unless the caller says how
 * many, and no more than a sixteenth of memory. */
void *alloc_large_system_hash(const char *tablename, unsigned long bucketsize,
			      unsigned long numentries, int scale, int flags,
			      unsigned int *_hash_shift, unsigned int *_hash_mask,
			      unsigned long low_limit, unsigned long high_limit)
{
	unsigned long long max = high_limit;
	unsigned long log2qty, size;
	void *table;

	if (!numentries) {
		numentries = round_up(totalram_pages(), SZ_1M / PAGE_SIZE);
		if (scale > PAGE_SHIFT)
			numentries >>= scale - PAGE_SHIFT;
		else
			numentries <<= PAGE_SHIFT - scale;
		if (numentries * bucketsize < PAGE_SIZE)
			numentries = PAGE_SIZE / bucketsize;
	}
	numentries = roundup_pow_of_two(numentries);
	if (!max)
		max = div64_ul((unsigned long long)totalram_pages() << PAGE_SHIFT >> 4, bucketsize);
	max = min(max, 0x80000000ULL);
	if (numentries < low_limit)
		numentries = low_limit;
	if (numentries > max)
		numentries = max;
	log2qty = ilog2(numentries);
	size = bucketsize << log2qty;
	table = kvmalloc(size, GFP_KERNEL | (flags & HASH_ZERO ? __GFP_ZERO : 0));
	if (!table)
		panic("Failed to allocate %s hash table\n", tablename);
	if (_hash_shift)
		*_hash_shift = log2qty;
	if (_hash_mask)
		*_hash_mask = (1 << log2qty) - 1;
	return table;
}

/* Whether `free_pages` leave `mark` free pages after an allocation of
 * `order`, as the release's mm/page_alloc.c reckons for order 0. The page
 * allocator takes blocks of any order from the untouched arena when its
 * lists have none, so that the free blocks of each order do not count;
 * nor do the reserves of `alloc_flags`, which the shim's allocations do
 * not draw on. */
bool __zone_watermark_ok(struct zone *z, unsigned int order, unsigned long mark,
			 int highest_zoneidx, unsigned int alloc_flags, long free_pages)
{
	free_pages -= (1L << order) - 1;
	return free_pages > (long)(mark + z->lowmem_reserve[highest_zoneidx]);
}

bool zone_watermark_ok(struct zone *z, unsigned int order, unsigned long mark,
		       int highest_zoneidx, unsigned int alloc_flags)
{
	return __zone_watermark_ok(z, order, mark, highest_zoneidx, alloc_flags,
				   zone_page_state(z, NR_FREE_PAGES));
}

/* The bytes of memory that the page allocator has given out, which may be
 * more than the host gave. */
u64 mikros_btrfs_memory_used(void)
{
	struct zone *zone = &NODE_DATA(0)->node_zones[ZONE_NORMAL];
	long free = atomic_long_read(&zone->vm_stat[NR_FREE_PAGES]);

	return (u64)(totalram_pages() - free) << PAGE_SHIFT;
}
