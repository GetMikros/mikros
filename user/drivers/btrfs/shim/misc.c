// SPDX-License-Identifier: GPL-2.0
/*
 * What the code built on the shim asks for but the program does not use:
 * CPU hotplug, SRCU notifier chains, uevents, the btrfs control device,
 * managed device resources, firmware nodes and RAID6 recovery, which
 * MIKSPEC-0053.R3 leaves out. Each does as little as its callers allow.
 */
#include <linux/cpuhotplug.h>
#include <linux/cpumask.h>
#include <linux/srcu.h>
#include <linux/kobject.h>
#include <linux/miscdevice.h>
#include <linux/device.h>
#include <linux/property.h>
#include <linux/raid/pq.h>
#include <linux/fs.h>
#include <linux/blkdev.h>
#include <linux/slab.h>
#include "shim.h"

int __cpuhp_setup_state(enum cpuhp_state state, const char *name, bool invoke,
			int (*startup)(unsigned int cpu), int (*teardown)(unsigned int cpu),
			bool multi_instance)
{
	unsigned int cpu;

	if (invoke && startup)
		for_each_possible_cpu(cpu)
			startup(cpu);
	return 0;
}

int __cpuhp_state_add_instance(enum cpuhp_state state, struct hlist_node *node, bool invoke)
{
	return 0;
}

int __cpuhp_state_remove_instance(enum cpuhp_state state, struct hlist_node *node, bool invoke)
{
	return 0;
}

int init_srcu_struct(struct srcu_struct *ssp)
{
	return 0;
}

int __srcu_read_lock(struct srcu_struct *ssp)
{
	rcu_read_lock();
	return 0;
}

void __srcu_read_unlock(struct srcu_struct *ssp, int idx)
{
	rcu_read_unlock();
}

void synchronize_srcu(struct srcu_struct *ssp)
{
	synchronize_rcu();
}

int kobject_uevent(struct kobject *kobj, enum kobject_action action)
{
	return 0;
}

int kobject_uevent_env(struct kobject *kobj, enum kobject_action action, char *envp[])
{
	return 0;
}

/* /dev/btrfs-control: there is no /dev to have it in. */
int misc_register(struct miscdevice *misc)
{
	return 0;
}

void misc_deregister(struct miscdevice *misc)
{
}

void *__devres_alloc_node(dr_release_t release, size_t size, gfp_t gfp, int nid,
			  const char *name)
{
	return kzalloc(size, gfp);
}

void devres_add(struct device *dev, void *res)
{
}

void devres_free(void *res)
{
	kfree(res);
}

int __devm_add_action(struct device *dev, void (*action)(void *), void *data, const char *name)
{
	return 0;
}

unsigned int fwnode_count_parents(const struct fwnode_handle *fwnode)
{
	return 0;
}

const char *fwnode_get_name(const struct fwnode_handle *fwnode)
{
	return NULL;
}

const char *fwnode_get_name_prefix(const struct fwnode_handle *fwnode)
{
	return NULL;
}

struct fwnode_handle *fwnode_get_nth_parent(struct fwnode_handle *fwnode, unsigned int depth)
{
	return NULL;
}

static void no_raid6(void)
{
	panic("RAID5 and RAID6 profiles are not supported");
}

static void no_gen_syndrome(int disks, size_t bytes, void **ptrs)
{
	no_raid6();
}

static void no_xor_syndrome(int disks, int start, int stop, size_t bytes, void **ptrs)
{
	no_raid6();
}

static void no_2data_recov(int disks, size_t bytes, int faila, int failb, void **ptrs)
{
	no_raid6();
}

static void no_datap_recov(int disks, size_t bytes, int faila, void **ptrs)
{
	no_raid6();
}

struct raid6_calls raid6_call = {
	.gen_syndrome = no_gen_syndrome,
	.xor_syndrome = no_xor_syndrome,
	.name = "none",
};
void (*raid6_2data_recov)(int, size_t, int, int, void **) = no_2data_recov;
void (*raid6_datap_recov)(int, size_t, int, void **) = no_datap_recov;
