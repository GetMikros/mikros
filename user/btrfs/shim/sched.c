// SPDX-License-Identifier: GPL-2.0
/*
 * The shim's scheduler: each task is a thread of the program, on a CPU of
 * its own while it runs. A task sleeps as Linux's do: it sets its state,
 * checks what it waits for, and calls schedule(), which waits on the
 * task's word until a wakeup has set the state back to TASK_RUNNING.
 * Kernel threads, wake queues and timeouts are built on that.
 */
#include <linux/sched.h>
#include <linux/sched/task.h>
#include <linux/sched/wake_q.h>
#include <linux/sched/signal.h>
#include <linux/kthread.h>
#include <linux/rcuwait.h>
#include <linux/slab.h>
#include <linux/cred.h>
#include <linux/completion.h>
#include <linux/bottom_half.h>
#include <linux/highuid.h>
#include <linux/binfmts.h>
#include <linux/task_work.h>
#include <linux/sched/coredump.h>
#include <linux/sched/mm.h>
#include <linux/init_task.h>
#include <linux/wait_bit.h>
#include <linux/user_namespace.h>
#include <linux/futex.h>
#include "shim.h"

/* A task and what the shim keeps with it. */
struct mikros_task {
	struct task_struct task;
	/* Changes, and is waited on, for each wakeup. */
	u32 wake;
	struct list_head all;
	/* For a kernel thread: its function, its data and its result. */
	int (*threadfn)(void *data);
	void *data;
	int result;
	unsigned long kthread_flags;
	struct completion parked;
	struct completion exited;
};

enum {
	KTHREAD_SHOULD_STOP,
	KTHREAD_SHOULD_PARK,
	KTHREAD_IS_PARKED,
};

static inline struct mikros_task *to_mikros(struct task_struct *task)
{
	return container_of(task, struct mikros_task, task);
}

/* The tasks, and the CPUs that running ones hold. */
static LIST_HEAD(tasks);
static DEFINE_SPINLOCK(tasks_lock);
static DECLARE_BITMAP(cpus_held, NR_CPUS);
static atomic_t next_pid = ATOMIC_INIT(1);

DEFINE_RWLOCK(tasklist_lock);
unsigned long init_stack[THREAD_SIZE / sizeof(unsigned long)];
int overflowuid = DEFAULT_OVERFLOWUID;
int overflowgid = DEFAULT_OVERFLOWGID;
int suid_dumpable;

struct task_struct *mikros_task_create(const char *fmt, va_list args)
{
	struct mikros_task *mt = kzalloc(sizeof(*mt), GFP_KERNEL);
	struct task_struct *task;

	if (!mt)
		return NULL;
	task = &mt->task;
	refcount_set(&task->usage, 1);
	task->__state = TASK_RUNNING;
	task->pid = task->tgid = atomic_inc_return(&next_pid);
	/* Every task has the credentials of the system, and its file system
	 * context, namespaces and signal state, as Linux's kernel threads
	 * share init_task's. */
	task->real_cred = get_cred(init_task.real_cred);
	task->cred = get_cred(init_task.cred);
	task->fs = init_task.fs;
	task->files = init_task.files;
	task->nsproxy = init_task.nsproxy;
	task->signal = init_task.signal;
	task->sighand = init_task.sighand;
	task->active_mm = init_task.active_mm;
	task->prio = task->static_prio = task->normal_prio = DEFAULT_PRIO;
	if (fmt)
		vsnprintf(task->comm, sizeof(task->comm), fmt, args);
	init_completion(&mt->parked);
	init_completion(&mt->exited);
	spin_lock(&tasks_lock);
	list_add_tail(&mt->all, &tasks);
	spin_unlock(&tasks_lock);
	return task;
}

struct task_struct *mikros_task_createf(const char *fmt, ...)
{
	struct task_struct *task;
	va_list args;

	va_start(args, fmt);
	task = mikros_task_create(fmt, args);
	va_end(args);
	return task;
}

void mikros_task_enter(struct task_struct *task)
{
	int cpu;

	spin_lock(&tasks_lock);
	cpu = find_first_zero_bit(cpus_held, NR_CPUS);
	if (cpu < NR_CPUS)
		__set_bit(cpu, cpus_held);
	spin_unlock(&tasks_lock);
	if (cpu >= NR_CPUS)
		panic("the shim runs more than %d threads", NR_CPUS);
	mikros_cpu = cpu;
	mikros_irq_flags = ARCH_IRQ_ENABLED;
	task->thread_info.cpu = cpu;
	task->wake_cpu = cpu;
	mikros_current_task = task;
}

