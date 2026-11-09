// SPDX-License-Identifier: BSD-3-Clause
/*
 * What the files of the ACPI server's glue share: the simple types that
 * Rust reads devices and their resources as, and helpers of one file that
 * the others call. The `mikros_acpi_` functions are those Rust calls; the
 * library keeps no other glue symbol global.
 */

#ifndef MIKROS_ACPI_GLUE_H
#define MIKROS_ACPI_GLUE_H

#include "acpi.h"
#include "accommon.h"

/* The kinds of a device's resources. */
#define MIKROS_ACPI_IO      1
#define MIKROS_ACPI_MEMORY  2
#define MIKROS_ACPI_IRQ     3

/* The flags of an IRQ resource. */
#define MIKROS_ACPI_LEVEL       (1u << 0)
#define MIKROS_ACPI_ACTIVE_LOW  (1u << 1)
#define MIKROS_ACPI_SHARED      (1u << 2)

/* The most resources and compatible IDs of a device that are passed on. */
#define MIKROS_ACPI_RESOURCES_MAX   16
#define MIKROS_ACPI_CIDS_MAX        8

struct mikros_acpi_resource {
    UINT32 kind;
    UINT32 flags;
    UINT64 base;
    UINT64 size;
};

struct mikros_acpi_device {
    const char *path;
    const char *hid;
    const char *cids[MIKROS_ACPI_CIDS_MAX];
    UINT32 cid_count;
    struct mikros_acpi_resource resources[MIKROS_ACPI_RESOURCES_MAX];
    UINT32 resource_count;
};

/* Whether the strings `a` and `b` are the same. */
BOOLEAN glue_same(const char *a, const char *b);

/* Whether the hardware ID `id` names a PCI host bridge. */
BOOLEAN glue_is_host_bridge(const char *id);

/* The integer that the object `name` of `handle` evaluates to, in
 * `value`: FALSE when it has none. */
BOOLEAN glue_integer(ACPI_HANDLE handle, const char *name, UINT64 *value);

/* Adds the I/O ports, memory and interrupts of the _CRS of `handle` to
 * `device`. */
void glue_current_resources(ACPI_HANDLE handle, struct mikros_acpi_device *device);

#endif
