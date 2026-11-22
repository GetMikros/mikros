/* A window of an ordinary client (`--window`): a toplevel of 200 by 150
 * pixels of one colour, red 120, green 60, blue 200, whose buffer is
 * XRGB8888 in a memory object. It connects through the wayland protocol, as
 * apps do, acknowledges every configure but keeps its buffer, which the
 * compositor stretches to the window's frame when it is resized. It runs
 * until the compositor closes the connection. It exits with the number of
 * the step that failed, if one does. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "mikros-buffer-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

enum { XRGB8888 = 0x34325258, WIDTH = 200, HEIGHT = 150 };

struct win {
	struct wl_compositor *compositor;
	struct xdg_wm_base *wm_base;
	struct mikros_buffer_v1 *buffers;
	int configured;
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
		   const char *interface, uint32_t version)
{
	struct win *w = data;
	(void)version;
	if (strcmp(interface, wl_compositor_interface.name) == 0)
		w->compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
	else if (strcmp(interface, xdg_wm_base_interface.name) == 0)
		w->wm_base = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
	else if (strcmp(interface, mikros_buffer_v1_interface.name) == 0)
		w->buffers = wl_registry_bind(registry, name, &mikros_buffer_v1_interface, 1);
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
	(void)data, (void)registry, (void)name;
}

static const struct wl_registry_listener registry_listener = {
	.global = global,
	.global_remove = global_remove,
};

static void ping(void *data, struct xdg_wm_base *wm_base, uint32_t serial)
{
	(void)data;
	xdg_wm_base_pong(wm_base, serial);
}

static const struct xdg_wm_base_listener wm_base_listener = {.ping = ping};

static void surface_configure(void *data, struct xdg_surface *surface, uint32_t serial)
{
	struct win *w = data;
	xdg_surface_ack_configure(surface, serial);
	w->configured = 1;
}

static const struct xdg_surface_listener surface_listener = {.configure = surface_configure};

static void toplevel_configure(void *data, struct xdg_toplevel *toplevel, int32_t width,
			       int32_t height, struct wl_array *states)
{
	(void)data, (void)toplevel, (void)width, (void)height, (void)states;
}

static void toplevel_close(void *data, struct xdg_toplevel *toplevel)
{
	(void)data, (void)toplevel;
}

static const struct xdg_toplevel_listener toplevel_listener = {
	.configure = toplevel_configure,
	.close = toplevel_close,
};

int window(void)
{
	struct win w = {0};
	struct wl_display *display = wl_display_connect(NULL);
	if (!display) {
		fprintf(stderr, "greeter-probe: no connection (errno %d)\n", errno);
		return 10;
	}
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, &w);
	if (wl_display_roundtrip(display) < 0 || !w.compositor || !w.wm_base || !w.buffers)
		return 11;
	xdg_wm_base_add_listener(w.wm_base, &wm_base_listener, &w);
	struct wl_surface *surface = wl_compositor_create_surface(w.compositor);
	struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(w.wm_base, surface);
	xdg_surface_add_listener(xdg, &surface_listener, &w);
	struct xdg_toplevel *toplevel = xdg_surface_get_toplevel(xdg);
	xdg_toplevel_add_listener(toplevel, &toplevel_listener, &w);
	wl_surface_commit(surface);
	if (wl_display_roundtrip(display) < 0 || !w.configured)
		return 12;

	uint32_t stride = WIDTH * 4;
	size_t size = (size_t)stride * HEIGHT;
	/* Sealed against shrinking, so that the compositor may import it. */
	int memory = memfd_create("window", MFD_ALLOW_SEALING);
	if (memory < 0 || ftruncate(memory, (off_t)size) != 0 ||
	    fcntl(memory, F_ADD_SEALS, F_SEAL_SHRINK) != 0)
		return 13;
	uint32_t *pixels = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, memory, 0);
	if (pixels == MAP_FAILED)
		return 14;
	for (size_t i = 0; i < (size_t)WIDTH * HEIGHT; i++)
		pixels[i] = 0xff000000u | (120u << 16) | (60u << 8) | 200u;
	munmap(pixels, size);
	struct wl_buffer *buffer = mikros_buffer_v1_create_from_memory(w.buffers, memory, WIDTH, HEIGHT,
								       XRGB8888, stride, 0);
	wl_surface_attach(surface, buffer, 0, 0);
	wl_surface_damage_buffer(surface, 0, 0, WIDTH, HEIGHT);
	wl_surface_commit(surface);
	if (wl_display_roundtrip(display) < 0)
		return 15;
	close(memory);
	while (wl_display_dispatch(display) >= 0)
		;
	return 0;
}
