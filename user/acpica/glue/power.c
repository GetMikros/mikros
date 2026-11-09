// SPDX-License-Identifier: BSD-3-Clause
/*
 * Entering S5, resetting the machine through the FADT's reset register,
 * and the power button: the fixed event and power button devices'
 * notifications.
 */

#include "glue.h"

/* The notification of a power button device that it was pressed. */
#define BUTTON_PRESSED  0x80

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
