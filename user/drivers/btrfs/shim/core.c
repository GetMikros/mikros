// SPDX-License-Identifier: GPL-2.0
/*
 * The shim's core: the state of each thread, the CPUs and per-CPU areas,
 * printk, panics and warnings, and mikros_btrfs_start(), which sets the
 * shim up and runs the initcalls of the code built on it.
 */
#include <linux/kernel.h>
#include <linux/init.h>
#include <linux/cpumask.h>
#include <linux/percpu.h>
#include <linux/printk.h>
#include <linux/sched.h>
#include <linux/sched/debug.h>
#include <linux/capability.h>
#include <linux/random.h>
#include <linux/notifier.h>
#include <linux/jiffies.h>
#include <linux/mm.h>
#include <linux/ucopysize.h>
#include <linux/tracepoint-defs.h>
#include <linux/radix-tree.h>
#include <linux/maple_tree.h>
#include <linux/utsname.h>
#include <linux/elf.h>
#include <linux/proc_ns.h>
#include <linux/swap.h>
#include <linux/workqueue.h>
#include "mm/internal.h"
#include "shim.h"

__thread struct task_struct *mikros_current_task;
__thread int mikros_cpu;
__thread unsigned long mikros_irq_flags;

/* Every CPU is possible and online: each running thread holds one. */
struct cpumask __cpu_possible_mask;
struct cpumask __cpu_online_mask;
struct cpumask __cpu_present_mask;
struct cpumask __cpu_active_mask;
struct cpumask __cpu_dying_mask;
atomic_t __num_online_cpus = ATOMIC_INIT(NR_CPUS);
unsigned int nr_cpu_ids = NR_CPUS;

enum system_states system_state = SYSTEM_BOOTING;

/* jiffies is jiffies_64, as the release's linker scripts make it. */
u64 jiffies_64 = INITIAL_JIFFIES;
asm(".globl jiffies\n.set jiffies, jiffies_64");

unsigned long empty_zero_page[PAGE_SIZE / sizeof(unsigned long)] __aligned(PAGE_SIZE);

struct uts_namespace init_uts_ns = {
	.ns.ns_type = ns_common_type(&init_uts_ns),
	.ns.__ns_ref = REFCOUNT_INIT(2),
	.name = {
		.sysname	= "Linux",
		.nodename	= "(none)",
		.release	= "6.18.62",
		.version	= "#1",
		.machine	= "mikros",
		.domainname	= "(none)",
	},
	.user_ns = &init_user_ns,
	.ns.inum = ns_init_inum(&init_uts_ns),
};

/* The program's read-only segments, which its ELF header that the linker
 * names __ehdr_start leads to. */
extern const struct elf64_hdr __ehdr_start;
#define RODATA_MAX	4
static struct {
	unsigned long start, end;
} rodata[RODATA_MAX];
static unsigned int rodata_count;

bool mikros_is_rodata(unsigned long addr)
{
	unsigned int i;

	for (i = 0; i < rodata_count; i++)
		if (addr >= rodata[i].start && addr < rodata[i].end)
			return true;
	return false;
}

static void find_rodata(void)
{
	const struct elf64_phdr *phdr = (const void *)&__ehdr_start + __ehdr_start.e_phoff;
	unsigned long bias = 0;
	unsigned int i;

	for (i = 0; i < __ehdr_start.e_phnum; i++)
		if (phdr[i].p_type == PT_LOAD && !phdr[i].p_offset)
			bias = (unsigned long)&__ehdr_start - phdr[i].p_vaddr;
	for (i = 0; i < __ehdr_start.e_phnum && rodata_count < RODATA_MAX; i++) {
		if (phdr[i].p_type != PT_LOAD || (phdr[i].p_flags & PF_W))
			continue;
		rodata[rodata_count].start = bias + phdr[i].p_vaddr;
		rodata[rodata_count].end = bias + phdr[i].p_vaddr + phdr[i].p_memsz;
		rodata_count++;
	}
}

const struct trace_print_flags gfpflag_names[] = { { 0, NULL } };
const struct trace_print_flags pageflag_names[] = { { 0, NULL } };
const struct trace_print_flags vmaflag_names[] = { { 0, NULL } };

void mikros_log(const char *fmt, ...)
{
	char line[512];
	va_list args;
	int len;

	va_start(args, fmt);
	len = vscnprintf(line, sizeof(line), fmt, args);
	va_end(args);
	mikros_host_log(line, len);
}

/* Logs each line of a message, without the level of its start, which
 * may come from the format or from an argument. */
static int log_message(const char *fmt, va_list args)
{
	char text[1024];
	const char *line, *end, *stop;
	int len;

	len = vscnprintf(text, sizeof(text), fmt, args);
	stop = text + len;
	for (line = printk_skip_headers(text); line < stop; line = end + 1) {
		end = memchr(line, '\n', stop - line);
		if (!end)
			end = stop;
		if (end > line)
			mikros_host_log(line, end - line);
	}
	return len;
}

int _printk(const char *fmt, ...)
{
	va_list args;
	int len;

	va_start(args, fmt);
	len = log_message(fmt, args);
	va_end(args);
	return len;
}

int _printk_deferred(const char *fmt, ...)
{
	va_list args;
	int len;

	va_start(args, fmt);
	len = log_message(fmt, args);
	va_end(args);
	return len;
}

