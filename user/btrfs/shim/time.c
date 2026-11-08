// SPDX-License-Identifier: GPL-2.0
/*
 * Time: the host's clocks, jiffies, which a thread of the shim advances
 * HZ times a second, and timers, which that thread runs when they expire.
 */
#include <linux/ktime.h>
#include <linux/timekeeping.h>
#include <linux/jiffies.h>
#include <linux/timer.h>
#include <linux/spinlock.h>
#include <linux/sched.h>
#include <linux/uaccess.h>
#include "shim.h"

/* The monotonic time at which jiffies was INITIAL_JIFFIES. */
static s64 start;

static HLIST_HEAD(timers);
static DEFINE_SPINLOCK(timers_lock);
/* The timer that runs, and a word that changes when it is done. */
static struct timer_list *running;
static u32 ran;
/* Changes when the timers change, for the timer thread. */
static u32 changed;

ktime_t ktime_get(void)
{
	return mikros_host_monotonic();
}

time64_t ktime_get_seconds(void)
{
	return mikros_host_monotonic() / NSEC_PER_SEC;
}

time64_t ktime_get_real_seconds(void)
{
	return mikros_host_realtime() / NSEC_PER_SEC;
}

void ktime_get_real_ts64(struct timespec64 *ts)
{
	*ts = ns_to_timespec64(mikros_host_realtime());
}

void ktime_get_coarse_real_ts64(struct timespec64 *ts)
{
	ktime_get_real_ts64(ts);
}

/* The coarse clock is the fine one, so a multigrain timestamp, which is
 * never older than one handed out before, is the time. */
void ktime_get_real_ts64_mg(struct timespec64 *ts)
{
	ktime_get_real_ts64(ts);
}

void ktime_get_coarse_real_ts64_mg(struct timespec64 *ts)
{
	ktime_get_real_ts64(ts);
}

void set_normalized_timespec64(struct timespec64 *ts, time64_t sec, s64 nsec)
{
	s32 rem;

	sec += div_s64_rem(nsec, NSEC_PER_SEC, &rem);
	if (rem < 0) {
		rem += NSEC_PER_SEC;
		sec--;
	}
	ts->tv_sec = sec;
	ts->tv_nsec = rem;
}

int get_timespec64(struct timespec64 *ts, const struct __kernel_timespec __user *uts)
{
	struct __kernel_timespec kts;

	if (copy_from_user(&kts, uts, sizeof(kts)))
		return -EFAULT;
	ts->tv_sec = kts.tv_sec;
	ts->tv_nsec = kts.tv_nsec;
	return 0;
}

struct timespec64 ns_to_timespec64(s64 nsec)
{
	struct timespec64 ts = { 0, 0 };
	s32 rem;

	if (likely(nsec > 0)) {
		ts.tv_sec = div_u64_rem(nsec, NSEC_PER_SEC, &rem);
		ts.tv_nsec = rem;
	} else if (nsec < 0) {
		ts.tv_sec = -div_u64_rem(-nsec - 1, NSEC_PER_SEC, &rem) - 1;
		ts.tv_nsec = NSEC_PER_SEC - rem - 1;
	}
	return ts;
}

unsigned long nsecs_to_jiffies(u64 n)
{
	return div_u64(n, NSEC_PER_SEC / HZ);
}

u64 nsecs_to_jiffies64(u64 n)
{
	return div_u64(n, NSEC_PER_SEC / HZ);
}

unsigned int jiffies_to_msecs(const unsigned long j)
{
	return (MSEC_PER_SEC / HZ) * j;
}

unsigned int jiffies_to_usecs(const unsigned long j)
{
	return (USEC_PER_SEC / HZ) * j;
}

unsigned long __msecs_to_jiffies(const unsigned int m)
{
	return DIV_ROUND_UP(m, MSEC_PER_SEC / HZ);
}

unsigned long __usecs_to_jiffies(const unsigned int u)
{
	return DIV_ROUND_UP(u, USEC_PER_SEC / HZ);
}

/* Timers that wake the processor up save no power here, so there is
 * nothing to gain by rounding them to whole seconds. */
unsigned long round_jiffies_relative(unsigned long j)
{
	return j;
}

void timer_init_key(struct timer_list *timer, void (*func)(struct timer_list *),
		    unsigned int flags, const char *name, struct lock_class_key *key)
{
	timer->entry.pprev = NULL;
	timer->function = func;
	timer->flags = flags;
}

int mod_timer(struct timer_list *timer, unsigned long expires)
{
	int pending;

	spin_lock(&timers_lock);
	pending = timer_pending(timer);
	if (pending)
		hlist_del(&timer->entry);
	timer->expires = expires;
	hlist_add_head(&timer->entry, &timers);
	spin_unlock(&timers_lock);
	__atomic_fetch_add(&changed, 1, __ATOMIC_RELEASE);
	mikros_wake(&changed, 1);
	return pending;
}

void add_timer(struct timer_list *timer)
{
	mod_timer(timer, timer->expires);
}

int timer_delete(struct timer_list *timer)
{
	int pending;

	spin_lock(&timers_lock);
	pending = timer_pending(timer);
	if (pending)
		hlist_del_init(&timer->entry);
	spin_unlock(&timers_lock);
	return pending;
}

int timer_delete_sync(struct timer_list *timer)
{
	int pending = timer_delete(timer);
	u32 seq;

	for (;;) {
		seq = __atomic_load_n(&ran, __ATOMIC_ACQUIRE);
		if (READ_ONCE(running) != timer)
			return pending;
		mikros_wait(&ran, seq, MIKROS_HOST_FOREVER);
	}
}

/* Advances jiffies, runs the timers that expired, and returns when the
 * next one does, in jiffies. */
static unsigned long tick(void)
{
	unsigned long now, next;
	struct timer_list *timer;

	now = INITIAL_JIFFIES + div64_s64(mikros_host_monotonic() - start, TICK_NSEC);
	WRITE_ONCE(jiffies, now);
	spin_lock(&timers_lock);
	for (;;) {
		struct hlist_node *n;

		next = now + HZ;
		timer = NULL;
		hlist_for_each_entry_safe(timer, n, &timers, entry) {
			if (time_after_eq(now, timer->expires))
				break;
			if (time_before(timer->expires, next))
				next = timer->expires;
		}
		if (!timer)
			break;
		hlist_del_init(&timer->entry);
		WRITE_ONCE(running, timer);
		spin_unlock(&timers_lock);
		timer->function(timer);
		spin_lock(&timers_lock);
		WRITE_ONCE(running, NULL);
		__atomic_fetch_add(&ran, 1, __ATOMIC_RELEASE);
		mikros_wake(&ran, INT_MAX);
	}
	spin_unlock(&timers_lock);
	return next;
}

static void timer_thread(void *arg)
{
	for (;;) {
		u32 seq = __atomic_load_n(&changed, __ATOMIC_ACQUIRE);
		unsigned long next = tick();
		s64 deadline;

		/* jiffies advances at least each tick. */
		next = min(next, READ_ONCE(jiffies) + 1);
		deadline = start + (s64)(next - INITIAL_JIFFIES) * TICK_NSEC;
		mikros_wait(&changed, seq, deadline);
	}
}

void mikros_time_init(void)
{
	struct task_struct *task = mikros_task_createf("timers");

	start = mikros_host_monotonic();
	WRITE_ONCE(jiffies, INITIAL_JIFFIES);
	if (!task || mikros_task_start(task, timer_thread, NULL))
		panic("cannot start the timer thread");
}
