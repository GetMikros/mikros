// SPDX-License-Identifier: BSD-3-Clause
/*
 * The routing of PCI interrupt pins: through the _PRT of the bridge above
 * a device, found among the namespace's bridges by their _ADR and the
 * secondary bus numbers of their configuration space, and through the
 * bridges above one without a _PRT, up to the PCI root bridge.
 */

#include "glue.h"

/* The most bridges from a root bridge down to a bus, and the most root
 * bridges of a segment. */
#define DEPTH_MAX   32
#define ROOTS_MAX   16

/* The configuration space registers of a header, and of a bridge's. */
#define HEADER_TYPE     0x0E
#define BUS_NUMBERS     0x18
#define TYPE_BRIDGE     1

/* Tells the firmware that interrupts are routed through I/O APICs, when
 * it has \_PIC. Returns an ACPI_STATUS. */
UINT32 mikros_acpi_use_apic(void)
{
    ACPI_OBJECT argument = { .Integer = { .Type = ACPI_TYPE_INTEGER, .Value = 1 } };
    ACPI_OBJECT_LIST arguments = { .Count = 1, .Pointer = &argument };
    ACPI_STATUS status = AcpiEvaluateObject(NULL, "\\_PIC", &arguments, NULL);
    return status == AE_NOT_FOUND ? AE_OK : status;
}

/* The GSI, trigger mode and polarity that the _PRT `table` gives the
 * interrupt pin `pin`, 0 for INTA, of the device `device`: AE_NOT_FOUND
 * when no entry applies. A link device's current resource gives its
 * interrupt. */
static ACPI_STATUS route_in(ACPI_PCI_ROUTING_TABLE *table, UINT8 device, UINT8 pin, UINT32 *gsi,
                            UINT32 *flags)
{
    for (; table->Length; table = ACPI_ADD_PTR(ACPI_PCI_ROUTING_TABLE, table, table->Length)) {
        if (ACPI_HIWORD(ACPI_LODWORD(table->Address)) != device || table->Pin != pin) {
            continue;
        }
        if (!table->Source[0]) {
            /* A fixed GSI, level-triggered and active low. */
            *gsi = table->SourceIndex;
            *flags = MIKROS_ACPI_LEVEL | MIKROS_ACPI_ACTIVE_LOW | MIKROS_ACPI_SHARED;
            return AE_OK;
        }
        ACPI_HANDLE link;
        if (ACPI_FAILURE(AcpiGetHandle(NULL, table->Source, &link))) {
            return AE_NOT_FOUND;
        }
        struct mikros_acpi_device current = { 0 };
        glue_current_resources(link, &current);
        for (UINT32 i = 0; i < current.resource_count; i++) {
            if (current.resources[i].kind == MIKROS_ACPI_IRQ) {
                *gsi = (UINT32)current.resources[i].base;
                *flags = current.resources[i].flags;
                return AE_OK;
            }
        }
        return AE_NOT_FOUND;
    }
    return AE_NOT_FOUND;
}

struct roots {
    UINT16 segment;
    ACPI_HANDLE handles[ROOTS_MAX];
    UINT8 buses[ROOTS_MAX];
    UINT32 count;
};

/* Collects the PCI root bridges of the segment, with their first buses. */
static ACPI_STATUS find_roots(ACPI_HANDLE handle, UINT32 level, void *context, void **ret)
{
    (void)level;
    (void)ret;
    struct roots *roots = context;
    ACPI_DEVICE_INFO *info;
    if (ACPI_FAILURE(AcpiGetObjectInfo(handle, &info))) {
        return AE_OK;
    }
    BOOLEAN root = (info->Valid & ACPI_VALID_HID) && glue_is_host_bridge(info->HardwareId.String);
    ACPI_FREE(info);
    if (!root) {
        return AE_OK;
    }
    UINT64 segment = 0, bus = 0;
    glue_integer(handle, METHOD_NAME__SEG, &segment);
    glue_integer(handle, METHOD_NAME__BBN, &bus);
    if (segment == roots->segment && roots->count < ROOTS_MAX) {
        roots->handles[roots->count] = handle;
        roots->buses[roots->count] = (UINT8)bus;
        roots->count++;
    }
    return AE_CTRL_DEPTH;
}

/* The bridges from a root bridge down to the one whose secondary bus is
 * the bus of the device, with the device number of each below the root. */
