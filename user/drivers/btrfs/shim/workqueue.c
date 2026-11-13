// SPDX-License-Identifier: GPL-2.0
/*
 * Workqueues: each has worker threads of its own, started when work comes
 * and none is idle, up to its max_active, and ending after a while idle.
 * An ordered workqueue has one. A work item's data holds its pending bit
 * and, once queued, its workqueue, as Linux's does; delayed work waits on
 * a timer.
 */
#include <linux/workqueue.h>
#include <linux/slab.h>
#include <linux/sched.h>
#include <linux/timer.h>
#include <linux/jiffies.h>
#include "shim.h"

/* How long a worker waits for work before it ends. */
#define IDLE_NS		(10 * NSEC_PER_SEC)
#define RUNNING_MAX	64
#define WQ_NAME_LEN	32

struct workqueue_struct {
	char name[WQ_NAME_LEN];
	unsigned int flags;
	int max_active;
	spinlock_t lock;
	struct list_head pending;
	/* The work items that workers run, and how many workers there are
	 * and wait for work. */
	struct work_struct *running[RUNNING_MAX];
	int workers, idle;
	/* How many items have been queued, and ran or were cancelled. */
	u64 queued, finished;
	bool dying;
	/* Changes when work comes or finishes. */
	u32 changed;
} __aligned(1 << WORK_STRUCT_PWQ_SHIFT);

struct workqueue_struct *system_wq;
struct workqueue_struct *system_percpu_wq;
struct workqueue_struct *system_dfl_wq;
struct workqueue_struct *system_highpri_wq;
struct workqueue_struct *system_long_wq;
struct workqueue_struct *system_unbound_wq;
struct workqueue_struct *system_freezable_wq;
struct workqueue_struct *system_power_efficient_wq;
struct workqueue_struct *system_freezable_power_efficient_wq;

static void kick(struct workqueue_struct *wq)
{
	__atomic_fetch_add(&wq->changed, 1, __ATOMIC_RELEASE);
	mikros_wake(&wq->changed, INT_MAX);
}

/* The workqueue that `work` was queued on last, if any. */
static struct workqueue_struct *wq_of(struct work_struct *work)
{
	unsigned long data = atomic_long_read(&work->data);

	if (!(data & WORK_STRUCT_PWQ))
		return NULL;
	return (struct workqueue_struct *)(data & WORK_STRUCT_PWQ_MASK);
}

static bool is_running(struct workqueue_struct *wq, struct work_struct *work)
{
	int i;

	for (i = 0; i < RUNNING_MAX; i++)
		if (wq->running[i] == work)
			return true;
	return false;
}

/* The first item of `wq` to run: one that no worker runs, as an item
 * never runs on two at once. */
static struct work_struct *next_work(struct workqueue_struct *wq)
{
	struct work_struct *work;

	list_for_each_entry(work, &wq->pending, entry)
		if (!is_running(wq, work))
			return work;
	return NULL;
}

static void worker(void *arg)
{
	struct workqueue_struct *wq = arg;
	struct work_struct *work;
	int slot;

	spin_lock(&wq->lock);
	for (;;) {
		while (!(work = next_work(wq))) {
			u32 seq = __atomic_load_n(&wq->changed, __ATOMIC_ACQUIRE);
			s64 deadline = mikros_host_monotonic() + IDLE_NS;

			if (wq->dying)
				goto out;
			wq->idle++;
			spin_unlock(&wq->lock);
			mikros_sleep(&wq->changed, seq, deadline);
			spin_lock(&wq->lock);
			wq->idle--;
			if (list_empty(&wq->pending) && mikros_host_monotonic() >= deadline)
				goto out;
		}
		list_del_init(&work->entry);
		for (slot = 0; wq->running[slot]; slot++)
			;
		wq->running[slot] = work;
		/* The item may be queued again while it runs. */
		atomic_long_set(&work->data, (unsigned long)wq | WORK_STRUCT_PWQ);
		spin_unlock(&wq->lock);
		work->func(work);
		spin_lock(&wq->lock);
		wq->running[slot] = NULL;
		wq->finished++;
		kick(wq);
	}
out:
	wq->workers--;
	kick(wq);
	spin_unlock(&wq->lock);
}

