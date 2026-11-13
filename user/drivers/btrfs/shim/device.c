// SPDX-License-Identifier: GPL-2.0
/*
 * The device model, as far as the block layer and the backing devices of
 * the page cache use it: a device is a kobject with a name, which counts
 * its references and is released as the release's drivers/base/core.c
 * releases one. There is no sysfs to show devices in, nor any buses or
 * drivers to bind them to.
 */
#include <linux/device.h>
#include <linux/device/class.h>
#include <linux/kobject.h>
#include <linux/slab.h>
#include "shim.h"

static void device_release(struct kobject *kobj)
{
	struct device *dev = kobj_to_dev(kobj);

	if (dev->release)
		dev->release(dev);
	else if (dev->type && dev->type->release)
		dev->type->release(dev);
	else if (dev->class && dev->class->dev_release)
		dev->class->dev_release(dev);
	else
		WARN(1, "Device '%s' does not have a release() function\n", dev_name(dev));
}

static const struct kobj_type device_ktype = {
	.release = device_release,
};

int class_register(const struct class *cls)
{
	return 0;
}

void device_initialize(struct device *dev)
{
	kobject_init(&dev->kobj, &device_ktype);
	mutex_init(&dev->mutex);
	spin_lock_init(&dev->devres_lock);
	INIT_LIST_HEAD(&dev->devres_head);
	set_dev_node(dev, NUMA_NO_NODE);
}

int device_add(struct device *dev)
{
	int ret = 0;

	if (dev->init_name) {
		ret = kobject_set_name(&dev->kobj, "%s", dev->init_name);
		dev->init_name = NULL;
	}
	return ret;
}

void device_del(struct device *dev)
{
}

struct device *get_device(struct device *dev)
{
	return dev ? kobj_to_dev(kobject_get(&dev->kobj)) : NULL;
}

int dev_set_name(struct device *dev, const char *fmt, ...)
{
	va_list args;
	int ret;

	va_start(args, fmt);
	ret = kobject_set_name_vargs(&dev->kobj, fmt, args);
	va_end(args);
	return ret;
}

void put_device(struct device *dev)
{
	if (dev)
		kobject_put(&dev->kobj);
}

void device_unregister(struct device *dev)
{
	device_del(dev);
	put_device(dev);
}

static void device_create_release(struct device *dev)
{
	kfree(dev);
}

struct device *device_create(const struct class *cls, struct device *parent, dev_t devt,
			     void *drvdata, const char *fmt, ...)
{
	struct device *dev = kzalloc(sizeof(*dev), GFP_KERNEL);
	va_list args;
	int ret;

	if (!dev)
		return ERR_PTR(-ENOMEM);
	device_initialize(dev);
	dev->devt = devt;
	dev->class = cls;
	dev->parent = parent;
	dev->release = device_create_release;
	dev_set_drvdata(dev, drvdata);
	va_start(args, fmt);
	ret = kobject_set_name_vargs(&dev->kobj, fmt, args);
	va_end(args);
	if (ret) {
		put_device(dev);
		return ERR_PTR(ret);
	}
	return dev;
}

void _dev_warn(const struct device *dev, const char *fmt, ...)
{
	struct va_format vaf;
	va_list args;

	va_start(args, fmt);
	vaf.fmt = fmt;
	vaf.va = &args;
	printk(KERN_WARNING "%s: %pV", dev_name(dev), &vaf);
	va_end(args);
}
