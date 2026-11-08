// SPDX-License-Identifier: GPL-2.0
/*
 * The memory node: the one node and zone that hold the arena, with the
 * counters and LRU lists of Linux's mm/ code, which the page cache keeps
 * up to date; the amount of memory, which the host gives and the page
 * cache's dirty limits and the VFS's hash tables are sized by; and what
 * that code asks of page reclaim.
 */
#include <linux/mm.h>
#include <linux/mmzone.h>
#include <linux/memblock.h>
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

/* Takes a locked, clean folio that nothing else holds out of the page
 * cache, as the release's mm/vmscan.c does: the pages it freed. */
long remove_mapping(struct address_space *mapping, struct folio *folio)
{
	void (*free_folio)(struct folio *) = mapping->a_ops->free_folio;
	int refcount = 1 + folio_nr_pages(folio);

	BUG_ON(!folio_test_locked(folio));
	BUG_ON(mapping != folio_mapping(folio));
	spin_lock(&mapping->host->i_lock);
	xa_lock_irq(&mapping->i_pages);
	if (!folio_ref_freeze(folio, refcount))
		goto busy;
	if (unlikely(folio_test_dirty(folio))) {
		folio_ref_unfreeze(folio, refcount);
		goto busy;
	}
	__filemap_remove_folio(folio, NULL);
	xa_unlock_irq(&mapping->i_pages);
	if (mapping_shrinkable(mapping))
		inode_add_lru(mapping->host);
	spin_unlock(&mapping->host->i_lock);
	if (free_folio)
		free_folio(folio);
	/* What remains is the page cache's reference, which goes so. */
	folio_ref_unfreeze(folio, 1);
	return folio_nr_pages(folio);

busy:
	xa_unlock_irq(&mapping->i_pages);
	spin_unlock(&mapping->host->i_lock);
	return 0;
}

/* Memory is not reclaimed yet, so a task that waits for reclaim to make
 * progress waits as long as the release's longest wait for writeback. */
void reclaim_throttle(pg_data_t *pgdat, enum vmscan_throttle_state reason)
{
	schedule_timeout_uninterruptible(HZ / 10);
}

/* Nothing counts throttled tasks, so this is never called. */
void __acct_reclaim_writeback(pg_data_t *pgdat, struct folio *folio, int nr_throttled)
{
}
