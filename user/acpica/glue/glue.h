// SPDX-License-Identifier: BSD-3-Clause
/*
 * What the files of the ACPI server's glue share. The `mikros_acpi_`
 * functions are those Rust calls; the library keeps no other glue symbol
 * global.
 */

#ifndef MIKROS_ACPI_GLUE_H
#define MIKROS_ACPI_GLUE_H

#include "acpi.h"
#include "accommon.h"

/* The flags of an IRQ resource. */
#define MIKROS_ACPI_LEVEL       (1u << 0)
#define MIKROS_ACPI_ACTIVE_LOW  (1u << 1)
#define MIKROS_ACPI_SHARED      (1u << 2)

#endif