/* Queues `work`, whose pending bit its caller set, on `wq`. */
static void enqueue(struct workqueue_struct *wq, struct work_struct *work)
{
	bool start = false;

	spin_lock(&wq->lock);
	atomic_long_set(&work->data, (unsigned long)wq | WORK_STRUCT_PWQ | WORK_STRUCT_PENDING);
	list_add_tail(&work->entry, &wq->pending);
	wq->queued++;
	if (!wq->idle && wq->workers < wq->max_active) {
		wq->workers++;
		start = true;
	}
	kick(wq);
	spin_unlock(&wq->lock);
	if (start) {
		struct task_struct *task = mikros_task_createf("kworker/%s", wq->name);

		if (!task || mikros_task_start(task, worker, wq))
			panic("cannot start a worker of %s", wq->name);
	}
}

bool queue_work_on(int cpu, struct workqueue_struct *wq, struct work_struct *work)
{
	if (test_and_set_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work)))
		return false;
	enqueue(wq, work);
	return true;
}

void delayed_work_timer_fn(struct timer_list *t)
{
	struct delayed_work *dwork = timer_container_of(dwork, t, timer);

	enqueue(dwork->wq, &dwork->work);
}

bool queue_delayed_work_on(int cpu, struct workqueue_struct *wq,
			   struct delayed_work *dwork, unsigned long delay)
{
	if (test_and_set_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(&dwork->work)))
		return false;
	dwork->wq = wq;
	dwork->cpu = cpu;
	if (!delay) {
		enqueue(wq, &dwork->work);
		return true;
	}
	mod_timer(&dwork->timer, jiffies + delay);
	return true;
}

/* Takes `work` off its workqueue, unless it runs: whether it was pending. */
static bool dequeue(struct work_struct *work)
{
	struct workqueue_struct *wq = wq_of(work);
	bool pending = false;

	if (!wq)
		return test_and_clear_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work));
	spin_lock(&wq->lock);
	if (test_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work)) && !list_empty(&work->entry)) {
		list_del_init(&work->entry);
		clear_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work));
		wq->finished++;
		pending = true;
		kick(wq);
	}
	spin_unlock(&wq->lock);
	return pending;
}

/* Waits until `work` is neither pending nor running. Whether it was. */
static bool wait_for(struct work_struct *work)
{
	struct workqueue_struct *wq = wq_of(work);
	bool waited = false;

	if (!wq)
		return false;
	for (;;) {
		u32 seq = __atomic_load_n(&wq->changed, __ATOMIC_ACQUIRE);
		bool busy;

		spin_lock(&wq->lock);
		busy = test_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work)) ||
		       is_running(wq, work);
		spin_unlock(&wq->lock);
		if (!busy)
			return waited;
		waited = true;
		mikros_sleep(&wq->changed, seq, MIKROS_HOST_FOREVER);
	}
}

bool flush_work(struct work_struct *work)
{
	return wait_for(work);
}

bool cancel_work(struct work_struct *work)
{
	return dequeue(work);
}

bool cancel_work_sync(struct work_struct *work)
{
	bool pending = dequeue(work);

	wait_for(work);
	return pending;
}

/* Takes a delayed work item off its timer, or else off its workqueue:
 * whether it was pending. */
static bool dequeue_delayed(struct delayed_work *dwork)
{
	if (timer_delete_sync(&dwork->timer)) {
		clear_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(&dwork->work));
		return true;
	}
	return dequeue(&dwork->work);
}

bool mod_delayed_work_on(int cpu, struct workqueue_struct *wq,
			 struct delayed_work *dwork, unsigned long delay)
{
	bool pending = dequeue_delayed(dwork);

	queue_delayed_work_on(cpu, wq, dwork, delay);
	return pending;
}

bool cancel_delayed_work(struct delayed_work *dwork)
{
	return dequeue_delayed(dwork);
}

