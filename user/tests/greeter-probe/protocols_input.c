/* The keyboard, the clipboard and the shortcuts inhibitor of the protocols
 * client (`--protocols`): the toplevel has the keyboard when it has
 * entered, a data source writes the text to the descriptor that a
 * receive passes, and the data device tells the offer of the selection. */

#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include "protocols.h"

static void inhibitor_active(void *data, struct zwp_keyboard_shortcuts_inhibitor_v1 *inhibitor)
{
	struct probe *p = data;
	(void)inhibitor;
	p->inhibited = 1;
}

static void inhibitor_inactive(void *data, struct zwp_keyboard_shortcuts_inhibitor_v1 *inhibitor)
{
	struct probe *p = data;
	(void)inhibitor;
	p->inhibited = 0;
}

static const struct zwp_keyboard_shortcuts_inhibitor_v1_listener inhibitor_listener = {
	.active = inhibitor_active,
	.inactive = inhibitor_inactive,
};

/* The keyboard: the toplevel has it when it has entered. */
static void keymap(void *data, struct wl_keyboard *k, uint32_t format, int32_t fd, uint32_t size)
{
	(void)data, (void)k, (void)format, (void)size;
	close(fd);
}

static void enter(void *data, struct wl_keyboard *k, uint32_t serial, struct wl_surface *surface,
		  struct wl_array *keys)
{
	struct probe *p = data;
	(void)k, (void)serial, (void)surface, (void)keys;
	p->focused = 1;
}

static void leave(void *data, struct wl_keyboard *k, uint32_t serial, struct wl_surface *surface)
{
	struct probe *p = data;
	(void)k, (void)serial, (void)surface;
	p->focused = 0;
}

static void key(void *data, struct wl_keyboard *k, uint32_t serial, uint32_t time, uint32_t code,
		uint32_t state)
{
	(void)data, (void)k, (void)serial, (void)time, (void)code, (void)state;
}

static void modifiers(void *data, struct wl_keyboard *k, uint32_t serial, uint32_t depressed,
		      uint32_t latched, uint32_t locked, uint32_t group)
{
	(void)data, (void)k, (void)serial, (void)depressed, (void)latched, (void)locked;
	(void)group;
}

static void repeat_info(void *data, struct wl_keyboard *k, int32_t rate, int32_t delay)
{
	(void)data, (void)k, (void)rate, (void)delay;
}

static const struct wl_keyboard_listener keyboard_listener = {
	.keymap = keymap,
	.enter = enter,
	.leave = leave,
	.key = key,
	.modifiers = modifiers,
	.repeat_info = repeat_info,
};

static void seat_capabilities(void *data, struct wl_seat *seat, uint32_t capabilities)
{
	struct probe *p = data;
	if ((capabilities & WL_SEAT_CAPABILITY_KEYBOARD) && !p->keyboard) {
		p->keyboard = wl_seat_get_keyboard(seat);
		wl_keyboard_add_listener(p->keyboard, &keyboard_listener, p);
	}
}

static void seat_name(void *data, struct wl_seat *seat, const char *name)
{
	(void)data, (void)seat, (void)name;
}

static const struct wl_seat_listener seat_listener = {
	.capabilities = seat_capabilities,
	.name = seat_name,
};

/* The clipboard: the source writes the text to the descriptor that a
 * receive passes; the device tells the offer of the selection. */
static void source_target(void *data, struct wl_data_source *source, const char *mime)
{
	(void)data, (void)source, (void)mime;
}

static void source_send(void *data, struct wl_data_source *source, const char *mime, int32_t fd)
{
	struct probe *p = data;
	(void)source;
	if (strcmp(mime, "text/plain") == 0 &&
	    write(fd, CLIPBOARD, strlen(CLIPBOARD)) == (ssize_t)strlen(CLIPBOARD))
		p->sent = 1;
	close(fd);
}

static void source_cancelled(void *data, struct wl_data_source *source)
{
	(void)data, (void)source;
}

static void source_drop(void *data, struct wl_data_source *source)
{
	(void)data, (void)source;
}

static void source_finished(void *data, struct wl_data_source *source)
{
	(void)data, (void)source;
}

static void source_action(void *data, struct wl_data_source *source, uint32_t action)
{
	(void)data, (void)source, (void)action;
}

