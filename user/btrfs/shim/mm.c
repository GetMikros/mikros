// SPDX-License-Identifier: GPL-2.0
/*
 * Memory: an arena of address space that the host fills on first touch,
 * with a struct page for each of its pages, as in a kernel without an MMU;
 * a buddy allocator of its pages; slab caches, which kmalloc's sizes are
 * too; and larger allocations, which vmalloc's are, as without an MMU.
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

static void init_kmalloc(void);

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
	init_kmalloc();
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

/* Slab caches. A slab is a folio whose head page keeps the cache in
 * ->private, the free objects in ->mapping and the objects in use in
 * ->index. Objects of a cache with a constructor keep their free pointer
 * after themselves, so that freeing leaves what the constructor did. */

struct kmem_cache {
	const char *name;
	unsigned int size, align, stride, offset, order, per_slab;
	slab_flags_t flags;
	void (*ctor)(void *);
	spinlock_t lock;
	struct list_head partial;
	unsigned int empty;
};

#define SLAB_EMPTY_KEPT	1

kmem_buckets kmalloc_caches[NR_KMALLOC_TYPES];
static struct kmem_cache kmalloc_cache[KMALLOC_SHIFT_HIGH + 1];

static inline struct page *slab_page(const void *object)
{
	return compound_head(virt_to_page(object));
}

static void **free_pointer(struct kmem_cache *s, void *object)
{
	return (void **)((char *)object + s->offset);
}

static void init_cache(struct kmem_cache *s, const char *name, unsigned int size,
		       unsigned int align, slab_flags_t flags, void (*ctor)(void *))
{
	unsigned int slab;

	s->name = name;
	s->size = size;
	s->align = max_t(unsigned int, align, ARCH_KMALLOC_MINALIGN);
	if (is_power_of_2(size))
		s->align = max(s->align, min_t(unsigned int, size, PAGE_SIZE));
	s->flags = flags;
	s->ctor = ctor;
	s->offset = ctor || (flags & SLAB_TYPESAFE_BY_RCU) ? ALIGN(size, sizeof(void *)) : 0;
	s->stride = ALIGN(s->offset ? s->offset + sizeof(void *) : max_t(unsigned int, size, sizeof(void *)),
			  s->align);
	for (s->order = 0; s->order < 3; s->order++)
		if ((PAGE_SIZE << s->order) / s->stride >= 8)
			break;
	slab = PAGE_SIZE << s->order;
	s->per_slab = slab / s->stride;
	spin_lock_init(&s->lock);
	INIT_LIST_HEAD(&s->partial);
}

static struct page *new_slab(struct kmem_cache *s, gfp_t gfp)
{
	struct page *page = alloc_block((gfp & ~__GFP_ZERO) | __GFP_COMP, s->order);
	char *base;
	unsigned int i;
	void *free = NULL;

	if (!page)
		return NULL;
	__folio_set_slab(page_folio(page));
	page->private = (unsigned long)s;
	base = page_address(page);
	for (i = s->per_slab; i-- > 0;) {
		void *object = base + i * s->stride;

		if (s->ctor)
			s->ctor(object);
		*free_pointer(s, object) = free;
		free = object;
	}
	page->mapping = free;
	page->__folio_index = 0;
	return page;
}

void *kmem_cache_alloc_noprof(struct kmem_cache *s, gfp_t gfp)
{
	struct page *page;
	void *object;

	spin_lock(&s->lock);
	if (list_empty(&s->partial)) {
		spin_unlock(&s->lock);
		page = new_slab(s, gfp);
		if (!page)
			return NULL;
		spin_lock(&s->lock);
		list_add(&page->lru, &s->partial);
	}
	page = list_first_entry(&s->partial, struct page, lru);
	object = page->mapping;
	page->mapping = *free_pointer(s, object);
	if (!page->__folio_index++ && s->empty)
		s->empty--;
	if (!page->mapping)
		list_del_init(&page->lru);
	spin_unlock(&s->lock);
	if (gfp & __GFP_ZERO)
		memset(object, 0, s->size);
	return object;
}

void *kmem_cache_alloc_lru_noprof(struct kmem_cache *s, struct list_lru *lru, gfp_t gfp)
{
	return kmem_cache_alloc_noprof(s, gfp);
}

void *kmem_cache_alloc_node_noprof(struct kmem_cache *s, gfp_t gfp, int node)
{
	return kmem_cache_alloc_noprof(s, gfp);
}

