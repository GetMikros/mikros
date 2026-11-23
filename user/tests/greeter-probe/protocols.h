/* The protocols client of the greeter probe (`--protocols`): what its two
 * files share. */

#ifndef PROTOCOLS_H
#define PROTOCOLS_H

#define _GNU_SOURCE
#include <stdint.h>
#include <wayland-client.h>
#include "fractional-scale-v1-client-protocol.h"
#include "keyboard-shortcuts-inhibit-unstable-v1-client-protocol.h"
#include "mikros-buffer-v1-client-protocol.h"
#include "presentation-time-client-protocol.h"
#include "viewporter-client-protocol.h"
#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

enum { WAIT_MS = 60000 };

#define CLIPBOARD "clipboard probe\n"

struct probe {
	struct wl_display *display;
	struct wl_compositor *compositor;
	struct wl_subcompositor *subcompositor;
	struct xdg_wm_base *wm_base;
	struct mikros_buffer_v1 *buffers;
	struct wp_viewporter *viewporter;
	struct wp_fractional_scale_manager_v1 *fractional;
	struct wp_presentation *presentation;
	struct zwlr_layer_shell_v1 *layer_shell;
	struct zwp_keyboard_shortcuts_inhibit_manager_v1 *inhibit_manager;
	struct wl_seat *seat;
	struct wl_data_device_manager *data_manager;
	struct wl_keyboard *keyboard;
	struct wl_data_device *data_device;
	int parent_configured, layer_configured, popup_configured;
	int top_configured, top_closed;
	uint32_t layer_width, layer_height;
	int32_t popup_rect[4];
	uint32_t fractional_scale;
	int presented, discarded;
	int focused, inhibited;
	int selection_offered, selection_text;
	int sent;
};

/* Says `format` in the log (`report`), that a step failed (`fail`, which
 * returns the step), or which step was reached (`stage`). */
void report(const char *format, ...);
int fail(int step, const char *format, ...);
void stage(const char *what);

/* Reads and answers the events that came, for up to `ms` milliseconds
 * when none has, and pumps until `*flag` is set, or the time is up. */
int pump(struct wl_display *display, int ms);
int until(struct probe *p, const volatile int *flag);

/* The keyboard, the clipboard and the shortcuts inhibitor
 * (protocols_input.c): `input_start` listens to the seat, which gives the
 * keyboard, and to the data device; `input_inhibit` makes the inhibitor of
 * `surface`; `input_clipboard` sets the clipboard and reads it back,
 * and returns 0 or the number of the step that failed. */
void input_start(struct probe *p);
void input_inhibit(struct probe *p, struct wl_surface *surface);
int input_clipboard(struct probe *p);

#endif