void mikros_task_exit(void)
{
	struct task_struct *task = current;

	spin_lock(&tasks_lock);
	__clear_bit(mikros_cpu, cpus_held);
	spin_unlock(&tasks_lock);
	WRITE_ONCE(task->__state, TASK_DEAD);
	/* The thread's reference: the task goes once nothing else holds it. */
	put_task_struct(task);
	mikros_current_task = NULL;
}

/* The last reference to a task went: it leaves the list and is freed. */
void __put_task_struct_rcu_cb(struct rcu_head *rhp)
{
	struct mikros_task *mt = to_mikros(container_of(rhp, struct task_struct, rcu));

	spin_lock(&tasks_lock);
	list_del(&mt->all);
	spin_unlock(&tasks_lock);
	put_cred(mt->task.cred);
	put_cred(mt->task.real_cred);
	kfree(mt);
}

void mikros_for_each_task(void (*fn)(struct task_struct *task, void *arg), void *arg)
{
	struct mikros_task *mt;

	spin_lock(&tasks_lock);
	list_for_each_entry(mt, &tasks, all)
		fn(&mt->task, arg);
	spin_unlock(&tasks_lock);
}

bool mikros_task_adopt(const char *name)
{
	struct task_struct *task;

	if (mikros_current_task)
		return false;
	task = mikros_task_createf("%s", name);
	if (!task)
		panic("no memory for a task of %s", name);
	mikros_task_enter(task);
	return true;
}

struct start {
	struct task_struct *task;
	void (*entry)(void *arg);
	void *arg;
};

static void task_thread(void *arg)
{
	struct start start = *(struct start *)arg;

	kfree(arg);
	mikros_task_enter(start.task);
	start.entry(start.arg);
	mikros_task_exit();
}

int mikros_task_start(struct task_struct *task, void (*entry)(void *arg), void *arg)
{
	struct start *start = kmalloc(sizeof(*start), GFP_KERNEL);
	int ret;

	if (!start)
		return -ENOMEM;
	start->task = task;
	start->entry = entry;
	start->arg = arg;
	/* The thread owns the reference that creating the task gave. */
	ret = mikros_host_thread(task_thread, start);
	if (ret)
		kfree(start);
	return ret;
}

void mikros_sched_init(void)
{
	struct task_struct *task = mikros_task_createf("main");
	int i;

	if (!task)
		panic("no memory for the first task");
	mikros_task_enter(task);
	/* As the release's sched_init() does. */
	wait_bit_init();
	/* As its fork_init() does, for as many threads as Linux allows. */
	for (i = 0; i < UCOUNT_COUNTS; i++)
		init_user_ns.ucount_max[i] = FUTEX_TID_MASK / 2;
	set_userns_rlimit_max(&init_user_ns, UCOUNT_RLIMIT_NPROC, RLIM_INFINITY);
	set_userns_rlimit_max(&init_user_ns, UCOUNT_RLIMIT_MSGQUEUE, RLIM_INFINITY);
	set_userns_rlimit_max(&init_user_ns, UCOUNT_RLIMIT_SIGPENDING, RLIM_INFINITY);
	set_userns_rlimit_max(&init_user_ns, UCOUNT_RLIMIT_MEMLOCK, RLIM_INFINITY);
}

/* Sleeps until the state of the task is TASK_RUNNING again, or until
 * `deadline`; whether it was woken. */
static bool sleep_until(s64 deadline)
{
	struct mikros_task *mt = to_mikros(current);
	u32 seq;

	for (;;) {
		seq = __atomic_load_n(&mt->wake, __ATOMIC_ACQUIRE);
		if (READ_ONCE(current->__state) == TASK_RUNNING)
			return true;
		if (deadline != MIKROS_HOST_FOREVER && mikros_host_monotonic() >= deadline)
			return false;
		mikros_wait(&mt->wake, seq, deadline);
	}
}

void schedule(void)
{
	sleep_until(MIKROS_HOST_FOREVER);
}

void schedule_preempt_disabled(void)
{
	sched_preempt_enable_no_resched();
	schedule();
	preempt_disable();
}

