// SPDX-License-Identifier: BSD-3-Clause
/*
 * The I/O ports, memory and interrupts of a device's _CRS, as the simple
 * resources that Rust reads, which hardware IDs name PCI host bridges,
 * and the integers of objects.
 */

#include "glue.h"

BOOLEAN glue_same(const char *a, const char *b)
{
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

BOOLEAN glue_integer(ACPI_HANDLE handle, const char *name, UINT64 *value)
{
    ACPI_OBJECT object;
    ACPI_BUFFER result = { .Length = sizeof(object), .Pointer = &object };
    if (ACPI_FAILURE(AcpiEvaluateObjectTyped(handle, (ACPI_STRING)name, NULL, &result,
                                             ACPI_TYPE_INTEGER))) {
        return FALSE;
    }
    *value = object.Integer.Value;
    return TRUE;
}

BOOLEAN glue_is_host_bridge(const char *id)
{
    return id && (glue_same(id, "PNP0A03") || glue_same(id, "PNP0A08"));
}

static void add_resource(struct mikros_acpi_device *device, UINT32 kind, UINT64 base,
                         UINT64 size, UINT32 flags)
{
    if (device->resource_count == MIKROS_ACPI_RESOURCES_MAX || size == 0) {
        return;
    }
    struct mikros_acpi_resource *r = &device->resources[device->resource_count++];
    r->kind = kind;
    r->base = base;
    r->size = size;
    r->flags = flags;
}

static UINT32 irq_flags(UINT8 triggering, UINT8 polarity, UINT8 shareable)
{
    UINT32 flags = 0;
    if (triggering == ACPI_LEVEL_SENSITIVE) {
        flags |= MIKROS_ACPI_LEVEL;
    }
    if (polarity == ACPI_ACTIVE_LOW) {
        flags |= MIKROS_ACPI_ACTIVE_LOW;
    }
    if (shareable == ACPI_SHARED) {
        flags |= MIKROS_ACPI_SHARED;
    }
    return flags;
}

/* Adds the I/O ports, memory and interrupts that `resource` names. */
static ACPI_STATUS take_resource(ACPI_RESOURCE *resource, void *context)
{
    struct mikros_acpi_device *device = context;
    ACPI_RESOURCE_DATA *d = &resource->Data;
    switch (resource->Type) {
    case ACPI_RESOURCE_TYPE_IO:
        add_resource(device, MIKROS_ACPI_IO, d->Io.Minimum, d->Io.AddressLength, 0);
        break;
    case ACPI_RESOURCE_TYPE_FIXED_IO:
        add_resource(device, MIKROS_ACPI_IO, d->FixedIo.Address, d->FixedIo.AddressLength, 0);
        break;
    case ACPI_RESOURCE_TYPE_MEMORY24:
        add_resource(device, MIKROS_ACPI_MEMORY, d->Memory24.Minimum,
                     d->Memory24.AddressLength, 0);
        break;
    case ACPI_RESOURCE_TYPE_MEMORY32:
        add_resource(device, MIKROS_ACPI_MEMORY, d->Memory32.Minimum,
                     d->Memory32.AddressLength, 0);
        break;
    case ACPI_RESOURCE_TYPE_FIXED_MEMORY32:
        add_resource(device, MIKROS_ACPI_MEMORY, d->FixedMemory32.Address,
                     d->FixedMemory32.AddressLength, 0);
        break;
    case ACPI_RESOURCE_TYPE_ADDRESS16:
    case ACPI_RESOURCE_TYPE_ADDRESS32:
    case ACPI_RESOURCE_TYPE_ADDRESS64: {
        ACPI_RESOURCE_ADDRESS64 address;
        if (ACPI_FAILURE(AcpiResourceToAddress64(resource, &address))) {
            break;
        }
        UINT32 kind = address.ResourceType == ACPI_MEMORY_RANGE ? MIKROS_ACPI_MEMORY
                    : address.ResourceType == ACPI_IO_RANGE ? MIKROS_ACPI_IO : 0;
        if (kind) {
            add_resource(device, kind, address.Address.Minimum,
                         address.Address.AddressLength, 0);
        }
        break;
    }
    case ACPI_RESOURCE_TYPE_IRQ:
        for (UINT8 i = 0; i < d->Irq.InterruptCount; i++) {
            add_resource(device, MIKROS_ACPI_IRQ, d->Irq.Interrupts[i], 1,
                         irq_flags(d->Irq.Triggering, d->Irq.Polarity, d->Irq.Shareable));
        }
        break;
    case ACPI_RESOURCE_TYPE_EXTENDED_IRQ:
        for (UINT8 i = 0; i < d->ExtendedIrq.InterruptCount; i++) {
            add_resource(device, MIKROS_ACPI_IRQ, d->ExtendedIrq.Interrupts[i], 1,
                         irq_flags(d->ExtendedIrq.Triggering, d->ExtendedIrq.Polarity,
                                   d->ExtendedIrq.Shareable));
        }
        break;
    default:
        break;
    }
    return AE_OK;
}

void glue_current_resources(ACPI_HANDLE handle, struct mikros_acpi_device *device)
{
    AcpiWalkResources(handle, METHOD_NAME__CRS, take_resource, device);
}