void panic(const char *fmt, ...)
{
	char text[512];
	va_list args;
	int len;

	va_start(args, fmt);
	len = vscnprintf(text, sizeof(text), fmt, args);
	va_end(args);
	mikros_host_abort(text, len);
}

void warn_slowpath_fmt(const char *file, int line, unsigned int taint,
		       const char *fmt, ...)
{
	va_list args;

	mikros_log("WARNING: at %s:%d", file, line);
	if (fmt) {
		va_start(args, fmt);
		log_message(fmt, args);
		va_end(args);
	}
}

void dump_stack_lvl(const char *log_lvl)
{
	mikros_log("(the shim keeps no stack traces)");
}

void dump_stack(void)
{
	dump_stack_lvl(KERN_DEFAULT);
}

void add_taint(unsigned flag, enum lockdep_ok lockdep_ok)
{
	mikros_log("tainted: %u", flag);
}

void dump_page(const struct page *page, const char *reason)
{
	mikros_log("page %px: flags %#lx, %s", page, page->flags.f, reason);
}

void __copy_overflow(int size, unsigned long count)
{
	WARN(1, "Buffer overflow detected (%d < %lu)!\n", size, count);
}

void get_random_bytes(void *buf, size_t len)
{
	mikros_host_random(buf, len);
}

u64 get_random_u64(void)
{
	u64 r;

	mikros_host_random(&r, sizeof(r));
	return r;
}

u32 __get_random_u32_below(u32 ceil)
{
	u64 r;

	mikros_host_random(&r, sizeof(r));
	return (u32)(((r & 0xffffffff) * ceil) >> 32);
}

int __cold execute_with_initialized_rng(struct notifier_block *nb)
{
	nb->notifier_call(nb, 0, NULL);
	return 0;
}

/* The initcalls of each level, in the sections that xtask named so. Each
 * level has a NULL entry of the shim's, so that its section exists, and
 * the linker keeps it for the references to its start and stop. */
#define LEVEL(n)							\
	extern initcall_t __start_mikros_initcall##n[];			\
	extern initcall_t __stop_mikros_initcall##n[];			\
	static initcall_t __used __section("mikros_initcall" #n)	\
		mikros_initcall_none##n;
LEVEL(_early) LEVEL(0) LEVEL(1) LEVEL(2) LEVEL(3) LEVEL(4) LEVEL(5)
LEVEL(6) LEVEL(7)
#undef LEVEL

static int run_initcalls(initcall_t *start, initcall_t *stop)
{
	initcall_t *call;
	int ret;

	for (call = start; call < stop; call++) {
		if (!*call)
			continue;
		ret = (*call)();
		if (ret) {
			mikros_log("initcall %ps failed: %d", *call, ret);
			return ret;
		}
	}
	return 0;
}

/* Sets the shim up, on the calling thread as its first task while the
 * call lasts, and runs the initcalls. 0, or the first initcall's error. */
int mikros_btrfs_start(void)
{
	static const struct {
		initcall_t *start, *stop;
	} levels[] = {
		{ __start_mikros_initcall_early, __stop_mikros_initcall_early },
		{ __start_mikros_initcall0, __stop_mikros_initcall0 },
		{ __start_mikros_initcall1, __stop_mikros_initcall1 },
		{ __start_mikros_initcall2, __stop_mikros_initcall2 },
		{ __start_mikros_initcall3, __stop_mikros_initcall3 },
		{ __start_mikros_initcall4, __stop_mikros_initcall4 },
		{ __start_mikros_initcall5, __stop_mikros_initcall5 },
		{ __start_mikros_initcall6, __stop_mikros_initcall6 },
		{ __start_mikros_initcall7, __stop_mikros_initcall7 },
	};
	unsigned int cpu, i;
	int ret;

	for (cpu = 0; cpu < NR_CPUS; cpu++) {
		cpumask_set_cpu(cpu, &__cpu_possible_mask);
		cpumask_set_cpu(cpu, &__cpu_online_mask);
		cpumask_set_cpu(cpu, &__cpu_present_mask);
		cpumask_set_cpu(cpu, &__cpu_active_mask);
	}
	find_rodata();
	mikros_mm_init();
	mikros_percpu_init();
	mikros_node_init();
	/* What start_kernel() sets up before the initcalls. */
	radix_tree_init();
	maple_tree_init();
	mikros_sched_init();
	mikros_time_init();
	mikros_rcu_init();
	mikros_workqueue_init();
	mikros_vfs_init();
	mikros_block_init();
	mm_percpu_wq = alloc_workqueue("mm_percpu_wq", WQ_MEM_RECLAIM, 0);
	if (!mm_percpu_wq)
		panic("no memory for mm_percpu_wq");
	/* Work for a CPU runs on a worker's own, so nothing may wait in a
	 * CPU's batches for such work to move it: folios go to the LRU
	 * lists as they come. */
	lru_cache_disable();
	for (i = 0; i < ARRAY_SIZE(levels); i++) {
		ret = run_initcalls(levels[i].start, levels[i].stop);
		if (ret)
			goto out;
	}
	/* Nor may counters wait in a CPU's share for such work to fold
	 * them, which mm/vmstat.c's late initcall lets them. */
	mikros_node_exact();
	system_state = SYSTEM_RUNNING;
out:
	/* The thread goes back to the program. */
	mikros_task_exit();
	return ret;
}
