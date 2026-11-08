// SPDX-License-Identifier: GPL-2.0
/*
 * Memory: an arena of address space that the host fills on first touch,
 * with a struct page for each of its pages, as in a kernel without an MMU,
 * and a buddy allocator of its pages.
 */
#include <linux/mm.h>
#include <linux/gfp.h>
#include <linux/slab.h>
#include <linux/vmalloc.h>
#include <linux/highmem.h>
#include <linux/string.h>
#include <linux/uaccess.h>
#include <linux/shrinker.h>
#include <linux/writeback.h>
#include "mm/internal.h"
#include "shim.h"

/* The arena: 64 GiB of address space. */
#define ARENA_SIZE	(64UL << 30)
#define ARENA_PAGES	(ARENA_SIZE >> PAGE_SHIFT)

unsigned long mikros_memory_start, mikros_memory_end;
struct page *mem_map;
unsigned long max_mapnr;

/* The free blocks of each order, and the end of the part of the arena
 * that the allocator has taken blocks from. */
static struct list_head free_area[MAX_PAGE_ORDER + 1];
static unsigned long high_water;
static DEFINE_SPINLOCK(zone_lock);

void mikros_mm_init(void)
{
	int order;

	mikros_memory_start = (unsigned long)mikros_host_reserve(ARENA_SIZE);
	mem_map = mikros_host_reserve(ARENA_PAGES * sizeof(struct page));
	if (!mikros_memory_start || !mem_map)
		panic("no address space for the memory arena");
	mikros_memory_end = mikros_memory_start + ARENA_SIZE;
	max_mapnr = ARENA_PAGES;
	high_water = mikros_memory_start;
	for (order = 0; order <= MAX_PAGE_ORDER; order++)
		INIT_LIST_HEAD(&free_area[order]);
}

/* Readies the struct pages of a block for use. */
static void reset_pages(struct page *page, unsigned int count)
{
	unsigned int i;

	for (i = 0; i < count; i++) {
		page[i].flags.f = 0;
		page[i].mapping = NULL;
		page[i].private = 0;
		page[i].page_type = UINT_MAX;
		page[i].compound_head = 0;
		set_page_count(&page[i], 0);
	}
}

/* A block larger than the buddy allocator's largest: blocks of the
 * largest order that follow each other, from the untouched arena. */
static struct page *take_big(unsigned int order)
{
	unsigned long size = PAGE_SIZE << order;
	struct page *page;

	if (high_water + size > mikros_memory_end)
		return NULL;
	page = virt_to_page(high_water);
	high_water += size;
	reset_pages(page, 1U << order);
	return page;
}

static struct page *take(unsigned int order)
{
	unsigned int o;
	struct page *page;

	for (o = order; o <= MAX_PAGE_ORDER; o++)
		if (!list_empty(&free_area[o]))
			break;
	if (o > MAX_PAGE_ORDER) {
		/* A block of the largest order from the untouched arena. */
		unsigned long size = PAGE_SIZE << MAX_PAGE_ORDER;

		if (high_water + size > mikros_memory_end)
			return NULL;
		page = virt_to_page(high_water);
		high_water += size;
		reset_pages(page, 1U << MAX_PAGE_ORDER);
		o = MAX_PAGE_ORDER;
	} else {
		page = list_first_entry(&free_area[o], struct page, buddy_list);
		list_del(&page->buddy_list);
		__ClearPageBuddy(page);
		set_page_private(page, 0);
	}
	/* The halves that are left over go to the lists below. */
	while (o > order) {
		struct page *half;

		o--;
		half = page + (1U << o);
		set_page_private(half, o);
		__SetPageBuddy(half);
		list_add(&half->buddy_list, &free_area[o]);
	}
	return page;
}

static void give_back(struct page *page, unsigned int order)
{
	unsigned long pfn = page_to_pfn(page);

	while (order < MAX_PAGE_ORDER) {
		unsigned long buddy_pfn = pfn ^ (1UL << order);
		struct page *buddy = pfn_to_page(buddy_pfn);

		if (buddy_pfn - ARCH_PFN_OFFSET >= max_mapnr ||
		    virt_to_page(high_water) <= buddy ||
		    !PageBuddy(buddy) || page_private(buddy) != order)
			break;
		list_del(&buddy->buddy_list);
		__ClearPageBuddy(buddy);
		set_page_private(buddy, 0);
		pfn &= ~(1UL << order);
		order++;
	}
	page = pfn_to_page(pfn);
	set_page_private(page, order);
	__SetPageBuddy(page);
	list_add(&page->buddy_list, &free_area[order]);
}

