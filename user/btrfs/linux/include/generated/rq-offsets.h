/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The shim's run queue, struct rq, holds only nr_pinned, first, which
 * migrate_enable() and migrate_disable() count in.
 */
#ifndef __LINUX_RQ_OFFSETS_H__
#define __LINUX_RQ_OFFSETS_H__
#define RQ_nr_pinned 0
#endif