void io_schedule(void)
{
	schedule();
}

int io_schedule_prepare(void)
{
	return 0;
}

void io_schedule_finish(int token)
{
}

long schedule_timeout(long timeout)
{
	s64 deadline;

	if (timeout == MAX_SCHEDULE_TIMEOUT) {
		schedule();
		return timeout;
	}
	if (timeout < 0)
		timeout = 0;
	deadline = mikros_host_monotonic() + jiffies_to_nsecs(timeout);
	if (sleep_until(deadline)) {
		s64 left = deadline - mikros_host_monotonic();

		return left > 0 ? nsecs_to_jiffies(left) : 0;
	}
	__set_current_state(TASK_RUNNING);
	return 0;
}

long io_schedule_timeout(long timeout)
{
	return schedule_timeout(timeout);
}

long schedule_timeout_interruptible(long timeout)
{
	__set_current_state(TASK_INTERRUPTIBLE);
	return schedule_timeout(timeout);
}

long schedule_timeout_killable(long timeout)
{
	__set_current_state(TASK_KILLABLE);
	return schedule_timeout(timeout);
}

long schedule_timeout_uninterruptible(long timeout)
{
	__set_current_state(TASK_UNINTERRUPTIBLE);
	return schedule_timeout(timeout);
}

/* Sets the state of `p` to TASK_RUNNING if it is in one of `state`, and
 * wakes it; whether it did. */
int try_to_wake_up(struct task_struct *p, unsigned int state, int wake_flags)
{
	struct mikros_task *mt = to_mikros(p);
	unsigned int s;

	smp_mb();
	s = READ_ONCE(p->__state);
	do {
		if (!(s & state))
			return 0;
	} while (!try_cmpxchg(&p->__state, &s, TASK_RUNNING));
	__atomic_fetch_add(&mt->wake, 1, __ATOMIC_RELEASE);
	mikros_wake(&mt->wake, 1);
	return 1;
}

int wake_up_process(struct task_struct *p)
{
	return try_to_wake_up(p, TASK_NORMAL, 0);
}

int wake_up_state(struct task_struct *p, unsigned int state)
{
	return try_to_wake_up(p, state, 0);
}

int default_wake_function(wait_queue_entry_t *curr, unsigned mode, int wake_flags,
			  void *key)
{
	return try_to_wake_up(curr->private, mode, wake_flags);
}

/* There is nothing to give the processor up for: other tasks run on
 * processors of their own. */
int __cond_resched(void)
{
	return 0;
}

int __cond_resched_lock(spinlock_t *lock)
{
	return 0;
}

int __cond_resched_rwlock_write(rwlock_t *lock)
{
	return 0;
}

void __local_bh_enable_ip(unsigned long ip, unsigned int cnt)
{
	__preempt_count_sub(cnt);
}

static bool __wake_q_add(struct wake_q_head *head, struct task_struct *task)
{
	struct wake_q_node *node = &task->wake_q;

	smp_mb__before_atomic();
	if (unlikely(cmpxchg_relaxed(&node->next, NULL, WAKE_Q_TAIL)))
		return false;
	*head->lastp = node;
	head->lastp = &node->next;
	return true;
}

void wake_q_add(struct wake_q_head *head, struct task_struct *task)
{
	if (__wake_q_add(head, task))
		get_task_struct(task);
}

void wake_q_add_safe(struct wake_q_head *head, struct task_struct *task)
{
	if (!__wake_q_add(head, task))
		put_task_struct(task);
}

void wake_up_q(struct wake_q_head *head)
{
	struct wake_q_node *node = head->first;

	while (node != WAKE_Q_TAIL) {
		struct task_struct *task = container_of(node, struct task_struct, wake_q);

		node = node->next;
		WRITE_ONCE(task->wake_q.next, NULL);
		wake_up_process(task);
		put_task_struct(task);
	}
}

int rcuwait_wake_up(struct rcuwait *w)
{
	struct task_struct *task;
	int ret = 0;

	smp_mb();
	rcu_read_lock();
	task = rcu_dereference(w->task);
	if (task)
		ret = wake_up_process(task);
	rcu_read_unlock();
	return ret;
}

void finish_rcuwait(struct rcuwait *w)
{
	rcu_assign_pointer(w->task, NULL);
	__set_current_state(TASK_RUNNING);
}

/* Kernel threads. */

