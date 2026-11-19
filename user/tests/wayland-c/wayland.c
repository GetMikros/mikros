/* A C program for the tests of libwayland.dlib. It is the client and,
 * on the other end of a channel, a compositor of its own that reads the
 * requests and writes the events by hand. It checks registry globals,
 * a listener of ten parameters, file descriptors passed both ways, and
 * that queued requests travel together in one channel message. It exits
 * with 0 when every check passed, and otherwise with the number of the
 * first check that failed. */

#define _GNU_SOURCE
#include <mikros/startup.h>
#include <mikros/syscalls.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>

enum { CHANNEL = 9, MEMORY = 4, TYPE_MEMORY = 3, SHOULD_WAIT = -22 };

/* The compositor's end, and the last channel message it read. */
static mikros_handle_t server;
static uint32_t message[16384];
static uint64_t message_len, handle_count;
static mikros_handle_t handles[64];

static int receive(void)
{
	return mikros_channel_read(server, (uint8_t *)message, sizeof message, handles, 64,
				   &message_len, &handle_count) == 0;
}

/* The number of Wayland messages in the last channel message. */
static int requests(void)
{
	int n = 0;
	for (uint64_t at = 0; at + 8 <= message_len; n++)
		at += message[at / 4 + 1] >> 16;
	return n;
}

/* Events, built word by word. */
static uint32_t event[256];
static int words;

static void begin(uint32_t object, uint32_t opcode)
{
	event[words] = object;
	event[words + 1] = opcode;
	words += 2;
}

static void put(uint32_t v)
{
	event[words++] = v;
}

static void put_string(const char *s)
{
	uint32_t len = strlen(s) + 1;
	put(len);
	memset(&event[words], 0, (len + 3) & ~3u);
	memcpy(&event[words], s, len);
	words += (len + 3) / 4;
}

/* Ends the event begun at word start, setting its size. */
static void end(int start)
{
	event[start + 1] |= (uint32_t)(words - start) * 4 << 16;
}

static int send_events(mikros_handle_t *passed, uint64_t count)
{
	int ok = mikros_channel_write(server, (const uint8_t *)event, words * 4, passed, count) == 0;
	words = 0;
	return ok;
}

struct state {
	int globals, geometry, keymap_fd;
	char interfaces[4][32];
	uint32_t versions[4];
	int32_t x, y, width, height, subpixel, transform;
	char make[16], model[16];
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
		   const char *interface, uint32_t version)
{
	struct state *s = data;
	if (s->globals < 4 && name == (uint32_t)s->globals + 1) {
		strncpy(s->interfaces[s->globals], interface, 31);
		s->versions[s->globals] = version;
	}
	s->globals++;
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
}

static const struct wl_registry_listener registry_listener = {global, global_remove};

static void geometry(void *data, struct wl_output *output, int32_t x, int32_t y,
		     int32_t width, int32_t height, int32_t subpixel, const char *make,
		     const char *model, int32_t transform)
{
	struct state *s = data;
	s->geometry++;
	s->x = x;
	s->y = y;
	s->width = width;
	s->height = height;
	s->subpixel = subpixel;
	s->transform = transform;
	strncpy(s->make, make, 15);
	strncpy(s->model, model, 15);
}

static const struct wl_output_listener output_listener = {.geometry = geometry};

static void keymap(void *data, struct wl_keyboard *keyboard, uint32_t format, int32_t fd,
		   uint32_t size)
{
	struct state *s = data;
	s->keymap_fd = format == 1 && size == 6 ? fd : -1;
}

static const struct wl_keyboard_listener keyboard_listener = {.keymap = keymap};

