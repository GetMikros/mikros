// SPDX-License-Identifier: BSD-3-Clause
/*
 * Starting ACPICA on the firmware's tables, and its output, which it
 * formats with its own vsnprintf and the ACPI server logs.
 */

#include "glue.h"

/* Finds the firmware's tables and parses the FADT. Returns an
 * ACPI_STATUS. */
UINT32 mikros_acpi_tables(void)
{
    ACPI_STATUS status = AcpiInitializeSubsystem();
    if (ACPI_FAILURE(status)) {
        return status;
    }
    return AcpiInitializeTables(NULL, 16, TRUE);
}

/* Loads the tables into the namespace, enables the ACPI hardware mode and
 * the SCI, and initializes the namespace's objects. Returns an
 * ACPI_STATUS. */
UINT32 mikros_acpi_enable(void)
{
    ACPI_STATUS status = AcpiLoadTables();
    if (ACPI_FAILURE(status)) {
        return status;
    }
    status = AcpiEnableSubsystem(ACPI_FULL_INITIALIZATION);
    if (ACPI_FAILURE(status)) {
        return status;
    }
    return AcpiInitializeObjects(ACPI_FULL_INITIALIZATION);
}

/* Takes a piece of ACPICA's output: the server puts the pieces of a line
 * together before it logs them. */
void mikros_acpi_print(const char *text);

void ACPI_INTERNAL_VAR_XFACE AcpiOsPrintf(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    AcpiOsVprintf(format, arguments);
    va_end(arguments);
}

void AcpiOsVprintf(const char *format, va_list arguments)
{
    char text[256];
    vsnprintf(text, sizeof(text), format, arguments);
    mikros_acpi_print(text);
}
