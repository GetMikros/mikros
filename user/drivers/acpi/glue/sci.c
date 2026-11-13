// SPDX-License-Identifier: BSD-3-Clause
/*
 * The system control interrupt and the interrupts of ISA sources: the
 * FADT's SCI number, and the GSI, trigger mode and polarity that an
 * interrupt source override of the MADT gives an ISA IRQ.
 */

#include "glue.h"

/* The most interrupt source overrides that are kept. */
#define OVERRIDES_MAX   16

/* The MADT's interrupt source overrides for the ISA bus, which
 * mikros_acpi_sci reads once the tables are found. */
static struct {
    UINT8 source;
    UINT32 gsi;
    UINT16 flags;
} overrides[OVERRIDES_MAX];
static UINT32 override_count;

static void read_overrides(void)
{
    ACPI_TABLE_HEADER *table;
    override_count = 0;
    if (ACPI_FAILURE(AcpiGetTable(ACPI_SIG_MADT, 1, &table))) {
        return;
    }
    ACPI_SUBTABLE_HEADER *entry = ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, table,
                                               sizeof(ACPI_TABLE_MADT));
    ACPI_SUBTABLE_HEADER *end = ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, table, table->Length);
    while (entry + 1 <= end && entry->Length >= sizeof(*entry) &&
           ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, entry, entry->Length) <= end &&
           override_count < OVERRIDES_MAX) {
        ACPI_MADT_INTERRUPT_OVERRIDE *source = (ACPI_MADT_INTERRUPT_OVERRIDE *)entry;
        if (entry->Type == ACPI_MADT_TYPE_INTERRUPT_OVERRIDE &&
            entry->Length >= sizeof(*source) && source->Bus == 0) {
            overrides[override_count].source = source->SourceIrq;
            overrides[override_count].gsi = source->GlobalIrq;
            overrides[override_count].flags = source->IntiFlags;
            override_count++;
        }
        entry = ACPI_ADD_PTR(ACPI_SUBTABLE_HEADER, entry, entry->Length);
    }
    AcpiPutTable(table);
}

UINT32 glue_isa_irq(UINT8 irq, UINT32 *flags)
{
    for (UINT32 i = 0; i < override_count; i++) {
        if (overrides[i].source != irq) {
            continue;
        }
        UINT16 polarity = overrides[i].flags & ACPI_MADT_POLARITY_MASK;
        UINT16 trigger = overrides[i].flags & ACPI_MADT_TRIGGER_MASK;
        if (polarity == ACPI_MADT_POLARITY_ACTIVE_HIGH) {
            *flags &= ~MIKROS_ACPI_ACTIVE_LOW;
        } else if (polarity == ACPI_MADT_POLARITY_ACTIVE_LOW) {
            *flags |= MIKROS_ACPI_ACTIVE_LOW;
        }
        if (trigger == ACPI_MADT_TRIGGER_EDGE) {
            *flags &= ~MIKROS_ACPI_LEVEL;
        } else if (trigger == ACPI_MADT_TRIGGER_LEVEL) {
            *flags |= MIKROS_ACPI_LEVEL;
        }
        return overrides[i].gsi;
    }
    return irq;
}

/* Reads the MADT's interrupt source overrides, and gives the SCI's number
 * in the FADT, and its GSI and flags, MIKROS_ACPI_LEVEL and
 * MIKROS_ACPI_ACTIVE_LOW: level-triggered and active low unless the MADT
 * overrides the number's trigger mode or polarity. Returns an
 * ACPI_STATUS. */
UINT32 mikros_acpi_sci(UINT32 *number, UINT32 *gsi, UINT32 *flags)
{
    read_overrides();
    *number = AcpiGbl_FADT.SciInterrupt;
    *flags = MIKROS_ACPI_LEVEL | MIKROS_ACPI_ACTIVE_LOW;
    *gsi = *number <= ACPI_UINT8_MAX ? glue_isa_irq((UINT8)*number, flags) : *number;
    return AE_OK;
}
