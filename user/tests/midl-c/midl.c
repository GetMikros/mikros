/* A system test in C of the bindings that midlc generates into the
 * sysroot, which selftest runs as it runs the others, without any
 * permission of its own. It includes the header of every library, lists
 * the instances of a device class through the process's directory, and
 * connects to the clock service, calling methods that return a number,
 * a string and a memory object, whose rights the bindings narrow to
 * those declared. Listing the clock, which has no named instances, and
 * connecting to a protocol that nothing serves return the directory's
 * errors. It exits with 0 when every check passed, and otherwise with
 * the number of the first check that failed. */

#include <mikros/midl/mikros.acpi.h>
#include <mikros/midl/mikros.base.h>
#include <mikros/midl/mikros.boot.h>
#include <mikros/midl/mikros.diag.h>
#include <mikros/midl/mikros.driver.h>
#include <mikros/midl/mikros.fs.btrfs.h>
#include <mikros/midl/mikros.fs.h>
#include <mikros/midl/mikros.gpu.h>
#include <mikros/midl/mikros.input.h>
#include <mikros/midl/mikros.log.h>
#include <mikros/midl/mikros.packages.h>
#include <mikros/midl/mikros.pci.h>
#include <mikros/midl/mikros.permissions.h>
#include <mikros/midl/mikros.process.h>
#include <mikros/midl/mikros.service.h>
#include <mikros/midl/mikros.session.h>
#include <mikros/midl/mikros.storage.h>
#include <mikros/midl/mikros.terminal.h>
#include <mikros/midl/mikros.time.h>
#include <mikros/midl/mikros.usb.h>
#include <mikros/startup.h>
#include <string.h>

/* Each call takes MIKROS_MIDL_CALL_ROOM of it, so the checks empty it
 * once they no longer need what a call decoded. */
static uint8_t memory[1 << 20];

static const mikros_midl_string_t CLOCK = {"mikros.time.Clock", 17};

int main(void)
{
    mikros_midl_arena_t arena;
    mikros_midl_arena_init(&arena, memory, sizeof memory);
    mikros_handle_t directory = mikros_directory();
    mikros_base_Status error = 0;

    /* A class's instances are listed, those that the caller may use among
     * them, and a protocol that a service provides has none. */
    mikros_service_Directory_List_Response listed;
    mikros_midl_string_t gpu = {"device.gpu", 10};
    mikros_midl_status_t s =
        mikros_service_Directory_List(directory, gpu, &listed, &error, INT64_MAX, &arena);
    if (s == MIKROS_MIDL_OK) {
        const mikros_midl_string_t *names = (const mikros_midl_string_t *)listed.instances.data;
        for (uint64_t i = 0; i < listed.instances.count; i++)
            if (names[i].data[names[i].size] != 0)
                return 11;
    } else if (s != MIKROS_MIDL_ERROR || error != mikros_base_Status_NOT_FOUND) {
        return 10;
    }
    mikros_midl_arena_reset(&arena);
    if (mikros_service_Directory_List(directory, CLOCK, &listed, &error, INT64_MAX, &arena) !=
            MIKROS_MIDL_ERROR ||
        error != mikros_base_Status_INVALID_ARGS)
        return 12;
    mikros_midl_arena_reset(&arena);

    mikros_handle_t clock, server;
    if (mikros_channel_create(0, &clock, &server) != 0)
        return 20;
    mikros_midl_string_t any = {NULL, 0};
    if (mikros_service_Directory_Connect(directory, CLOCK, any, server, &error, INT64_MAX,
                                         &arena) != MIKROS_MIDL_OK)
        return 21;
    mikros_midl_arena_reset(&arena);

    /* A number, and a string with a NUL after it. */
    mikros_time_Clock_GetUtc_Response utc;
    if (mikros_time_Clock_GetUtc(clock, &utc, INT64_MAX, &arena) != MIKROS_MIDL_OK || utc.utc <= 0)
        return 30;
    mikros_midl_arena_reset(&arena);
    mikros_time_Clock_GetTimezone_Response zone;
    if (mikros_time_Clock_GetTimezone(clock, &zone, INT64_MAX, &arena) != MIKROS_MIDL_OK)
        return 31;
    if (zone.name.data == NULL || strlen(zone.name.data) != zone.name.size)
        return 32;
    mikros_midl_arena_reset(&arena);

    /* A memory object with exactly its declared rights. */
    mikros_time_Clock_GetTimePage_Response page;
    if (mikros_time_Clock_GetTimePage(clock, &page, INT64_MAX, &arena) != MIKROS_MIDL_OK)
        return 40;
    uint8_t info[24];
    uint64_t len = 0;
    if (mikros_object_get_info(page.page, 1, info, sizeof info, &len) != 0 || len < 24)
        return 41;
    uint32_t kind, rights;
    memcpy(&kind, info + 16, 4);
    memcpy(&rights, info + 20, 4);
    if (kind != 3 || rights != (1u | 2u | 4u | 32u | 512u))
        return 42;
    mikros_handle_close(page.page);
    mikros_midl_arena_reset(&arena);

    /* An error of the method. */
    mikros_handle_t other, other_server;
    if (mikros_channel_create(0, &other, &other_server) != 0)
        return 50;
    mikros_midl_string_t nothing = {"mikros.nothing.Here", 19};
    if (mikros_service_Directory_Connect(directory, nothing, any, other_server, &error, INT64_MAX,
                                         &arena) != MIKROS_MIDL_ERROR)
        return 51;
    if (error != mikros_base_Status_NOT_FOUND)
        return 52;
    mikros_handle_close(other);
    mikros_handle_close(clock);
    return 0;
}
