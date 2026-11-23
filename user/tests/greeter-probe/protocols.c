/* The protocols client (`--protocols`): an ordinary client of the user's
 * session that uses the protocols of MIKSPEC-0066.R16 and MIKSPEC-0076.R24
 * that the desktop run has no other client for, and says on its standard
 * error how each went. It makes a toplevel of 240 by 180 pixels, PARENT
 * colour, with, as the subsurfaces of its surface, a synchronized one of 50
 * by 50 pixels at (10, 20), CHILD colour, and a desynchronized one at (100,
 * 30) whose buffer of 20 by 20 pixels, VIEW_LEFT on its left half and
 * VIEW_RIGHT on its right, is cropped by a viewport to the left half and
 * sized 60 by 40; a popup of 40 by 30 pixels, POPUP colour, placed at (20,
 * 120) of the toplevel by a positioner; and a layer surface in the layer
 * `bottom` along the left edge, 24 pixels wide, LAYER colour. It asks for
 * the fractional scale of its surface and for the feedback on the commit
 * that shows the toplevel, and, once the toplevel has the keyboard, sets
 * the clipboard, reads it back through a pipe as the data offer gives it
 * and checks that its shortcuts inhibitor is active. Then it says
 * "greeter-probe: protocols ready" and serves the connection until the
 * compositor closes it. It exits with the number of the step that failed,
 * after saying which on its standard error. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <mikros/midl/mikros.log.h>
#include <mikros/services.h>
#include <mikros/syscalls.h>
#include <wayland-client.h>
#include "fractional-scale-v1-client-protocol.h"
#include "keyboard-shortcuts-inhibit-unstable-v1-client-protocol.h"
#include "mikros-buffer-v1-client-protocol.h"
#include "presentation-time-client-protocol.h"
#include "viewporter-client-protocol.h"
#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"
#include "protocols.h"

enum {
	XRGB8888 = 0x34325258,
	PARENT_W = 240,
	PARENT_H = 180,
	LAYER_BOTTOM = 1,
	ANCHOR_LEFT_TOP_BOTTOM = 7,
};

#define PARENT 0xff3c8cdcu
#define CHILD 0xffdc3c3cu
#define VIEW_LEFT 0xfff0c800u
#define VIEW_RIGHT 0xff00a0a0u
#define LAYER 0xff14c878u
#define POPUP 0xfffafa3cu


/* Says `format` in the log, through `mikros.log.Writer`, which is where the
 * run reads it: a client that the console starts has no log on its
 * standard error. */
void report(const char *format, ...)
{
	static mikros_handle_t writer;
	static uint8_t room[MIKROS_MIDL_CALL_ROOM + 2048];
	char text[512];
	va_list args;
	va_start(args, format);
	vsnprintf(text, sizeof text, format, args);
	va_end(args);
	if (!writer) {
		mikros_handle_t theirs;
		if (mikros_channel_create(0, &writer, &theirs) != 0) {
			writer = 0;
			fputs(text, stderr);
			return;
		}
		if (mikros_service_connect("mikros.log.Writer", NULL, theirs) != 0) {
			mikros_handle_close(writer);
			writer = 0;
			fputs(text, stderr);
			return;
		}
	}
	mikros_midl_arena_t arena;
	mikros_midl_arena_init(&arena, room, sizeof room);
	mikros_midl_string_t message = {text, strlen(text)};
	mikros_log_Writer_Write(writer, mikros_log_Severity_ERROR, message, &arena);
}

int fail(int step, const char *format, ...)
{
	char detail[400];
	va_list args;
	va_start(args, format);
	vsnprintf(detail, sizeof detail, format, args);
	va_end(args);
	report("greeter-probe: protocols failed at step %d: %s", step, detail);
	return step;
}

/* Says on standard error which step the client has reached. */
void stage(const char *what)
{
	report("greeter-probe: protocols: %s", what);
}

/* Reads and answers the events that came, for up to `ms` milliseconds when
 * none has. Fails when the connection broke. */