static const struct wl_data_source_listener source_listener = {
	.target = source_target,
	.send = source_send,
	.cancelled = source_cancelled,
	.dnd_drop_performed = source_drop,
	.dnd_finished = source_finished,
	.action = source_action,
};

static void offer_mime(void *data, struct wl_data_offer *offer, const char *mime)
{
	struct probe *p = data;
	(void)offer;
	if (strcmp(mime, "text/plain") == 0)
		p->selection_text = 1;
}

static void offer_source_actions(void *data, struct wl_data_offer *offer, uint32_t actions)
{
	(void)data, (void)offer, (void)actions;
}

static void offer_action(void *data, struct wl_data_offer *offer, uint32_t action)
{
	(void)data, (void)offer, (void)action;
}

static const struct wl_data_offer_listener offer_listener = {
	.offer = offer_mime,
	.source_actions = offer_source_actions,
	.action = offer_action,
};

static struct wl_data_offer *selection_offer;

static void device_data_offer(void *data, struct wl_data_device *device, struct wl_data_offer *offer)
{
	(void)device;
	wl_data_offer_add_listener(offer, &offer_listener, data);
}

static void device_enter(void *data, struct wl_data_device *device, uint32_t serial,
			 struct wl_surface *surface, wl_fixed_t x, wl_fixed_t y,
			 struct wl_data_offer *offer)
{
	(void)data, (void)device, (void)serial, (void)surface, (void)x, (void)y, (void)offer;
}

static void device_leave(void *data, struct wl_data_device *device)
{
	(void)data, (void)device;
}

static void device_motion(void *data, struct wl_data_device *device, uint32_t time, wl_fixed_t x,
			  wl_fixed_t y)
{
	(void)data, (void)device, (void)time, (void)x, (void)y;
}

static void device_drop(void *data, struct wl_data_device *device)
{
	(void)data, (void)device;
}

static void device_selection(void *data, struct wl_data_device *device,
			     struct wl_data_offer *offer)
{
	struct probe *p = data;
	(void)device;
	selection_offer = offer;
	p->selection_offered = offer != NULL;
}

static const struct wl_data_device_listener device_listener = {
	.data_offer = device_data_offer,
	.enter = device_enter,
	.leave = device_leave,
	.motion = device_motion,
	.drop = device_drop,
	.selection = device_selection,
};

/* Sets the clipboard, and reads what the compositor's offer gives. */
int input_clipboard(struct probe *p)
{
	struct wl_data_source *source = wl_data_device_manager_create_data_source(p->data_manager);
	wl_data_source_add_listener(source, &source_listener, p);
	wl_data_source_offer(source, "text/plain");
	wl_data_device_set_selection(p->data_device, source, 0);
	if (!until(p, &p->selection_offered))
		return fail(30, "no selection came");
	if (!p->selection_text)
		return fail(31, "the offer of the selection has no text/plain");
	int fds[2];
	if (pipe(fds) != 0)
		return fail(32, "no pipe (errno %d)", errno);
	wl_data_offer_receive(selection_offer, "text/plain", fds[1]);
	close(fds[1]);
	char text[64] = {0};
	size_t have = 0;
	for (int waited = 0; waited < WAIT_MS; waited += 100) {
		if (pump(p->display, 100) < 0)
			return fail(33, "the connection broke");
		struct pollfd fd = {.fd = fds[0], .events = POLLIN};
		if (poll(&fd, 1, 0) > 0) {
			ssize_t n = read(fds[0], text + have, sizeof text - 1 - have);
			if (n <= 0)
				break;
			have += (size_t)n;
		}
	}
	close(fds[0]);
	if (strcmp(text, CLIPBOARD) != 0)
		return fail(34, "the clipboard gave \"%s\"", text);
	return 0;
}


void input_start(struct probe *p)
{
	wl_seat_add_listener(p->seat, &seat_listener, p);
	p->data_device = wl_data_device_manager_get_data_device(p->data_manager, p->seat);
	wl_data_device_add_listener(p->data_device, &device_listener, p);
}

void input_inhibit(struct probe *p, struct wl_surface *surface)
{
	struct zwp_keyboard_shortcuts_inhibitor_v1 *inhibitor =
		zwp_keyboard_shortcuts_inhibit_manager_v1_inhibit_shortcuts(
			p->inhibit_manager, surface, p->seat);
	zwp_keyboard_shortcuts_inhibitor_v1_add_listener(inhibitor, &inhibitor_listener, p);
}
