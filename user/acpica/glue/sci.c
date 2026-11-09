// SPDX-License-Identifier: BSD-3-Clause
/*
 * The system control interrupt: its number in the FADT, and the GSI,
 * trigger mode and polarity that an interrupt source override of the
 * MADT gives it.
 */

#include "glue.h"

/* The SCI's number in the FADT, and its GSI and flags, MIKROS_ACPI_LEVEL
 * and MIKROS_ACPI_ACTIVE_LOW: level-triggered and active low unless the
 * MADT overrides the number's trigger mode or polarity. Returns an
 * ACPI_STATUS. */
UINT32 mikros_acpi_sci(UINT32 *number, UINT32 *gsi, UINT32 *flags)
{
    *number = AcpiGbl_FADT.SciInterrupt;
    *gsi = *number;
    *flags = MIKROS_ACPI_LEVEL | MIKROS_ACPI_ACTIVE_LOW;
    ACPI_TABLE_HEADER *table;
    if (ACPI_FAILURE(AcpiGetTable(ACPI_SIG_MADT, 1, &table))) {
        return AE_OK;
    }
    ACPI_SUBTABLE_HEADER *entry = ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, table,
                                               sizeof(ACPI_TABLE_MADT));
    ACPI_SUBTABLE_HEADER *end = ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, table, table->Length);
    while (entry + 1 <= end && entry->Length >= sizeof(*entry) &&
           ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, entry, entry->Length) <= end) {
        ACPI_MADT_INTERRUPT_OVERRIDE *source = (ACPI_MADT_INTERRUPT_OVERRIDE *)entry;
        if (entry->Type == ACPI_MADT_TYPE_INTERRUPT_OVERRIDE &&
            entry->Length >= sizeof(*source) && source->Bus == 0 &&
            source->SourceIrq == *number) {
            *gsi = source->GlobalIrq;
            UINT16 polarity = source->IntiFlags & ACPI_MADT_POLARITY_MASK;
            UINT16 trigger = source->IntiFlags & ACPI_MADT_TRIGGER_MASK;
            if (polarity == ACPI_MADT_POLARITY_ACTIVE_HIGH) {
                *flags &= ~MIKROS_ACPI_ACTIVE_LOW;
            }
            if (trigger == ACPI_MADT_TRIGGER_EDGE) {
                *flags &= ~MIKROS_ACPI_LEVEL;
            }
            break;
        }
        entry = ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, entry, entry->Length);
    }
    AcpiPutTable(table);
    return AE_OK;
}