int main(void)
{
	struct state s = {.keymap_fd = -2};
	mikros_handle_t client;
	if (mikros_channel_create(0, &client, &server) != 0)
		return 10;
	int fd = mikros_fd_from_handle(client, CHANNEL);
	struct wl_display *display = fd < 0 ? NULL : wl_display_connect_to_fd(fd);
	if (!display)
		return 11;
	if (wl_display_get_fd(display) != fd)
		return 12;

	/* The registry, and four globals in one channel message. */
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, &s);
	if (wl_display_flush(display) < 0 || !receive())
		return 20;
	if (requests() != 1 || message[0] != 1 || (message[1] & 0xffff) != 1 || message[2] != 2)
		return 21;
	const char *names[] = {"wl_output", "wl_shm", "wl_seat", "wl_compositor"};
	const uint32_t versions[] = {4, 1, 7, 6};
	for (int i = 0; i < 4; i++) {
		int start = words;
		begin(2, 0);
		put(i + 1);
		put_string(names[i]);
		put(versions[i]);
		end(start);
	}
	if (!send_events(NULL, 0) || wl_display_dispatch(display) < 0)
		return 22;
	if (s.globals != 4)
		return 23;
	for (int i = 0; i < 4; i++)
		if (strcmp(s.interfaces[i], names[i]) != 0 || s.versions[i] != versions[i])
			return 24;

	/* Five requests, one with a descriptor, come in one channel message
	 * with its handle. */
	struct wl_output *output = wl_registry_bind(registry, 1, &wl_output_interface, 4);
	struct wl_shm *shm = wl_registry_bind(registry, 2, &wl_shm_interface, 1);
	struct wl_seat *seat = wl_registry_bind(registry, 3, &wl_seat_interface, 7);
	struct wl_keyboard *keyboard = wl_seat_get_keyboard(seat);
	wl_output_add_listener(output, &output_listener, &s);
	wl_keyboard_add_listener(keyboard, &keyboard_listener, &s);
	int pool_fd = memfd_create("wayland-c", 0);
	if (pool_fd < 0 || write(pool_fd, "SHM", 3) != 3 || ftruncate(pool_fd, 4096))
		return 30;
	struct wl_shm_pool *pool = wl_shm_create_pool(shm, pool_fd, 4096);
	close(pool_fd);
	if (!pool || wl_display_flush(display) < 0 || !receive())
		return 31;
	if (requests() != 5 || handle_count != 1)
		return 32;
	int received = mikros_fd_from_handle(handles[0], MEMORY);
	char buf[8];
	if (received < 0 || pread(received, buf, 3, 0) != 3 || memcmp(buf, "SHM", 3) != 0)
		return 33;
	close(received);

	/* An event of eight arguments, and one with a descriptor. */
	int start = words;
	begin(3, 0);
	put((uint32_t)-5);
	put(7);
	put(300);
	put(200);
	put(1);
	put_string("Mikros");
	put_string("Test");
	put(0);
	end(start);
	start = words;
	begin(6, 0);
	put(1);
	put(6);
	end(start);
	int keymap_source = memfd_create("keymap", 0);
	mikros_handle_t keymap_handle;
	if (keymap_source < 0 || write(keymap_source, "KEYMAP", 6) != 6 ||
	    mikros_handle_from_fd(keymap_source, &keymap_handle) != 0)
		return 40;
	if (!send_events(&keymap_handle, 1) || wl_display_dispatch(display) < 0)
		return 41;
	if (s.geometry != 1 || s.x != -5 || s.y != 7 || s.width != 300 || s.height != 200 ||
	    s.subpixel != 1 || s.transform != 0 || strcmp(s.make, "Mikros") != 0 ||
	    strcmp(s.model, "Test") != 0)
		return 42;
	if (s.keymap_fd < 0)
		return 43;
	char *keymap_text = mmap(NULL, 6, PROT_READ, MAP_PRIVATE, s.keymap_fd, 0);
	if (keymap_text == MAP_FAILED || memcmp(keymap_text, "KEYMAP", 6) != 0)
		return 44;

	/* A hundred queued requests travel in one channel message. */
	for (int i = 0; i < 100; i++)
		wl_callback_destroy(wl_display_sync(display));
	if (wl_display_flush(display) < 0 || !receive() || requests() != 100)
		return 50;
	uint64_t len, count;
	if (mikros_channel_read(server, (uint8_t *)message, sizeof message, handles, 64, &len,
				&count) != SHOULD_WAIT)
		return 51;

	/* Once the compositor goes, dispatching fails. */
	mikros_handle_close(server);
	if (wl_display_dispatch(display) != -1)
		return 60;
	wl_display_disconnect(display);
	return 0;
}
