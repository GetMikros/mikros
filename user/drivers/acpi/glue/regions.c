// SPDX-License-Identifier: BSD-3-Clause
/*
 * The handlers of the SystemMemory and SystemIO operation regions. Each
 * asks the operating system layer for the resource of an access before
 * it makes it: memory is mapped here, a page or two at a time, and ports
 * are left to ACPICA's own handler. When the resource is refused, the
 * access fails with AE_ACCESS and the region is logged the first time
 * (MIKSPEC-0038.R24).
 *
 * PCI_Config regions keep ACPICA's handler, which installs itself again
 * on each PCI root bridge that holds such a region.
 *
 * The handlers are marked as ACPICA's default ones, as those they
 * replace were: ACPICA then runs the _REG methods of their regions
 * after loading the tables, deactivates a region that is deleted, which
 * frees its context and gives back its memory, and keeps the interpreter
 * locked while a handler runs, so that the accesses of a region never
 * overlap.
 */

#include "glue.h"
#include "acevents.h"
#include "acinterp.h"
#include "acnamesp.h"

/* The resources of accesses, from the operating system layer: 0 when it
 * holds them, the status that refused them otherwise. A memory access
 * maps its pages, and gives in `at` where its first byte is, until
 * AcpiOsUnmapMemory. */
INT32 mikros_acpi_hold_memory(UINT64 address, UINT64 length, UINT64 *at);
INT32 mikros_acpi_hold_ports(UINT64 port, UINT64 count);

/* Logs the operation region `path`, of `space`, at `address` and `length`
 * long, whose resource `status` refused. */
void mikros_acpi_region_refused(const char *path, UINT32 space, UINT64 address, UINT64 length,
                                INT32 status);

/* Pages of a memory region that are mapped, from `start` to `end`, at
 * `at`. */
struct pages {
    UINT64 start;
    UINT64 end;
    UINT8 *at;
    struct pages *next;
};

/* The context of a region. */
struct region {
    ACPI_OPERAND_OBJECT *object;
    /* The context of ACPICA's setup, for SystemIO, whose accesses
     * ACPICA's handler makes. */
    void *inner;
    struct pages *mapped;
    BOOLEAN logged;
};

/* Fails an access of `region` whose resource `status` refused, logging
 * the region the first time. */
static ACPI_STATUS refuse(struct region *region, INT32 status)
{
    if (!region->logged) {
        region->logged = TRUE;
        ACPI_OPERAND_OBJECT *object = region->object;
        ACPI_NAMESPACE_NODE *node = object->Region.Node;
        UINT32 size = AcpiNsBuildNormalizedPath(node, NULL, 0, TRUE);
        char *path = ACPI_ALLOCATE_ZEROED(size);
        if (path) {
            AcpiNsBuildNormalizedPath(node, path, size, TRUE);
        }
        mikros_acpi_region_refused(path ? path : AcpiUtGetNodeName(node),
                                   object->Region.SpaceId, object->Region.Address,
                                   object->Region.Length, status);
        ACPI_FREE(path);
    }
    return AE_ACCESS;
}

/* Activates a region with ACPICA's setup `inner`, when there is one, or
 * deactivates it. */
static ACPI_STATUS setup(ACPI_ADR_SPACE_SETUP inner, ACPI_HANDLE handle, UINT32 function,
                         void *handler_context, void **region_context)
{
    if (function == ACPI_REGION_DEACTIVATE) {
        struct region *region = *region_context;
        ACPI_STATUS status = AE_OK;
        if (region) {
            if (inner) {
                status = inner(handle, function, handler_context, &region->inner);
            }
            while (region->mapped) {
                struct pages *pages = region->mapped;
                region->mapped = pages->next;
                AcpiOsUnmapMemory(pages->at, pages->end - pages->start);
                ACPI_FREE(pages);
            }
            ACPI_FREE(region);
        }
        *region_context = NULL;
        return status;
    }
    struct region *region = ACPI_ALLOCATE_ZEROED(sizeof(*region));
    if (!region) {
        return AE_NO_MEMORY;
    }
    region->object = handle;
    if (inner) {
        ACPI_STATUS status = inner(handle, function, handler_context, &region->inner);
        if (ACPI_FAILURE(status)) {
            ACPI_FREE(region);
            return status;
        }
    }
    *region_context = region;
    return AE_OK;
}

static ACPI_STATUS memory_setup(ACPI_HANDLE handle, UINT32 function, void *handler_context,
                                void **region_context)
{
    return setup(NULL, handle, function, handler_context, region_context);
}

static ACPI_STATUS io_setup(ACPI_HANDLE handle, UINT32 function, void *handler_context,
                            void **region_context)
{
    return setup(AcpiEvIoSpaceRegionSetup, handle, function, handler_context, region_context);
}

/* Where the `bytes` at `address` of `region` are mapped, mapping their
 * pages unless they are already: NULL, with `refused` set, when their
 * resource was refused. */
