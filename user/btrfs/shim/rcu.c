// SPDX-License-Identifier: GPL-2.0
/*
 * RCU: read-side sections are preemption-disabled sections, as in a
 * kernel without preemptible RCU, and a grace period ends once every
 * other task has been seen out of the sections it was in when it began:
 * with a preemption count of 0, or with a quiescent count that changed
 * since, which the shim's asm/preempt.h counts each time the preemption
 * count returns to 0. A thread of the shim runs the callbacks after the
 * grace periods that follow their call_rcu().
 */
#include <linux/rcupdate.h>
#include <linux/sched.h>
#include <linux/sched/task.h>
#include <linux/slab.h>
#include <linux/mm.h>
#include <linux/delay.h>
#include "shim.h"

/* The callbacks waiting for a grace period, and how many have been queued
 * and run, which rcu_barrier() compares. */
static struct rcu_head *queue;
static struct rcu_head **queue_tail = &queue;
static DEFINE_SPINLOCK(queue_lock);
static u64 queued, done;
/* Changes when callbacks are queued, and when they have run. */
static u32 queue_changed, done_changed;
/* Never changes, for sleeping until a deadline. */
static u32 nap;

/* A task seen when a grace period began. */
struct seen {
	struct task_struct *task;
	unsigned long quiescent;
	bool out;
};

struct snapshot {
	struct seen *seen;
	unsigned int count, room;
};

static void see(struct task_struct *task, void *arg)
{
	struct snapshot *snap = arg;
	struct seen *s;

	/* No room was left for an earlier task: the snapshot starts over. */
	if (!snap->room)
		return;
	if (task == current || READ_ONCE(task->__state) == TASK_DEAD ||
	    !refcount_inc_not_zero(&task->usage))
		return;
	if (snap->count == snap->room) {
		put_task_struct(task);
		snap->room = 0;
		return;
	}
	s = &snap->seen[snap->count++];
	s->task = task;
	s->quiescent = __atomic_load_n(&task->thread_info.quiescent, __ATOMIC_ACQUIRE);
	s->out = __atomic_load_n(&task->thread_info.preempt_count, __ATOMIC_SEQ_CST) == 0;
}

static void count(struct task_struct *task, void *arg)
{
	(*(unsigned int *)arg)++;
}

/* Waits until a grace period that begins now ends. */
static void grace_period(void)
{
	struct snapshot snap;
	unsigned int i, left, sleep = 1;

	for (;;) {
		unsigned int tasks = 0;

		mikros_for_each_task(count, &tasks);
		snap.room = tasks + 8;
		snap.count = 0;
		snap.seen = kmalloc_array(snap.room, sizeof(*snap.seen), GFP_KERNEL | __GFP_NOFAIL);
		mikros_for_each_task(see, &snap);
		if (snap.room)
			break;
		/* More tasks came than there was room for: again. */
		for (i = 0; i < snap.count; i++)
			put_task_struct(snap.seen[i].task);
		kfree(snap.seen);
	}
	do {
		left = 0;
		for (i = 0; i < snap.count; i++) {
			struct seen *s = &snap.seen[i];
			struct task_struct *t = s->task;

			if (!s->out)
				s->out = READ_ONCE(t->__state) == TASK_DEAD ||
					 __atomic_load_n(&t->thread_info.preempt_count,
							 __ATOMIC_SEQ_CST) == 0 ||
					 __atomic_load_n(&t->thread_info.quiescent,
							 __ATOMIC_ACQUIRE) != s->quiescent;
			left += !s->out;
		}
		if (left) {
			mikros_wait(&nap, 0, mikros_host_monotonic() + sleep * NSEC_PER_MSEC);
			sleep = min(sleep * 2, 10u);
		}
	} while (left);
	for (i = 0; i < snap.count; i++)
		put_task_struct(snap.seen[i].task);
	kfree(snap.seen);
}

void synchronize_rcu(void)
{
	might_sleep();
	grace_period();
}

void synchronize_rcu_expedited(void)
{
	synchronize_rcu();
}

void call_rcu(struct rcu_head *head, rcu_callback_t func)
{
	head->func = func;
	head->next = NULL;
	spin_lock(&queue_lock);
	*queue_tail = head;
	queue_tail = &head->next;
	queued++;
	spin_unlock(&queue_lock);
	__atomic_fetch_add(&queue_changed, 1, __ATOMIC_RELEASE);
	mikros_wake(&queue_changed, 1);
}

void kvfree_call_rcu(struct rcu_head *head, void *ptr)
{
	if (!head) {
		synchronize_rcu();
		kvfree(ptr);
		return;
	}
	/* The head lies in the memory: the callback frees `ptr` itself. */
	spin_lock(&queue_lock);
	head->func = (rcu_callback_t)ptr;
	head->next = NULL;
	*queue_tail = head;
	queue_tail = &head->next;
	queued++;
	spin_unlock(&queue_lock);
	__atomic_fetch_add(&queue_changed, 1, __ATOMIC_RELEASE);
	mikros_wake(&queue_changed, 1);
}

void rcu_barrier(void)
{
	u64 target;
	u32 seq;

	spin_lock(&queue_lock);
	target = queued;
	spin_unlock(&queue_lock);
	for (;;) {
		seq = __atomic_load_n(&done_changed, __ATOMIC_ACQUIRE);
		if (READ_ONCE(done) >= target)
			return;
		mikros_wait(&done_changed, seq, MIKROS_HOST_FOREVER);
	}
}

static void rcu_thread(void *arg)
{
	for (;;) {
		struct rcu_head *batch, *next;
		u64 count = 0;
		u32 seq;

		seq = __atomic_load_n(&queue_changed, __ATOMIC_ACQUIRE);
		spin_lock(&queue_lock);
		batch = queue;
		queue = NULL;
		queue_tail = &queue;
		spin_unlock(&queue_lock);
		if (!batch) {
			mikros_wait(&queue_changed, seq, MIKROS_HOST_FOREVER);
			continue;
		}
		grace_period();
		for (; batch; batch = next) {
			rcu_callback_t func = batch->func;

			next = batch->next;
			/* kvfree_call_rcu() keeps the memory in func, which is
			 * an address of the arena rather than of code. */
			if (virt_addr_valid(func))
				kvfree((void *)func);
			else
				func(batch);
			count++;
		}
		WRITE_ONCE(done, done + count);
		__atomic_fetch_add(&done_changed, 1, __ATOMIC_RELEASE);
		mikros_wake(&done_changed, INT_MAX);
	}
}

void mikros_rcu_init(void)
{
	struct task_struct *task = mikros_task_createf("rcu");

	if (!task || mikros_task_start(task, rcu_thread, NULL))
		panic("cannot start the RCU thread");
}