struct chain {
    ACPI_HANDLE bridges[DEPTH_MAX];
    UINT8 devices[DEPTH_MAX];
    UINT32 count;
};

/* The child of `parent`, on bus `bus` of `segment`, that is a PCI bridge
 * whose buses include `target`, with its device number and secondary
 * bus: NULL when no child is. */
static ACPI_HANDLE bridge_toward(ACPI_HANDLE parent, UINT16 segment, UINT8 bus, UINT8 target,
                                 UINT8 *device, UINT8 *secondary)
{
    ACPI_HANDLE child = NULL;
    while (ACPI_SUCCESS(AcpiGetNextObject(ACPI_TYPE_DEVICE, parent, child, &child))) {
        UINT64 address;
        if (!glue_integer(child, METHOD_NAME__ADR, &address)) {
            continue;
        }
        ACPI_PCI_ID id = {
            .Segment = segment,
            .Bus = bus,
            .Device = ACPI_HIWORD(ACPI_LODWORD(address)) & 0x1F,
            .Function = ACPI_LOWORD(ACPI_LODWORD(address)) & 0x7,
        };
        UINT64 type, buses;
        if (ACPI_FAILURE(AcpiOsReadPciConfiguration(&id, HEADER_TYPE, &type, 8)) ||
            (type & 0x7F) != TYPE_BRIDGE ||
            ACPI_FAILURE(AcpiOsReadPciConfiguration(&id, BUS_NUMBERS, &buses, 32))) {
            continue;
        }
        UINT8 first = (UINT8)(buses >> 8), last = (UINT8)(buses >> 16);
        if (first > bus && first <= target && target <= last) {
            *device = (UINT8)id.Device;
            *secondary = first;
            return child;
        }
    }
    return NULL;
}

/* Finds the bridges down to bus `bus` of `segment`: FALSE when no root
 * bridge's bridges reach it. */
static BOOLEAN find_chain(UINT16 segment, UINT8 bus, struct chain *chain)
{
    struct roots roots = { .segment = segment };
    AcpiWalkNamespace(ACPI_TYPE_DEVICE, ACPI_ROOT_OBJECT, ACPI_UINT32_MAX, find_roots, NULL,
                      &roots, NULL);
    for (UINT32 r = 0; r < roots.count; r++) {
        if (roots.buses[r] > bus) {
            continue;
        }
        chain->bridges[0] = roots.handles[r];
        chain->devices[0] = 0;
        chain->count = 1;
        UINT8 at = roots.buses[r];
        while (at != bus && chain->count < DEPTH_MAX) {
            UINT8 device, secondary;
            ACPI_HANDLE next = bridge_toward(chain->bridges[chain->count - 1], segment, at, bus,
                                             &device, &secondary);
            if (!next) {
                break;
            }
            chain->bridges[chain->count] = next;
            chain->devices[chain->count] = device;
            chain->count++;
            at = secondary;
        }
        if (at == bus) {
            return TRUE;
        }
    }
    return FALSE;
}

/* The GSI, trigger mode and polarity of interrupt pin `pin`, 1 for INTA,
 * of PCI device `device` on bus `bus` of `segment`. Returns an
 * ACPI_STATUS, AE_NOT_FOUND when no entry applies. */
UINT32 mikros_acpi_pci_route(UINT16 segment, UINT8 bus, UINT8 device, UINT8 pin, UINT32 *gsi,
                             UINT32 *flags)
{
    if (pin < 1 || pin > 4 || device > 31) {
        return AE_BAD_PARAMETER;
    }
    struct chain chain;
    if (!find_chain(segment, bus, &chain)) {
        return AE_NOT_FOUND;
    }
    for (UINT32 i = chain.count; i-- > 0;) {
        ACPI_BUFFER table = { .Length = ACPI_ALLOCATE_BUFFER };
        ACPI_STATUS status = AcpiGetIrqRoutingTable(chain.bridges[i], &table);
        if (ACPI_SUCCESS(status)) {
            status = route_in(table.Pointer, device, pin - 1, gsi, flags);
            ACPI_FREE(table.Pointer);
            return status;
        }
        if (status != AE_NOT_FOUND || i == 0) {
            return status;
        }
        /* The pin of the bridge that the device's pin is wired to. */
        pin = (UINT8)(((pin - 1 + device) % 4) + 1);
        device = chain.devices[i];
    }
    return AE_NOT_FOUND;
}