bool cancel_delayed_work_sync(struct delayed_work *dwork)
{
	bool pending = dequeue_delayed(dwork);

	wait_for(&dwork->work);
	return pending;
}

bool flush_delayed_work(struct delayed_work *dwork)
{
	if (timer_delete_sync(&dwork->timer))
		enqueue(dwork->wq, &dwork->work);
	return flush_work(&dwork->work);
}

void __flush_workqueue(struct workqueue_struct *wq)
{
	u64 target;

	spin_lock(&wq->lock);
	target = wq->queued;
	spin_unlock(&wq->lock);
	for (;;) {
		u32 seq = __atomic_load_n(&wq->changed, __ATOMIC_ACQUIRE);

		if (READ_ONCE(wq->finished) >= target)
			return;
		mikros_sleep(&wq->changed, seq, MIKROS_HOST_FOREVER);
	}
}

void drain_workqueue(struct workqueue_struct *wq)
{
	u64 queued;

	do {
		spin_lock(&wq->lock);
		queued = wq->queued;
		spin_unlock(&wq->lock);
		__flush_workqueue(wq);
	} while (READ_ONCE(wq->queued) != queued);
}

unsigned int work_busy(struct work_struct *work)
{
	struct workqueue_struct *wq = wq_of(work);
	unsigned int busy = 0;

	if (test_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work)))
		busy |= WORK_BUSY_PENDING;
	if (wq) {
		spin_lock(&wq->lock);
		if (is_running(wq, work))
			busy |= WORK_BUSY_RUNNING;
		spin_unlock(&wq->lock);
	}
	return busy;
}

void workqueue_set_max_active(struct workqueue_struct *wq, int max_active)
{
	spin_lock(&wq->lock);
	wq->max_active = clamp(max_active, 1, RUNNING_MAX);
	spin_unlock(&wq->lock);
}

struct workqueue_struct *alloc_workqueue_noprof(const char *fmt, unsigned int flags,
						int max_active, ...)
{
	struct workqueue_struct *wq = kzalloc(sizeof(*wq), GFP_KERNEL);
	va_list args;

	if (!wq)
		return NULL;
	va_start(args, max_active);
	vsnprintf(wq->name, sizeof(wq->name), fmt, args);
	va_end(args);
	if (flags & __WQ_ORDERED)
		max_active = 1;
	else if (!max_active)
		max_active = WQ_DFL_ACTIVE;
	wq->flags = flags;
	wq->max_active = clamp(max_active, 1, RUNNING_MAX);
	spin_lock_init(&wq->lock);
	INIT_LIST_HEAD(&wq->pending);
	return wq;
}

void destroy_workqueue(struct workqueue_struct *wq)
{
	drain_workqueue(wq);
	spin_lock(&wq->lock);
	wq->dying = true;
	kick(wq);
	while (wq->workers) {
		u32 seq = __atomic_load_n(&wq->changed, __ATOMIC_ACQUIRE);

		spin_unlock(&wq->lock);
		mikros_sleep(&wq->changed, seq, MIKROS_HOST_FOREVER);
		spin_lock(&wq->lock);
	}
	spin_unlock(&wq->lock);
	kfree(wq);
}

/* Workers have no descriptions to show, and no workqueue has a rescuer:
 * a worker starts whenever one is needed. */
void set_worker_desc(const char *fmt, ...)
{
}

bool current_is_workqueue_rescuer(void)
{
	return false;
}

void mikros_workqueue_init(void)
{
	system_percpu_wq = alloc_workqueue("events", 0, 0);
	system_dfl_wq = alloc_workqueue("events_unbound", WQ_UNBOUND, 0);
	if (!system_percpu_wq || !system_dfl_wq)
		panic("no memory for the system workqueues");
	system_wq = system_percpu_wq;
	system_highpri_wq = system_percpu_wq;
	system_long_wq = system_percpu_wq;
	system_freezable_wq = system_percpu_wq;
	system_power_efficient_wq = system_percpu_wq;
	system_freezable_power_efficient_wq = system_percpu_wq;
	system_unbound_wq = system_dfl_wq;
}