static void cache_free(struct kmem_cache *s, struct page *page, void *object)
{
	bool release = false;

	spin_lock(&s->lock);
	if (!page->mapping)
		list_add(&page->lru, &s->partial);
	*free_pointer(s, object) = page->mapping;
	page->mapping = object;
	if (!--page->__folio_index) {
		/* An empty slab goes, but for a few that are kept, and but
		 * for those whose objects stay valid across RCU readers. */
		if (s->empty < SLAB_EMPTY_KEPT || (s->flags & SLAB_TYPESAFE_BY_RCU)) {
			s->empty++;
		} else {
			list_del(&page->lru);
			release = true;
		}
	}
	spin_unlock(&s->lock);
	if (release) {
		page->mapping = NULL;
		page->private = 0;
		__folio_clear_slab(page_folio(page));
		__free_pages(page, s->order);
	}
}

void kmem_cache_free(struct kmem_cache *s, void *object)
{
	if (object)
		cache_free(s, slab_page(object), object);
}

void kmem_cache_free_bulk(struct kmem_cache *s, size_t size, void **p)
{
	size_t i;

	for (i = 0; i < size; i++)
		if (s)
			kmem_cache_free(s, p[i]);
		else
			kfree(p[i]);
}

struct kmem_cache *__kmem_cache_create_args(const char *name, unsigned int object_size,
					    struct kmem_cache_args *args, slab_flags_t flags)
{
	struct kmem_cache *s = kzalloc(sizeof(*s), GFP_KERNEL);

	if (!s)
		return NULL;
	init_cache(s, name, object_size, args->align, flags, args->ctor);
	return s;
}

unsigned int kmem_cache_size(struct kmem_cache *s)
{
	return s->size;
}

/* There are no buckets of caches: their users take kmalloc's caches. */
kmem_buckets *kmem_buckets_create(const char *name, slab_flags_t flags, unsigned int useroffset,
				  unsigned int usersize, void (*ctor)(void *))
{
	return NULL;
}

bool kmem_dump_obj(void *object)
{
	return false;
}

void kmem_cache_destroy(struct kmem_cache *s)
{
	struct page *page, *next;

	if (!s)
		return;
	if (s->flags & SLAB_TYPESAFE_BY_RCU)
		rcu_barrier();
	list_for_each_entry_safe(page, next, &s->partial, lru) {
		if (page->__folio_index)
			mikros_log("cache %s is destroyed with objects in use", s->name);
		list_del(&page->lru);
		page->mapping = NULL;
		page->private = 0;
		__folio_clear_slab(page_folio(page));
		__free_pages(page, s->order);
	}
	kfree(s);
}

/* Sheaves, which the maple tree fills ahead of time. */
struct slab_sheaf {
	unsigned int size, capacity;
	void *objects[];
};

static int fill_sheaf(struct kmem_cache *s, gfp_t gfp, struct slab_sheaf *sheaf, unsigned int size)
{
	while (sheaf->size < size) {
		void *object = kmem_cache_alloc_noprof(s, gfp);

		if (!object)
			return -ENOMEM;
		sheaf->objects[sheaf->size++] = object;
	}
	return 0;
}

struct slab_sheaf *kmem_cache_prefill_sheaf(struct kmem_cache *s, gfp_t gfp, unsigned int size)
{
	struct slab_sheaf *sheaf = kzalloc(struct_size(sheaf, objects, size), gfp);

	if (!sheaf)
		return NULL;
	sheaf->capacity = size;
	if (fill_sheaf(s, gfp, sheaf, size)) {
		kmem_cache_return_sheaf(s, gfp, sheaf);
		return NULL;
	}
	return sheaf;
}

int kmem_cache_refill_sheaf(struct kmem_cache *s, gfp_t gfp, struct slab_sheaf **sheafp,
			    unsigned int size)
{
	struct slab_sheaf *sheaf = *sheafp;

	if (!sheaf) {
		*sheafp = kmem_cache_prefill_sheaf(s, gfp, size);
		return *sheafp ? 0 : -ENOMEM;
	}
	if (size > sheaf->capacity) {
		struct slab_sheaf *bigger = krealloc(sheaf, struct_size(sheaf, objects, size), gfp);

		if (!bigger)
			return -ENOMEM;
		bigger->capacity = size;
		*sheafp = sheaf = bigger;
	}
	return fill_sheaf(s, gfp, sheaf, size);
}

void kmem_cache_return_sheaf(struct kmem_cache *s, gfp_t gfp, struct slab_sheaf *sheaf)
{
	while (sheaf->size)
		kmem_cache_free(s, sheaf->objects[--sheaf->size]);
	kfree(sheaf);
}

void *kmem_cache_alloc_from_sheaf_noprof(struct kmem_cache *s, gfp_t gfp,
					 struct slab_sheaf *sheaf)
{
	void *object;

	if (!sheaf->size)
		return NULL;
	object = sheaf->objects[--sheaf->size];
	if (gfp & __GFP_ZERO)
		memset(object, 0, s->size);
	return object;
}