void prep_compound_page(struct page *page, unsigned int order)
{
	int i;

	__SetPageHead(page);
	for (i = 1; i < (1 << order); i++)
		prep_compound_tail(page, i);
	prep_compound_head(page, order);
}

static struct page *alloc_block(gfp_t gfp, unsigned int order)
{
	struct page *page;

	spin_lock(&zone_lock);
	page = order > MAX_PAGE_ORDER ? take_big(order) : take(order);
	spin_unlock(&zone_lock);
	if (!page) {
		if (!(gfp & __GFP_NOWARN))
			mikros_log("out of memory for an order-%u allocation", order);
		return NULL;
	}
	mikros_node_account(-(1L << order));
	set_page_count(page, 1);
	if (order && (gfp & __GFP_COMP))
		prep_compound_page(page, order);
	if (gfp & __GFP_ZERO)
		memset(page_address(page), 0, PAGE_SIZE << order);
	return page;
}

static void free_block(struct page *page, unsigned int order)
{
	unsigned int i;

	reset_pages(page, 1U << order);
	mikros_node_account(1L << order);
	spin_lock(&zone_lock);
	if (order <= MAX_PAGE_ORDER)
		give_back(page, order);
	else
		for (i = 0; i < 1U << order; i += 1U << MAX_PAGE_ORDER)
			give_back(page + i, MAX_PAGE_ORDER);
	spin_unlock(&zone_lock);
}

struct page *__alloc_pages_noprof(gfp_t gfp, unsigned int order, int preferred_nid,
				  nodemask_t *nodemask)
{
	return alloc_block(gfp, order);
}

struct folio *__folio_alloc_noprof(gfp_t gfp, unsigned int order, int preferred_nid,
				   nodemask_t *nodemask)
{
	struct page *page = alloc_block(gfp | __GFP_COMP, order);

	return page ? page_rmappable_folio(page) : NULL;
}

unsigned long alloc_pages_bulk_noprof(gfp_t gfp, int preferred_nid, nodemask_t *nodemask,
				      int nr_pages, struct page **page_array)
{
	int i, got = 0;

	for (i = 0; i < nr_pages; i++) {
		if (page_array[i]) {
			got++;
			continue;
		}
		page_array[i] = alloc_block(gfp, 0);
		if (!page_array[i])
			break;
		got++;
	}
	return got;
}

/* The release's mm/swap.c frees folios whose last reference went with
 * these. */
void free_frozen_pages(struct page *page, unsigned int order)
{
	free_block(page, order);
}

void free_unref_folios(struct folio_batch *folios)
{
	unsigned int i;

	for (i = 0; i < folio_batch_count(folios); i++)
		free_block(&folios->folios[i]->page, folio_order(folios->folios[i]));
	folio_batch_reinit(folios);
}

unsigned long get_free_pages_noprof(gfp_t gfp, unsigned int order)
{
	struct page *page = alloc_block(gfp & ~__GFP_HIGHMEM, order);

	return page ? (unsigned long)page_address(page) : 0;
}

void __free_pages(struct page *page, unsigned int order)
{
	if (put_page_testzero(page))
		free_block(page, order);
}

void free_pages(unsigned long addr, unsigned int order)
{
	if (addr)
		__free_pages(virt_to_page((void *)addr), order);
}

unsigned long get_zeroed_page_noprof(gfp_t gfp)
{
	return get_free_pages_noprof(gfp | __GFP_ZERO, 0);
}

/* Makes each page of a block that is not compound a block of its own. */
void split_page(struct page *page, unsigned int order)
{
	unsigned int i;

	VM_BUG_ON_PAGE(PageCompound(page), page);
	for (i = 1; i < 1U << order; i++)
		set_page_count(page + i, 1);
}

/* Whether a page is in a free block of the buddy allocator. */
bool is_free_buddy_page(const struct page *page)
{
	unsigned long pfn = page_to_pfn(page);
	unsigned int order;
	bool free = false;

	spin_lock(&zone_lock);
	for (order = 0; order <= MAX_PAGE_ORDER; order++) {
		const struct page *head = pfn_to_page(pfn & ~((1UL << order) - 1));

		if (PageBuddy(head) && page_private(head) >= order) {
			free = true;
			break;
		}
	}
	spin_unlock(&zone_lock);
	return free;
}
