// SPDX-License-Identifier: BSD-3-Clause
/*
 * The ECAM ranges of the MCFG, through which the operating system layer
 * reaches PCI configuration space.
 */

#include "glue.h"

/* The ECAM range `index` of the MCFG: its base, segment and first and
 * last bus. Returns an ACPI_STATUS, AE_NOT_FOUND past the last range or
 * without an MCFG. */
UINT32 mikros_acpi_ecam(UINT32 index, UINT64 *base, UINT16 *segment, UINT8 *first, UINT8 *last)
{
    ACPI_TABLE_HEADER *table;
    ACPI_STATUS status = AcpiGetTable(ACPI_SIG_MCFG, 1, &table);
    if (ACPI_FAILURE(status)) {
        return status;
    }
    UINT32 count = 0;
    if (table->Length >= sizeof(ACPI_TABLE_MCFG)) {
        count = (table->Length - sizeof(ACPI_TABLE_MCFG)) / sizeof(ACPI_MCFG_ALLOCATION);
    }
    if (index < count) {
        ACPI_MCFG_ALLOCATION *range =
            ACPI_ADD_PTR(ACPI_MCFG_ALLOCATION, table, sizeof(ACPI_TABLE_MCFG)) + index;
        *base = range->Address;
        *segment = range->PciSegment;
        *first = range->StartBusNumber;
        *last = range->EndBusNumber;
    } else {
        status = AE_NOT_FOUND;
    }
    AcpiPutTable(table);
    return status;
}
