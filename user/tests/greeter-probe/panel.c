/* The taskbar part of the shell probe (`--panel`): a panel along the
 * bottom edge of the output, 48 pixels thick, whose buffer is ARGB8888
 * and fully transparent and whose whole surface has the material haze,
 * so that the screen shows the compositor's haze there. Once the
 * compositor configured the panel and the buffer is committed, it serves
 * the connection until the compositor closes it. It exits with the
 * number of the step that failed, if one does. */

#define _GNU_SOURCE
#include <fcntl.h>
#include <mikros/startup.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "mikros-buffer-v1-client-protocol.h"
#include "mikros-material-v1-client-protocol.h"
#include "xdg-activation-v1-client-protocol.h"
#include "mikros-shell-v1-client-protocol.h"

enum {
	ROLE_USER = 16,
	WAYLAND_ID = 1,
	KIND_CHANNEL = 9,
	ARGB8888 = 0x34325241,
	BOTTOM = 2,
	THICKNESS = 48,
	HAZE = 1,
};

struct bar {
	struct wl_compositor *compositor;
	struct wl_output *output;
	struct mikros_buffer_v1 *buffers;
	struct mikros_shell_v1 *shell;
	struct mikros_material_v1 *materials;
	uint32_t serial, width, height;
	int configured;
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
		   const char *interface, uint32_t version)
{
	struct bar *b = data;
	(void)version;
	if (strcmp(interface, wl_compositor_interface.name) == 0)
		b->compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
	else if (strcmp(interface, wl_output_interface.name) == 0 && !b->output)
		b->output = wl_registry_bind(registry, name, &wl_output_interface, 3);
	else if (strcmp(interface, mikros_buffer_v1_interface.name) == 0)
		b->buffers = wl_registry_bind(registry, name, &mikros_buffer_v1_interface, 1);
	else if (strcmp(interface, mikros_shell_v1_interface.name) == 0)
		b->shell = wl_registry_bind(registry, name, &mikros_shell_v1_interface, 1);
	else if (strcmp(interface, mikros_material_v1_interface.name) == 0)
		b->materials = wl_registry_bind(registry, name, &mikros_material_v1_interface, 1);
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
	(void)data, (void)registry, (void)name;
}

static const struct wl_registry_listener registry_listener = {
	.global = global,
	.global_remove = global_remove,
};

static void configure(void *data, struct mikros_panel_v1 *panel, uint32_t serial,
		      uint32_t width, uint32_t height)
{
	struct bar *b = data;
	(void)panel;
	b->serial = serial;
	b->width = width;
	b->height = height;
	b->configured = 1;
}

static void closed(void *data, struct mikros_panel_v1 *panel)
{
	(void)data, (void)panel;
}

static const struct mikros_panel_v1_listener panel_listener = {
	.configure = configure,
	.closed = closed,
};

int panel(void)
{
	struct bar b = {0};
	mikros_handle_t channel = mikros_take_startup_handle(ROLE_USER, WAYLAND_ID);
	int fd = channel == 0 ? -1 : mikros_fd_from_handle(channel, KIND_CHANNEL);
	struct wl_display *display = fd < 0 ? NULL : wl_display_connect_to_fd(fd);
	if (!display)
		return 90;
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, &b);
	if (wl_display_roundtrip(display) < 0 || !b.compositor || !b.output || !b.buffers ||
	    !b.shell || !b.materials)
		return 91;

	struct wl_surface *surface = wl_compositor_create_surface(b.compositor);
	struct mikros_panel_v1 *panel =
		mikros_shell_v1_get_panel(b.shell, surface, b.output, BOTTOM);
	mikros_panel_v1_add_listener(panel, &panel_listener, &b);
	mikros_panel_v1_set_thickness(panel, THICKNESS);
	wl_surface_commit(surface);
	if (wl_display_roundtrip(display) < 0 || !b.configured || b.width == 0 ||
	    b.height != THICKNESS)
		return 92;
	mikros_panel_v1_ack_configure(panel, b.serial);

	uint32_t stride = b.width * 4;
	size_t size = (size_t)stride * b.height;
	/* Sealed against shrinking, so that the compositor may import it,
	 * and written with zeros, fully transparent. */
	int memory = memfd_create("panel", MFD_ALLOW_SEALING);
	if (memory < 0 || ftruncate(memory, (off_t)size) != 0 ||
	    fcntl(memory, F_ADD_SEALS, F_SEAL_SHRINK) != 0)
		return 93;
	void *pixels = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, memory, 0);
	if (pixels == MAP_FAILED)
		return 95;
	memset(pixels, 0, size);
	munmap(pixels, size);
	struct wl_buffer *buffer = mikros_buffer_v1_create_from_memory(
		b.buffers, memory, b.width, b.height, ARGB8888, stride, 0);

	struct wl_region *region = wl_compositor_create_region(b.compositor);
	wl_region_add(region, 0, 0, (int32_t)b.width, (int32_t)b.height);
	struct mikros_surface_material_v1 *material =
		mikros_material_v1_get_material(b.materials, surface);
	mikros_surface_material_v1_set_region(material, HAZE, region);
	wl_region_destroy(region);
	wl_surface_attach(surface, buffer, 0, 0);
	wl_surface_damage_buffer(surface, 0, 0, (int32_t)b.width, (int32_t)b.height);
	wl_surface_commit(surface);
	if (wl_display_roundtrip(display) < 0)
		return 94;
	close(memory);
	while (wl_display_dispatch(display) >= 0)
		;
	return 0;
}
