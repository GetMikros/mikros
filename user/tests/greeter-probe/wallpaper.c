/* The desktop part of the shell probe (`--wallpaper [file]`): it hands
 * the compositor a wallpaper through mikros_shell_v1.set_wallpaper, the
 * pixels of `file`, raw XRGB8888 rows of the output's size, or without a
 * file a flat test colour, waits until the compositor released the
 * buffer, and then serves the connection until the compositor closes it.
 * It exits with the number of the step that failed, if one does. */

#define _GNU_SOURCE
#include <fcntl.h>
#include <mikros/startup.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "mikros-buffer-v1-client-protocol.h"
#include "xdg-activation-v1-client-protocol.h"
#include "mikros-shell-v1-client-protocol.h"

enum { ROLE_USER = 16, WAYLAND_ID = 1, KIND_CHANNEL = 9, XRGB8888 = 0x34325258 };

/* The test colour in XRGB8888: red 40, green 90, blue 160. */
static const uint32_t COLOUR = 0x00285aa0;

struct wall {
	struct wl_output *output;
	struct mikros_buffer_v1 *buffers;
	struct mikros_shell_v1 *shell;
	int32_t width, height;
	int released;
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
		   const char *interface, uint32_t version)
{
	struct wall *w = data;
	(void)version;
	if (strcmp(interface, wl_output_interface.name) == 0 && !w->output)
		w->output = wl_registry_bind(registry, name, &wl_output_interface, 3);
	else if (strcmp(interface, mikros_buffer_v1_interface.name) == 0)
		w->buffers = wl_registry_bind(registry, name, &mikros_buffer_v1_interface, 1);
	else if (strcmp(interface, mikros_shell_v1_interface.name) == 0)
		w->shell = wl_registry_bind(registry, name, &mikros_shell_v1_interface, 1);
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
	(void)data, (void)registry, (void)name;
}

static const struct wl_registry_listener registry_listener = {
	.global = global,
	.global_remove = global_remove,
};

static void geometry(void *data, struct wl_output *output, int32_t x, int32_t y,
		     int32_t physical_width, int32_t physical_height, int32_t subpixel,
		     const char *make, const char *model, int32_t transform)
{
	(void)data, (void)output, (void)x, (void)y, (void)physical_width, (void)physical_height;
	(void)subpixel, (void)make, (void)model, (void)transform;
}

static void mode(void *data, struct wl_output *output, uint32_t flags, int32_t width,
		 int32_t height, int32_t refresh)
{
	struct wall *w = data;
	(void)output, (void)refresh;
	if (flags & WL_OUTPUT_MODE_CURRENT) {
		w->width = width;
		w->height = height;
	}
}

static void output_done(void *data, struct wl_output *output)
{
	(void)data, (void)output;
}

static void scale(void *data, struct wl_output *output, int32_t factor)
{
	(void)data, (void)output, (void)factor;
}

static const struct wl_output_listener output_listener = {
	.geometry = geometry,
	.mode = mode,
	.done = output_done,
	.scale = scale,
};

static void release(void *data, struct wl_buffer *buffer)
{
	struct wall *w = data;
	(void)buffer;
	w->released = 1;
}

static const struct wl_buffer_listener buffer_listener = {.release = release};

/* Fills the `size` bytes at `pixels` from `file`, or with the test colour
 * without one: 0 on success. */
static int fill(uint32_t *pixels, size_t size, const char *file)
{
	if (!file) {
		for (size_t i = 0; i < size / 4; i++)
			pixels[i] = COLOUR;
		return 0;
	}
	int fd = open(file, O_RDONLY);
	if (fd < 0)
		return -1;
	size_t at = 0;
	while (at < size) {
		ssize_t n = read(fd, (uint8_t *)pixels + at, size - at);
		if (n <= 0)
			break;
		at += (size_t)n;
	}
	close(fd);
	return at == size ? 0 : -1;
}

int wallpaper(const char *file)
{
	struct wall w = {0};
	mikros_handle_t channel = mikros_take_startup_handle(ROLE_USER, WAYLAND_ID);
	int fd = channel == 0 ? -1 : mikros_fd_from_handle(channel, KIND_CHANNEL);
	struct wl_display *display = fd < 0 ? NULL : wl_display_connect_to_fd(fd);
	if (!display)
		return 80;
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, &w);
	if (wl_display_roundtrip(display) < 0 || !w.output || !w.buffers || !w.shell)
		return 81;
	wl_output_add_listener(w.output, &output_listener, &w);
	if (wl_display_roundtrip(display) < 0 || w.width <= 0 || w.height <= 0)
		return 82;

	uint32_t stride = (uint32_t)w.width * 4;
	size_t size = (size_t)stride * (size_t)w.height;
	/* Sealed against shrinking, so that the compositor may import it. */
	int memory = memfd_create("wallpaper", MFD_ALLOW_SEALING);
	if (memory < 0 || ftruncate(memory, (off_t)size) != 0 ||
	    fcntl(memory, F_ADD_SEALS, F_SEAL_SHRINK) != 0)
		return 83;
	uint32_t *pixels = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, memory, 0);
	if (pixels == MAP_FAILED)
		return 84;
	int filled = fill(pixels, size, file);
	munmap(pixels, size);
	if (filled != 0)
		return 85;

	struct wl_buffer *buffer = mikros_buffer_v1_create_from_memory(
		w.buffers, memory, (uint32_t)w.width, (uint32_t)w.height, XRGB8888, stride, 0);
	wl_buffer_add_listener(buffer, &buffer_listener, &w);
	mikros_shell_v1_set_wallpaper(w.shell, w.output, buffer);
	if (wl_display_roundtrip(display) < 0)
		return 86;
	close(memory);
	if (!w.released)
		return 87;
	while (wl_display_dispatch(display) >= 0)
		;
	return 0;
}
