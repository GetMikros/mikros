// SPDX-License-Identifier: BSD-3-Clause
/*
 * Entering S5, resetting the machine through the FADT's reset register,
 * the power button: the fixed event and power button devices'
 * notifications, and the operation regions held before power-off.
 */

#include "glue.h"
#include "acdispat.h"
#include "acinterp.h"
#include "acnamesp.h"

/* The notification of a power button device that it was pressed. */
#define BUTTON_PRESSED  0x80

/* The address spaces of power registers, as mikros_acpi_power_register
 * gives them. */
#define MIKROS_ACPI_SPACE_MEMORY    0
#define MIKROS_ACPI_SPACE_IO        1

/* Runs the preparation for sleep state S5, which evaluates \_PTS.
 * Returns an ACPI_STATUS. */
UINT32 mikros_acpi_prepare_s5(void)
{
    return AcpiEnterSleepStatePrep(ACPI_STATE_S5);
}

/* Enters sleep state S5, which does not return when it works. Returns
 * an ACPI_STATUS. */
UINT32 mikros_acpi_enter_s5(void)
{
    return AcpiEnterSleepState(ACPI_STATE_S5);
}

/* Whether the FADT says that its reset register resets the machine. */
UINT32 mikros_acpi_reset_supported(void)
{
    return (AcpiGbl_FADT.Flags & ACPI_FADT_RESET_REGISTER) != 0;
}

/* Writes the FADT's reset value to its reset register. Returns an
 * ACPI_STATUS. */
UINT32 mikros_acpi_reset(void)
{
    return AcpiReset();
}

static void (*pressed)(void *context);
static void *pressed_context;

static UINT32 fixed_power_button(void *context)
{
    (void)context;
    pressed(pressed_context);
    return ACPI_INTERRUPT_HANDLED;
}

static void power_button_notified(ACPI_HANDLE device, UINT32 value, void *context)
{
    (void)device;
    (void)context;
    if (value == BUTTON_PRESSED) {
        pressed(pressed_context);
    }
}

static ACPI_STATUS watch_button(ACPI_HANDLE handle, UINT32 level, void *context, void **ret)
{
    (void)level;
    (void)context;
    (void)ret;
    AcpiInstallNotifyHandler(handle, ACPI_DEVICE_NOTIFY, power_button_notified, NULL);
    return AE_OK;
}

/* Calls `on_press(context)` each time the fixed power button event or a
 * power button device's notification comes. Returns an ACPI_STATUS. */
UINT32 mikros_acpi_watch_power_button(void (*on_press)(void *context), void *context)
{
    pressed = on_press;
    pressed_context = context;
    ACPI_STATUS status = AcpiInstallFixedEventHandler(ACPI_EVENT_POWER_BUTTON,
                                                      fixed_power_button, NULL);
    if (ACPI_SUCCESS(status)) {
        status = AcpiEnableEvent(ACPI_EVENT_POWER_BUTTON, 0);
    }
    if (ACPI_FAILURE(status) && status != AE_NOT_EXIST && status != AE_SUPPORT) {
        return status;
    }
    return AcpiGetDevices("PNP0C0C", watch_button, NULL, NULL);
}

/* The power register `index`: 0 the PM1a control register, 1 the PM1b
 * control register and 2 the reset register, with its address space, a
 * MIKROS_ACPI_SPACE_ value, its address and its size in bytes. Returns
 * AE_NOT_EXIST when the FADT has no such register, the reset register
 * only when the FADT reports it, or one in another address space. */
UINT32 mikros_acpi_power_register(UINT32 index, UINT32 *space, UINT64 *address, UINT32 *bytes)
{
    ACPI_GENERIC_ADDRESS *registers[] = {
        &AcpiGbl_FADT.XPm1aControlBlock,
        &AcpiGbl_FADT.XPm1bControlBlock,
        (AcpiGbl_FADT.Flags & ACPI_FADT_RESET_REGISTER) ? &AcpiGbl_FADT.ResetRegister : NULL,
    };
    if (index >= ACPI_ARRAY_LENGTH(registers)) {
        return AE_BAD_PARAMETER;
    }
    ACPI_GENERIC_ADDRESS *r = registers[index];
    if (!r || !r->Address) {
        return AE_NOT_EXIST;
    }
    switch (r->SpaceId) {
    case ACPI_ADR_SPACE_SYSTEM_MEMORY:
        *space = MIKROS_ACPI_SPACE_MEMORY;
        break;
    case ACPI_ADR_SPACE_SYSTEM_IO:
        *space = MIKROS_ACPI_SPACE_IO;
        break;
    default:
        return AE_NOT_EXIST;
    }
    *address = r->Address;
    *bytes = r->BitWidth > 8 ? (UINT32)(r->BitWidth + 7) / 8 : 1;
    return AE_OK;
}

struct region_visit {
    void (*found)(UINT32 space, UINT64 address, UINT64 length, void *context);
    void *context;
};

/* Reports the operation region `handle`, with its address and length,
 * which are evaluated first, as when a method first uses it, when they
 * have not been yet. One whose address cannot be evaluated is left out. */
static ACPI_STATUS visit_region(ACPI_HANDLE handle, UINT32 level, void *context, void **ret)
{
    (void)level;
    (void)ret;
    struct region_visit *visit = context;
    ACPI_OPERAND_OBJECT *region = AcpiNsGetAttachedObject((ACPI_NAMESPACE_NODE *)handle);
    if (!region || region->Common.Type != ACPI_TYPE_REGION) {
        return AE_OK;
    }
    if (!(region->Common.Flags & AOPOBJ_DATA_VALID)) {
        AcpiExEnterInterpreter();
        ACPI_STATUS status = AcpiDsGetRegionArguments(region);
        AcpiExExitInterpreter();
        if (ACPI_FAILURE(status)) {
            return AE_OK;
        }
    }
    visit->found(region->Region.SpaceId, region->Region.Address, region->Region.Length,
                 visit->context);
    return AE_OK;
}

/* Calls `found` with the address space, address and length of each
 * operation region in the namespace. Returns an ACPI_STATUS. */
UINT32 mikros_acpi_operation_regions(
    void (*found)(UINT32 space, UINT64 address, UINT64 length, void *context), void *context)
{
    struct region_visit visit = {found, context};
    return AcpiWalkNamespace(ACPI_TYPE_REGION, ACPI_ROOT_OBJECT, ACPI_UINT32_MAX, visit_region,
                             NULL, &visit, NULL);
}
