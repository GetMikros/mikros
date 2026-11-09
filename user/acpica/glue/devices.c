// SPDX-License-Identifier: BSD-3-Clause
/*
 * The devices of the namespace that the ACPI server adds, with their
 * hardware IDs, paths and the resources of their _CRS.
 */

#include "glue.h"

/* The status that the _STA of `handle` gives, or present, enabled, shown
 * and functioning when it has none. */
static UINT32 status_of(ACPI_HANDLE handle)
{
    UINT64 status;
    if (!glue_integer(handle, METHOD_NAME__STA, &status)) {
        return ACPI_UINT32_MAX;
    }
    return (UINT32)status;
}

struct walk {
    void (*found)(void *context, const struct mikros_acpi_device *device);
    void *context;
};

/* Whether `info` names a PCI host bridge, by its _HID or a _CID. */
static BOOLEAN names_host_bridge(const ACPI_DEVICE_INFO *info)
{
    if ((info->Valid & ACPI_VALID_HID) && glue_is_host_bridge(info->HardwareId.String)) {
        return TRUE;
    }
    if (info->Valid & ACPI_VALID_CID) {
        for (UINT32 i = 0; i < info->CompatibleIdList.Count; i++) {
            if (glue_is_host_bridge(info->CompatibleIdList.Ids[i].String)) {
                return TRUE;
            }
        }
    }
    return FALSE;
}

/* Passes the device `handle` on when its _STA says that it is present and
 * enabled and it has a _HID, unless it is a PCI host bridge. The walk goes
 * on below host bridges, and below the objects of PCI functions, which
 * have an _ADR and no _HID, so that the devices of an LPC bridge are
 * found. */
static ACPI_STATUS visit(ACPI_HANDLE handle, UINT32 level, void *context, void **ret)
{
    (void)level;
    (void)ret;
    struct walk *walk = context;
    ACPI_DEVICE_INFO *info;
    if (ACPI_FAILURE(AcpiGetObjectInfo(handle, &info))) {
        return AE_OK;
    }
    if (names_host_bridge(info)) {
        ACPI_FREE(info);
        return AE_OK;
    }
    const char *hid = (info->Valid & ACPI_VALID_HID) ? info->HardwareId.String : NULL;
    UINT32 sta = status_of(handle);
    BOOLEAN present = (sta & ACPI_STA_DEVICE_PRESENT) && (sta & ACPI_STA_DEVICE_ENABLED);
    if (!present || !hid) {
        ACPI_FREE(info);
        /* The children of a device that is neither present nor
         * functioning are not there either. */
        if (!(sta & ACPI_STA_DEVICE_PRESENT) && !(sta & ACPI_STA_DEVICE_FUNCTIONING)) {
            return AE_CTRL_DEPTH;
        }
        return AE_OK;
    }
    char path[256];
    ACPI_BUFFER name = { .Length = sizeof(path), .Pointer = path };
    if (ACPI_FAILURE(AcpiGetName(handle, ACPI_FULL_PATHNAME_NO_TRAILING, &name))) {
        ACPI_FREE(info);
        return AE_OK;
    }
    struct mikros_acpi_device device = { .path = path, .hid = hid };
    if (info->Valid & ACPI_VALID_CID) {
        for (UINT32 i = 0; i < info->CompatibleIdList.Count && i < MIKROS_ACPI_CIDS_MAX; i++) {
            device.cids[device.cid_count++] = info->CompatibleIdList.Ids[i].String;
        }
    }
    glue_current_resources(handle, &device);
    walk->found(walk->context, &device);
    ACPI_FREE(info);
    return AE_OK;
}

/* Calls `found` with each device of the namespace that the ACPI server
 * adds; the device and its strings last for the call. Returns an
 * ACPI_STATUS. */
UINT32 mikros_acpi_devices(void (*found)(void *context, const struct mikros_acpi_device *device),
                           void *context)
{
    struct walk walk = { .found = found, .context = context };
    return AcpiWalkNamespace(ACPI_TYPE_DEVICE, ACPI_ROOT_OBJECT, ACPI_UINT32_MAX, visit, NULL,
                             &walk, NULL);
}
