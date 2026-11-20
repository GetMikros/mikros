// A system test in C++ of the bindings that midlc generates into the
// sysroot: the header of every library compiles as C++, and an arena
// takes memory. It exits with 0.

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

int main()
{
    static uint8_t memory[64];
    mikros_midl_arena_t arena;
    mikros_midl_arena_init(&arena, memory, sizeof memory);
    return mikros_midl_alloc(&arena, 8) == memory ? 0 : 1;
}