static UINT8 *memory_at(struct region *region, UINT64 address, UINT32 bytes, ACPI_STATUS *refused)
{
    struct pages *pages;
    for (pages = region->mapped; pages; pages = pages->next) {
        if (pages->start <= address && address + bytes <= pages->end) {
            return pages->at + (address - pages->start);
        }
    }
    UINT64 at;
    INT32 status = mikros_acpi_hold_memory(address, bytes, &at);
    if (status) {
        *refused = refuse(region, status);
        return NULL;
    }
    pages = ACPI_ALLOCATE_ZEROED(sizeof(*pages));
    if (!pages) {
        AcpiOsUnmapMemory(ACPI_TO_POINTER(at), bytes);
        *refused = AE_NO_MEMORY;
        return NULL;
    }
    pages->start = ACPI_ROUND_DOWN(address, ACPI_DEFAULT_PAGE_SIZE);
    pages->end = ACPI_ROUND_UP(address + bytes, ACPI_DEFAULT_PAGE_SIZE);
    pages->at = (UINT8 *)ACPI_TO_POINTER(at) - (address - pages->start);
    pages->next = region->mapped;
    region->mapped = pages;
    return (UINT8 *)ACPI_TO_POINTER(at);
}

static ACPI_STATUS memory_handler(UINT32 function, ACPI_PHYSICAL_ADDRESS address, UINT32 width,
                                  UINT64 *value, void *handler_context, void *region_context)
{
    (void)handler_context;
    if (width != 8 && width != 16 && width != 32 && width != 64) {
        return AE_AML_OPERAND_VALUE;
    }
    UINT32 bytes = width / 8;
    if (address + bytes < address) {
        return AE_AML_ILLEGAL_ADDRESS;
    }
    ACPI_STATUS failed = AE_OK;
    UINT8 *at = memory_at(region_context, address, bytes, &failed);
    if (!at) {
        return failed;
    }
    switch (function) {
    case ACPI_READ:
        switch (width) {
        case 8:
            *value = *(volatile UINT8 *)at;
            break;
        case 16:
            *value = *(volatile UINT16 *)at;
            break;
        case 32:
            *value = *(volatile UINT32 *)at;
            break;
        default:
            *value = *(volatile UINT64 *)at;
            break;
        }
        return AE_OK;
    case ACPI_WRITE:
        switch (width) {
        case 8:
            *(volatile UINT8 *)at = (UINT8)*value;
            break;
        case 16:
            *(volatile UINT16 *)at = (UINT16)*value;
            break;
        case 32:
            *(volatile UINT32 *)at = (UINT32)*value;
            break;
        default:
            *(volatile UINT64 *)at = *value;
            break;
        }
        return AE_OK;
    default:
        return AE_BAD_PARAMETER;
    }
}

static ACPI_STATUS io_handler(UINT32 function, ACPI_PHYSICAL_ADDRESS address, UINT32 width,
                              UINT64 *value, void *handler_context, void *region_context)
{
    struct region *region = region_context;
    /* Other widths and ports past the last are ACPICA's to refuse. */
    if ((width == 8 || width == 16 || width == 32) && address + width / 8 <= 0x10000) {
        INT32 status = mikros_acpi_hold_ports(address, width / 8);
        if (status) {
            return refuse(region, status);
        }
    }
    return AcpiExSystemIoSpaceHandler(function, address, width, value, handler_context,
                                      region->inner);
}

/* Installs `handler` and `setup` for `space` on the namespace's root, as
 * a default handler. */
static ACPI_STATUS install(ACPI_ADR_SPACE_TYPE space, ACPI_ADR_SPACE_HANDLER handler,
                           ACPI_ADR_SPACE_SETUP setup)
{
    ACPI_STATUS status = AcpiInstallAddressSpaceHandler(ACPI_ROOT_OBJECT, space, handler, setup,
                                                        NULL);
    if (ACPI_FAILURE(status)) {
        return status;
    }
    ACPI_OPERAND_OBJECT *root = AcpiNsGetAttachedObject(AcpiGbl_RootNode);
    ACPI_OPERAND_OBJECT *installed =
        root ? AcpiEvFindRegionHandler(space, root->CommonNotify.Handler) : NULL;
    if (!installed) {
        return AE_NOT_EXIST;
    }
    installed->AddressSpace.HandlerFlags |= ACPI_ADDR_HANDLER_DEFAULT_INSTALLED;
    return AE_OK;
}

ACPI_STATUS glue_install_region_handlers(void)
{
    ACPI_STATUS status = install(ACPI_ADR_SPACE_SYSTEM_MEMORY, memory_handler, memory_setup);
    if (ACPI_SUCCESS(status)) {
        status = install(ACPI_ADR_SPACE_SYSTEM_IO, io_handler, io_setup);
    }
    return status;
}