static struct mikros_task *kthread_of(struct task_struct *task)
{
	return task->flags & PF_KTHREAD ? to_mikros(task) : NULL;
}

static void kthread_main(void *arg)
{
	struct mikros_task *mt = arg;

	/* A new kernel thread waits for its first wakeup, in the state
	 * that kthread_create_on_node() gave it, which that wakeup may have
	 * ended before the thread started. */
	schedule();
	if (!test_bit(KTHREAD_SHOULD_STOP, &mt->kthread_flags))
		mt->result = mt->threadfn(mt->data);
	complete_all(&mt->exited);
}

struct task_struct *kthread_create_on_node(int (*threadfn)(void *data), void *data,
					   int node, const char namefmt[], ...)
{
	struct task_struct *task;
	struct mikros_task *mt;
	va_list args;
	int ret;

	va_start(args, namefmt);
	task = mikros_task_create(namefmt, args);
	va_end(args);
	if (!task)
		return ERR_PTR(-ENOMEM);
	mt = to_mikros(task);
	mt->threadfn = threadfn;
	mt->data = data;
	task->flags |= PF_KTHREAD;
	/* Until it runs, the thread sleeps uninterruptibly. */
	task->__state = TASK_UNINTERRUPTIBLE;
	ret = mikros_task_start(task, kthread_main, mt);
	if (ret) {
		put_task_struct(task);
		return ERR_PTR(ret);
	}
	return task;
}

bool kthread_should_stop(void)
{
	struct mikros_task *mt = kthread_of(current);

	return mt && test_bit(KTHREAD_SHOULD_STOP, &mt->kthread_flags);
}

bool kthread_should_park(void)
{
	struct mikros_task *mt = kthread_of(current);

	return mt && test_bit(KTHREAD_SHOULD_PARK, &mt->kthread_flags);
}

bool kthread_should_stop_or_park(void)
{
	return kthread_should_stop() || kthread_should_park();
}

void kthread_parkme(void)
{
	struct mikros_task *mt = kthread_of(current);

	if (!mt)
		return;
	for (;;) {
		set_current_state(TASK_PARKED);
		if (!test_bit(KTHREAD_SHOULD_PARK, &mt->kthread_flags))
			break;
		if (!test_and_set_bit(KTHREAD_IS_PARKED, &mt->kthread_flags))
			complete(&mt->parked);
		schedule();
	}
	clear_bit(KTHREAD_IS_PARKED, &mt->kthread_flags);
	__set_current_state(TASK_RUNNING);
}

int kthread_park(struct task_struct *k)
{
	struct mikros_task *mt = kthread_of(k);

	if (!mt || test_bit(KTHREAD_SHOULD_STOP, &mt->kthread_flags))
		return -ENOSYS;
	set_bit(KTHREAD_SHOULD_PARK, &mt->kthread_flags);
	if (k != current) {
		wake_up_process(k);
		wait_for_completion(&mt->parked);
	}
	return 0;
}

int kthread_stop(struct task_struct *k)
{
	struct mikros_task *mt = kthread_of(k);
	int ret;

	get_task_struct(k);
	set_bit(KTHREAD_SHOULD_STOP, &mt->kthread_flags);
	clear_bit(KTHREAD_SHOULD_PARK, &mt->kthread_flags);
	wake_up_state(k, TASK_UNINTERRUPTIBLE | TASK_INTERRUPTIBLE | TASK_PARKED);
	wait_for_completion(&mt->exited);
	ret = mt->result;
	put_task_struct(k);
	return ret;
}

/* Signals: no task is ever sent one, and nothing ever runs a task's work
 * when it returns to user space, which none has, so that the callers do
 * what they do for kernel threads. */

int send_sig(int sig, struct task_struct *p, int priv)
{
	return 0;
}

int do_send_sig_info(int sig, struct kernel_siginfo *info, struct task_struct *p,
		     enum pid_type type)
{
	return 0;
}

int task_work_add(struct task_struct *task, struct callback_head *work,
		  enum task_work_notify_mode notify)
{
	return -ESRCH;
}

long do_no_restart_syscall(struct restart_block *param)
{
	return -EINTR;
}

/* No task has an address space of its own. */
struct mm_struct *get_task_mm(struct task_struct *task)
{
	return NULL;
}

void set_dumpable(struct mm_struct *mm, int value)
{
}
