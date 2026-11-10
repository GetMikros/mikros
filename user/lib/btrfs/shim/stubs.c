// SPDX-License-Identifier: GPL-2.0
/*
 * The kernel functions that the shim does not provide yet. Each ends the
 * program, naming itself, so that a run shows which come next. This file
 * includes no headers, as the stubs do not have the functions' types.
 */

void mikros_host_abort(const char *text, unsigned long len) __attribute__((noreturn));

#define STUB(name)							\
	void name(void);						\
	void name(void)							\
	{								\
		static const char text[] = "the shim has no " #name;	\
		mikros_host_abort(text, sizeof(text) - 1);		\
	}

STUB(__rq_qos_done_bio)
STUB(access_process_vm)
STUB(bdev_disk_changed)
STUB(bio_split_io_at)
STUB(blk_free_ext_minor)
STUB(disable_delayed_work_sync)
STUB(do_mmap)
STUB(enable_delayed_work)
STUB(filemap_fault)
STUB(filemap_map_pages)
STUB(get_user_pages_fast)
STUB(mmput)
STUB(pidfd_pid)
STUB(pidfd_prepare)
STUB(pidfs_add_pid)
STUB(pidfs_free_pid)
STUB(pidfs_remove_pid)
STUB(pin_user_pages_fast)
STUB(ptrace_may_access)
STUB(unpin_user_folio)
STUB(unpin_user_page)
STUB(vfs_fadvise)