unsigned int kmem_cache_sheaf_size(struct slab_sheaf *sheaf)
{
	return sheaf->size;
}

/* kmalloc. */

static void init_kmalloc(void)
{
	static const char *const names[] = {
		[1] = "kmalloc-96", [2] = "kmalloc-192", [3] = "kmalloc-8",
		[4] = "kmalloc-16", [5] = "kmalloc-32", [6] = "kmalloc-64",
		[7] = "kmalloc-128", [8] = "kmalloc-256", [9] = "kmalloc-512",
		[10] = "kmalloc-1k", [11] = "kmalloc-2k", [12] = "kmalloc-4k",
		[13] = "kmalloc-8k",
	};
	unsigned int i, type;

	BUILD_BUG_ON(KMALLOC_SHIFT_HIGH != 13);
	for (i = 1; i <= KMALLOC_SHIFT_HIGH; i++) {
		unsigned int size = i == 1 ? 96 : i == 2 ? 192 : 1U << i;

		init_cache(&kmalloc_cache[i], names[i], size, 0, 0, NULL);
		for (type = 0; type < NR_KMALLOC_TYPES; type++)
			kmalloc_caches[type][i] = &kmalloc_cache[i];
	}
}

static void *kmalloc_large(size_t size, gfp_t flags)
{
	unsigned int order = get_order(size);
	struct page *page = alloc_block(flags | __GFP_COMP, order);

	if (!page)
		return NULL;
	__folio_set_large_kmalloc(page_folio(page));
	return page_address(page);
}

void *__kmalloc_noprof(size_t size, gfp_t flags)
{
	if (unlikely(!size))
		return ZERO_SIZE_PTR;
	if (size > KMALLOC_MAX_CACHE_SIZE)
		return kmalloc_large(size, flags);
	return kmem_cache_alloc_noprof(&kmalloc_cache[kmalloc_index(size)], flags);
}

void *__kmalloc_cache_noprof(struct kmem_cache *s, gfp_t flags, size_t size)
{
	return kmem_cache_alloc_noprof(s, flags);
}

void *__kmalloc_cache_node_noprof(struct kmem_cache *s, gfp_t flags, int node, size_t size)
{
	return kmem_cache_alloc_noprof(s, flags);
}

void *__kmalloc_large_noprof(size_t size, gfp_t flags)
{
	return kmalloc_large(size, flags);
}

void *__kmalloc_large_node_noprof(size_t size, gfp_t flags, int node)
{
	return kmalloc_large(size, flags);
}

void *__kmalloc_node_noprof(DECL_BUCKET_PARAMS(size, b), gfp_t flags, int node)
{
	return __kmalloc_noprof(size, flags);
}

void *__kmalloc_node_track_caller_noprof(DECL_BUCKET_PARAMS(size, b), gfp_t flags, int node,
					 unsigned long caller)
{
	return __kmalloc_noprof(size, flags);
}

size_t ksize(const void *object)
{
	struct folio *folio;

	if (ZERO_OR_NULL_PTR(object))
		return 0;
	folio = page_folio(slab_page(object));
	if (folio_test_slab(folio))
		return ((struct kmem_cache *)folio->private)->size;
	return folio_size(folio);
}

size_t kmalloc_size_roundup(size_t size)
{
	if (!size)
		return 0;
	if (size > KMALLOC_MAX_CACHE_SIZE)
		return PAGE_SIZE << get_order(size);
	return kmalloc_cache[kmalloc_index(size)].size;
}

void kfree(const void *object)
{
	struct folio *folio;

	if (ZERO_OR_NULL_PTR(object))
		return;
	folio = page_folio(slab_page(object));
	if (folio_test_slab(folio)) {
		cache_free(folio->private, &folio->page, (void *)object);
		return;
	}
	__folio_clear_large_kmalloc(folio);
	folio_put(folio);
}

void kfree_sensitive(const void *object)
{
	size_t size = ksize(object);

	if (size) {
		memzero_explicit((void *)object, size);
		kfree(object);
	}
}

void *krealloc_node_align_noprof(const void *p, size_t new_size, unsigned long align,
				 gfp_t flags, int nid)
{
	size_t old = ksize(p);
	void *new;

	if (!new_size) {
		kfree(p);
		return ZERO_SIZE_PTR;
	}
	if (p && new_size <= old && IS_ALIGNED((unsigned long)p, align))
		return (void *)p;
	new = __kmalloc_noprof(max_t(size_t, new_size, align), flags);
	if (!new)
		return NULL;
	if (p) {
		memcpy(new, p, min(old, new_size));
		kfree(p);
	}
	return new;
}