int pump(struct wl_display *display, int ms)
{
	while (wl_display_prepare_read(display) != 0)
		if (wl_display_dispatch_pending(display) < 0)
			return -1;
	if (wl_display_flush(display) < 0 && errno != EAGAIN) {
		wl_display_cancel_read(display);
		return -1;
	}
	struct pollfd fd = {.fd = wl_display_get_fd(display), .events = POLLIN};
	if (poll(&fd, 1, ms) > 0) {
		if (wl_display_read_events(display) < 0)
			return -1;
	} else {
		wl_display_cancel_read(display);
	}
	return wl_display_dispatch_pending(display) < 0 ? -1 : 0;
}

/* Pumps until `*flag` is set, or the time is up. */
int until(struct probe *p, const volatile int *flag)
{
	for (int waited = 0; !*flag && waited < WAIT_MS; waited += 100)
		if (pump(p->display, 100) < 0)
			return 0;
	return *flag;
}

/* A buffer of `width` by `height` pixels whose left `split` columns, all
 * of them for 0, are `left` and the others `right`. */
static struct wl_buffer *make_buffer(struct probe *p, int width, int height, int split,
				     uint32_t left, uint32_t right)
{
	uint32_t stride = (uint32_t)width * 4;
	size_t size = (size_t)stride * (size_t)height;
	int memory = memfd_create("protocols", MFD_ALLOW_SEALING);
	if (memory < 0 || ftruncate(memory, (off_t)size) != 0 ||
	    fcntl(memory, F_ADD_SEALS, F_SEAL_SHRINK) != 0)
		return NULL;
	uint32_t *pixels = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, memory, 0);
	if (pixels == MAP_FAILED)
		return NULL;
	for (int y = 0; y < height; y++)
		for (int x = 0; x < width; x++)
			pixels[(size_t)y * (size_t)width + (size_t)x] =
				split == 0 || x < split ? left : right;
	munmap(pixels, size);
	struct wl_buffer *buffer = mikros_buffer_v1_create_from_memory(
		p->buffers, memory, width, height, XRGB8888, stride, 0);
	close(memory);
	return buffer;
}

static void attach(struct wl_surface *surface, struct wl_buffer *buffer, int width, int height)
{
	wl_surface_attach(surface, buffer, 0, 0);
	wl_surface_damage_buffer(surface, 0, 0, width, height);
}

