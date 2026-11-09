// SPDX-License-Identifier: BSD-3-Clause
/*
 * Entering S5, resetting the machine through the FADT's reset register,
 * and the power button: the fixed event and power button devices'
 * notifications.
 */

#include "glue.h"

/* The notification of a power button device that it was pressed. */
#define BUTTON_PRESSED  0x80

/* The address spaces of power registers, as mikros_acpi_power_register
 * gives them. */
#define MIKROS_ACPI_SPACE_MEMORY    0
#define MIKROS_ACPI_SPACE_IO        1

/* Enters sleep state S5. Returns an ACPI_STATUS when it does not. */
UINT32 mikros_acpi_power_off(void)
{
    ACPI_STATUS status = AcpiEnterSleepStatePrep(ACPI_STATE_S5);
    if (ACPI_FAILURE(status)) {
        return status;
    }
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