/* The globals. */
static void global(void *data, struct wl_registry *registry, uint32_t name,
		   const char *interface, uint32_t version)
{
	struct probe *p = data;
#define BIND(field, iface, v)                                                              \
	if (strcmp(interface, iface.name) == 0) {                                          \
		p->field = wl_registry_bind(registry, name, &iface,                        \
					    version < (v) ? version : (v));                \
		return;                                                                    \
	}
	BIND(compositor, wl_compositor_interface, 4)
	BIND(subcompositor, wl_subcompositor_interface, 1)
	BIND(wm_base, xdg_wm_base_interface, 3)
	BIND(buffers, mikros_buffer_v1_interface, 1)
	BIND(viewporter, wp_viewporter_interface, 1)
	BIND(fractional, wp_fractional_scale_manager_v1_interface, 1)
	BIND(presentation, wp_presentation_interface, 1)
	BIND(layer_shell, zwlr_layer_shell_v1_interface, 4)
	BIND(inhibit_manager, zwp_keyboard_shortcuts_inhibit_manager_v1_interface, 1)
	BIND(seat, wl_seat_interface, 5)
	BIND(data_manager, wl_data_device_manager_interface, 3)
#undef BIND
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

/* The toplevel's and the popup's xdg_surface. */
static void parent_surface_configure(void *data, struct xdg_surface *surface, uint32_t serial)
{
	struct probe *p = data;
	xdg_surface_ack_configure(surface, serial);
	p->parent_configured = 1;
}

static const struct xdg_surface_listener parent_surface_listener = {
	.configure = parent_surface_configure,
};

static void popup_surface_configure(void *data, struct xdg_surface *surface, uint32_t serial)
{
	struct probe *p = data;
	xdg_surface_ack_configure(surface, serial);
	p->popup_configured = 1;
}

static const struct xdg_surface_listener popup_surface_listener = {
	.configure = popup_surface_configure,
};

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

static void popup_configure(void *data, struct xdg_popup *popup, int32_t x, int32_t y,
			    int32_t width, int32_t height)
{
	struct probe *p = data;
	(void)popup;
	p->popup_rect[0] = x, p->popup_rect[1] = y;
	p->popup_rect[2] = width, p->popup_rect[3] = height;
}

static void popup_done(void *data, struct xdg_popup *popup)
{
	(void)data, (void)popup;
}

static void popup_repositioned(void *data, struct xdg_popup *popup, uint32_t token)
{
	(void)data, (void)popup, (void)token;
}

static const struct xdg_popup_listener popup_listener = {
	.configure = popup_configure,
	.popup_done = popup_done,
	.repositioned = popup_repositioned,
};

/* The layer surface. */
static void layer_configure(void *data, struct zwlr_layer_surface_v1 *surface, uint32_t serial,
			    uint32_t width, uint32_t height)
{
	struct probe *p = data;
	zwlr_layer_surface_v1_ack_configure(surface, serial);
	p->layer_width = width, p->layer_height = height;
	p->layer_configured = 1;
}

static void layer_closed(void *data, struct zwlr_layer_surface_v1 *surface)
{
	(void)data, (void)surface;
	report("greeter-probe: protocols: the layer surface was closed");
}

static const struct zwlr_layer_surface_v1_listener layer_listener = {
	.configure = layer_configure,
	.closed = layer_closed,
};

/* The fractional scale, the presentation feedback and the inhibitor. */
static void preferred_scale(void *data, struct wp_fractional_scale_v1 *scale, uint32_t value)
{
	struct probe *p = data;
	(void)scale;
	p->fractional_scale = value;
}

static const struct wp_fractional_scale_v1_listener scale_listener = {
	.preferred_scale = preferred_scale,
};

static void feedback_sync_output(void *data, struct wp_presentation_feedback *feedback,
				 struct wl_output *output)
{
	(void)data, (void)feedback, (void)output;
}

static void feedback_presented(void *data, struct wp_presentation_feedback *feedback,
			       uint32_t sec_hi, uint32_t sec_lo, uint32_t nsec, uint32_t refresh,
			       uint32_t seq_hi, uint32_t seq_lo, uint32_t flags)
{
	struct probe *p = data;
	(void)feedback, (void)sec_hi, (void)sec_lo, (void)nsec, (void)refresh;
	(void)seq_hi, (void)seq_lo, (void)flags;
	p->presented = 1;
}

static void feedback_discarded(void *data, struct wp_presentation_feedback *feedback)
{
	struct probe *p = data;
	(void)feedback;
	p->discarded = 1;
}

static const struct wp_presentation_feedback_listener feedback_listener = {
	.sync_output = feedback_sync_output,
	.presented = feedback_presented,
	.discarded = feedback_discarded,
};

int protocols(void)
{
	struct probe p = {0};
	p.display = wl_display_connect(NULL);
	if (!p.display)
		return fail(10, "no connection (errno %d)", errno);
	struct wl_registry *registry = wl_display_get_registry(p.display);
	wl_registry_add_listener(registry, &registry_listener, &p);
	if (wl_display_roundtrip(p.display) < 0)
		return fail(11, "no registry");
	if (!p.compositor || !p.subcompositor || !p.wm_base || !p.buffers || !p.viewporter ||
	    !p.fractional || !p.presentation || !p.layer_shell || !p.inhibit_manager ||
	    !p.seat || !p.data_manager)
		return fail(12, "a global is missing: compositor %d subcompositor %d wm_base %d "
				"buffers %d viewporter %d fractional %d presentation %d "
				"layer_shell %d inhibit %d seat %d data %d",
			    !!p.compositor, !!p.subcompositor, !!p.wm_base, !!p.buffers,
			    !!p.viewporter, !!p.fractional, !!p.presentation, !!p.layer_shell,
			    !!p.inhibit_manager, !!p.seat, !!p.data_manager);
	xdg_wm_base_add_listener(p.wm_base, &wm_base_listener, &p);
	input_start(&p);

	/* The toplevel. */
	struct wl_surface *parent = wl_compositor_create_surface(p.compositor);
	struct xdg_surface *parent_xdg = xdg_wm_base_get_xdg_surface(p.wm_base, parent);
	xdg_surface_add_listener(parent_xdg, &parent_surface_listener, &p);
	struct xdg_toplevel *toplevel = xdg_surface_get_toplevel(parent_xdg);
	xdg_toplevel_add_listener(toplevel, &toplevel_listener, &p);
	xdg_toplevel_set_title(toplevel, "protocols");
	struct wp_fractional_scale_v1 *scale =
		wp_fractional_scale_manager_v1_get_fractional_scale(p.fractional, parent);
	wp_fractional_scale_v1_add_listener(scale, &scale_listener, &p);
	wl_surface_commit(parent);
	if (!until(&p, &p.parent_configured))
		return fail(13, "the toplevel was not configured");
	stage("the toplevel is configured");
	if (p.fractional_scale != 120)
		return fail(14, "the preferred fractional scale is %u, not 120", p.fractional_scale);

	/* The layer surface along the left edge, in the layer `bottom`. */
	struct wl_surface *layer_surface = wl_compositor_create_surface(p.compositor);
	struct zwlr_layer_surface_v1 *layer = zwlr_layer_shell_v1_get_layer_surface(
		p.layer_shell, layer_surface, NULL, LAYER_BOTTOM, "protocols");
	zwlr_layer_surface_v1_add_listener(layer, &layer_listener, &p);
	zwlr_layer_surface_v1_set_size(layer, 24, 0);
	zwlr_layer_surface_v1_set_anchor(layer, ANCHOR_LEFT_TOP_BOTTOM);
	zwlr_layer_surface_v1_set_exclusive_zone(layer, 24);
	wl_surface_commit(layer_surface);
	if (!until(&p, &p.layer_configured))
		return fail(15, "the layer surface was not configured");
	if (p.layer_width != 24 || p.layer_height == 0)
		return fail(16, "the layer surface got %ux%u", p.layer_width, p.layer_height);
	stage("the layer surface is configured");
	struct wl_buffer *layer_buffer = make_buffer(&p, 24, (int)p.layer_height, 0, LAYER, LAYER);
	if (!layer_buffer)
		return fail(17, "no buffer for the layer surface");
	attach(layer_surface, layer_buffer, 24, (int)p.layer_height);
	wl_surface_commit(layer_surface);

	/* The subsurfaces: the synchronized one is shown with its parent's
	 * commit, the other, desynchronized and cropped by a viewport, at its own. */
	struct wl_surface *child = wl_compositor_create_surface(p.compositor);
	struct wl_subsurface *sub = wl_subcompositor_get_subsurface(p.subcompositor, child, parent);
	wl_subsurface_set_position(sub, 10, 20);
	struct wl_buffer *child_buffer = make_buffer(&p, 50, 50, 0, CHILD, CHILD);
	struct wl_surface *view = wl_compositor_create_surface(p.compositor);
	struct wl_subsurface *view_sub = wl_subcompositor_get_subsurface(p.subcompositor, view, parent);
	wl_subsurface_set_position(view_sub, 100, 30);
	wl_subsurface_set_desync(view_sub);
	struct wp_viewport *viewport = wp_viewporter_get_viewport(p.viewporter, view);
	struct wl_buffer *view_buffer = make_buffer(&p, 20, 20, 10, VIEW_LEFT, VIEW_RIGHT);
	struct wl_buffer *parent_buffer = make_buffer(&p, PARENT_W, PARENT_H, 0, PARENT, PARENT);
	if (!child_buffer || !view_buffer || !parent_buffer)
		return fail(18, "no buffers for the surfaces");
	attach(child, child_buffer, 50, 50);
	wl_surface_commit(child);
	wp_viewport_set_source(viewport, wl_fixed_from_int(0), wl_fixed_from_int(0),
			       wl_fixed_from_int(10), wl_fixed_from_int(20));
	wp_viewport_set_destination(viewport, 60, 40);
	attach(view, view_buffer, 20, 20);
	wl_surface_commit(view);
	struct wp_presentation_feedback *feedback = wp_presentation_feedback(p.presentation, parent);
	wp_presentation_feedback_add_listener(feedback, &feedback_listener, &p);
	attach(parent, parent_buffer, PARENT_W, PARENT_H);
	wl_surface_commit(parent);
	if (wl_display_roundtrip(p.display) < 0)
		return fail(19, "the commits broke the connection");
	stage("the surfaces are committed");

	/* The popup, placed at (20, 120) by the bottom right corner of the
	 * rectangle (0, 100, 20, 20), extending down and to the right. */
	struct xdg_positioner *positioner = xdg_wm_base_create_positioner(p.wm_base);
	xdg_positioner_set_size(positioner, 40, 30);
	xdg_positioner_set_anchor_rect(positioner, 0, 100, 20, 20);
	xdg_positioner_set_anchor(positioner, XDG_POSITIONER_ANCHOR_BOTTOM_RIGHT);
	xdg_positioner_set_gravity(positioner, XDG_POSITIONER_GRAVITY_BOTTOM_RIGHT);
	struct wl_surface *popup_surface = wl_compositor_create_surface(p.compositor);
	struct xdg_surface *popup_xdg = xdg_wm_base_get_xdg_surface(p.wm_base, popup_surface);
	xdg_surface_add_listener(popup_xdg, &popup_surface_listener, &p);
	struct xdg_popup *popup = xdg_surface_get_popup(popup_xdg, parent_xdg, positioner);
	xdg_popup_add_listener(popup, &popup_listener, &p);
	wl_surface_commit(popup_surface);
	if (!until(&p, &p.popup_configured))
		return fail(20, "the popup was not configured");
	if (p.popup_rect[0] != 20 || p.popup_rect[1] != 120 || p.popup_rect[2] != 40 ||
	    p.popup_rect[3] != 30)
		return fail(21, "the popup got (%d, %d) %dx%d", p.popup_rect[0], p.popup_rect[1],
			    p.popup_rect[2], p.popup_rect[3]);
	stage("the popup is configured");
	struct wl_buffer *popup_buffer = make_buffer(&p, 40, 30, 0, POPUP, POPUP);
	if (!popup_buffer)
		return fail(22, "no buffer for the popup");
	attach(popup_surface, popup_buffer, 40, 30);
	wl_surface_commit(popup_surface);

	/* A frame that showed the toplevel gives the feedback, and a
	 * keyboard that entered it, the inhibitor and the clipboard. */
	int answered = 0;
	for (int waited = 0; waited < WAIT_MS && !answered; waited += 100) {
		if (pump(p.display, 100) < 0)
			return fail(23, "the connection broke");
		answered = p.presented || p.discarded;
	}
	if (!p.presented)
		return fail(24, "the feedback of the commit was %s",
			    p.discarded ? "discarded" : "not answered");
	stage("the feedback was presented");
	input_inhibit(&p, parent);
	report("greeter-probe: protocols waiting for the keyboard");
	if (!until(&p, &p.focused))
		return fail(25, "the toplevel did not get the keyboard");
	if (!until(&p, &p.inhibited))
		return fail(26, "the shortcuts inhibitor did not become active");
	int failed = input_clipboard(&p);
	if (failed)
		return failed;
	report("greeter-probe: protocols ready");
	while (pump(p.display, 1000) >= 0)
		;
	return 0;
}
