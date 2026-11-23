/* The greeter of the desktop run: the one part of the system app
 * system.mikros.greeter_probe, which the compositor starts in greeter
 * mode with its Wayland channel as the USER startup handle 1 and its
 * Login channel as 3. It shows a fullscreen toplevel, which Vulkan
 * clears to one colour and presents through the Wayland WSI, and which
 * the desktop run looks for on the screen. With the arguments
 * `--login <name> <password>`, it then logs that user in through
 * Login. Then it serves the connection until the compositor closes it.
 * Once the seat has a keyboard, it takes the keyboard's keymap and key
 * repeat, and when the toplevel has the keyboard's focus and the key K is
 * pressed, after the repeat of 25 keys per second after 600 ms that the
 * compositor gives without a setting, it presents a frame of a second
 * colour.
 *
 * With the argument `--control`, as a part of the system app
 * system.mikros.shell_probe in a user session, it shows nothing and only
 * connects to the service manager's Control, which needs a user
 * permission whose question the compositor cancels: it exits with 0 when
 * the connection is refused and with 70 when it is not. With
 * `--wallpaper [file]`, as that app's desktop part, it hands the
 * compositor a wallpaper (wallpaper.c), and with `--panel`, as its
 * taskbar part, it shows a panel of haze (panel.c).
 *
 * It exits with the number of the step that failed, if one does. */

#include <mikros/midl/mikros.session.h>
#include <mikros/services.h>
#include <mikros/startup.h>
#include <mikros/syscalls.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <wayland-client.h>
#define VK_USE_PLATFORM_WAYLAND_KHR
#include <vulkan/vulkan.h>
#include "xdg-shell-client-protocol.h"

enum { ROLE_USER = 16, WAYLAND_ID = 1, LOGIN_ID = 3, KIND_CHANNEL = 9, KEY_K = 37 };

/* The memory of the Login call's messages. */
static uint8_t arena_memory[MIKROS_MIDL_CALL_ROOM + 4096];

/* The colour, in the bytes of a B8G8R8A8_UNORM image: red 200, green 40,
 * blue 120. */
static const VkClearColorValue COLOUR = {.float32 = {200 / 255.0f, 40 / 255.0f, 120 / 255.0f, 1}};
/* The colour once K was typed: red 40, green 170, blue 90. */
static const VkClearColorValue TYPED = {.float32 = {40 / 255.0f, 170 / 255.0f, 90 / 255.0f, 1}};

struct state {
	struct wl_compositor *compositor;
	struct xdg_wm_base *wm_base;
	struct wl_seat *seat;
	struct wl_keyboard *keyboard;
	int32_t width, height;
	int configured;
	/* Whether the keyboard gave a keymap of xkb and the default key
	 * repeat, whether the toplevel has its focus, and whether K was
	 * typed: 1 until the frame of the second colour is presented, then
	 * 2. */
	int keymap, repeat, focused, typed;
	/* What presents the toplevel's frames. */
	VkDevice device;
	VkQueue queue;
	uint32_t family;
	VkSwapchainKHR swapchain;
	VkImage images[8];
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
		   const char *interface, uint32_t version)
{
	struct state *s = data;
	if (strcmp(interface, wl_compositor_interface.name) == 0 && version >= 4)
		s->compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
	else if (strcmp(interface, xdg_wm_base_interface.name) == 0)
		s->wm_base = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
	else if (strcmp(interface, wl_seat_interface.name) == 0 && !s->seat)
		s->seat = wl_registry_bind(registry, name, &wl_seat_interface, 5);
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

static void toplevel_configure(void *data, struct xdg_toplevel *toplevel, int32_t width,
			       int32_t height, struct wl_array *states)
{
	struct state *s = data;
	(void)toplevel, (void)states;
	s->width = width;
	s->height = height;
}

static void toplevel_close(void *data, struct xdg_toplevel *toplevel)
{
	(void)data, (void)toplevel;
}

static const struct xdg_toplevel_listener toplevel_listener = {
	.configure = toplevel_configure,
	.close = toplevel_close,
};

static void surface_configure(void *data, struct xdg_surface *surface, uint32_t serial)
{
	struct state *s = data;
	xdg_surface_ack_configure(surface, serial);
	s->configured = 1;
}

static const struct xdg_surface_listener surface_listener = {.configure = surface_configure};

static void keymap(void *data, struct wl_keyboard *keyboard, uint32_t format, int32_t fd,
		   uint32_t size)
{
	struct state *s = data;
	(void)keyboard;
	s->keymap = format == WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 && size > 0;
	close(fd);
}

static void enter(void *data, struct wl_keyboard *keyboard, uint32_t serial,
		  struct wl_surface *surface, struct wl_array *keys)
{
	struct state *s = data;
	(void)keyboard, (void)serial, (void)surface, (void)keys;
	s->focused = 1;
}

static void leave(void *data, struct wl_keyboard *keyboard, uint32_t serial,
		  struct wl_surface *surface)
{
	struct state *s = data;
	(void)keyboard, (void)serial, (void)surface;
	s->focused = 0;
}

static void key(void *data, struct wl_keyboard *keyboard, uint32_t serial, uint32_t time,
		uint32_t key, uint32_t state)
{
	struct state *s = data;
	(void)keyboard, (void)serial, (void)time;
	if (key == KEY_K && state == WL_KEYBOARD_KEY_STATE_PRESSED && s->keymap && s->repeat &&
	    s->focused && !s->typed)
		s->typed = 1;
}

static void modifiers(void *data, struct wl_keyboard *keyboard, uint32_t serial,
		      uint32_t depressed, uint32_t latched, uint32_t locked, uint32_t group)
{
	(void)data, (void)keyboard, (void)serial, (void)depressed, (void)latched, (void)locked,
		(void)group;
}

static void repeat_info(void *data, struct wl_keyboard *keyboard, int32_t rate, int32_t delay)
{
	struct state *s = data;
	(void)keyboard;
	s->repeat = rate == 25 && delay == 600;
}

static const struct wl_keyboard_listener keyboard_listener = {
	.keymap = keymap,
	.enter = enter,
	.leave = leave,
	.key = key,
	.modifiers = modifiers,
	.repeat_info = repeat_info,
};

/* The seat's keyboard, once it has one. */
static void capabilities(void *data, struct wl_seat *seat, uint32_t capabilities)
{
	struct state *s = data;
	if ((capabilities & WL_SEAT_CAPABILITY_KEYBOARD) && !s->keyboard) {
		s->keyboard = wl_seat_get_keyboard(seat);
		wl_keyboard_add_listener(s->keyboard, &keyboard_listener, s);
	}
}

static void seat_name(void *data, struct wl_seat *seat, const char *name)
{
	(void)data, (void)seat, (void)name;
}

static const struct wl_seat_listener seat_listener = {
	.capabilities = capabilities,
	.name = seat_name,
};

/* Clears the next image of the swapchain to `colour` and presents it:
 * one command buffer that waits for the acquisition and signals the
 * presentation. */
static int present(struct state *s, const VkClearColorValue *colour)
{
	VkDevice device = s->device;
	VkQueue queue = s->queue;
	uint32_t family = s->family;
	VkSwapchainKHR swapchain = s->swapchain;
	const VkImage *images = s->images;
	VkSemaphoreCreateInfo semaphore_info = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
	VkSemaphore acquired, drawn;
	if (vkCreateSemaphore(device, &semaphore_info, NULL, &acquired) != VK_SUCCESS ||
	    vkCreateSemaphore(device, &semaphore_info, NULL, &drawn) != VK_SUCCESS)
		return 50;
	uint32_t index;
	if (vkAcquireNextImageKHR(device, swapchain, UINT64_MAX, acquired, VK_NULL_HANDLE,
				  &index) != VK_SUCCESS)
		return 51;
	VkCommandPoolCreateInfo pool_info = {
		.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
		.queueFamilyIndex = family,
	};
	VkCommandPool pool;
	if (vkCreateCommandPool(device, &pool_info, NULL, &pool) != VK_SUCCESS)
		return 52;
	VkCommandBufferAllocateInfo allocate = {
		.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
		.commandPool = pool,
		.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
		.commandBufferCount = 1,
	};
	VkCommandBuffer commands;
	if (vkAllocateCommandBuffers(device, &allocate, &commands) != VK_SUCCESS)
		return 53;
	VkCommandBufferBeginInfo begin = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
	vkBeginCommandBuffer(commands, &begin);
	VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
	VkImageMemoryBarrier barrier = {
		.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
		.dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
		.oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
		.newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
		.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
		.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
		.image = images[index],
		.subresourceRange = range,
	};
	vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
			     VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);
	vkCmdClearColorImage(commands, images[index], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
			     colour, 1, &range);
	barrier.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
	barrier.dstAccessMask = 0;
	barrier.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
	barrier.newLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
	vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TRANSFER_BIT,
			     VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);
	if (vkEndCommandBuffer(commands) != VK_SUCCESS)
		return 54;
	VkPipelineStageFlags stage = VK_PIPELINE_STAGE_TRANSFER_BIT;
	VkSubmitInfo submit = {
		.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
		.waitSemaphoreCount = 1,
		.pWaitSemaphores = &acquired,
		.pWaitDstStageMask = &stage,
		.commandBufferCount = 1,
		.pCommandBuffers = &commands,
		.signalSemaphoreCount = 1,
		.pSignalSemaphores = &drawn,
	};
	if (vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE) != VK_SUCCESS)
		return 55;
	VkPresentInfoKHR present_info = {
		.sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
		.waitSemaphoreCount = 1,
		.pWaitSemaphores = &drawn,
		.swapchainCount = 1,
		.pSwapchains = &swapchain,
		.pImageIndices = &index,
	};
	if (vkQueuePresentKHR(queue, &present_info) != VK_SUCCESS)
		return 56;
	return 0;
}

/* Vulkan on the surface `surface` of `display`, its swapchain the size of
 * the toplevel, and one frame presented. */
static int draw(struct wl_display *display, struct wl_surface *surface, struct state *s)
{
	const char *instance_extensions[] = {VK_KHR_SURFACE_EXTENSION_NAME,
					     VK_KHR_WAYLAND_SURFACE_EXTENSION_NAME};
	VkApplicationInfo app = {
		.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
		.pApplicationName = "greeter-probe",
		.apiVersion = VK_API_VERSION_1_1,
	};
	VkInstanceCreateInfo instance_info = {
		.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
		.pApplicationInfo = &app,
		.enabledExtensionCount = 2,
		.ppEnabledExtensionNames = instance_extensions,
	};
	VkInstance instance;
	if (vkCreateInstance(&instance_info, NULL, &instance) != VK_SUCCESS)
		return 30;
	VkWaylandSurfaceCreateInfoKHR surface_info = {
		.sType = VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR,
		.display = display,
		.surface = surface,
	};
	VkSurfaceKHR target;
	if (vkCreateWaylandSurfaceKHR(instance, &surface_info, NULL, &target) != VK_SUCCESS)
		return 31;
	uint32_t count = 1;
	VkPhysicalDevice physical;
	VkResult listed = vkEnumeratePhysicalDevices(instance, &count, &physical);
	if ((listed != VK_SUCCESS && listed != VK_INCOMPLETE) || count == 0)
		return 32;
	/* The first queue family that draws and presents. */
	VkQueueFamilyProperties families[8];
	uint32_t family_count = 8, family = UINT32_MAX;
	vkGetPhysicalDeviceQueueFamilyProperties(physical, &family_count, families);
	for (uint32_t i = 0; i < family_count && family == UINT32_MAX; i++) {
		VkBool32 presents = VK_FALSE;
		vkGetPhysicalDeviceSurfaceSupportKHR(physical, i, target, &presents);
		if (presents && (families[i].queueFlags & VK_QUEUE_GRAPHICS_BIT))
			family = i;
	}
	if (family == UINT32_MAX)
		return 33;
	float priority = 1;
	VkDeviceQueueCreateInfo queue_info = {
		.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
		.queueFamilyIndex = family,
		.queueCount = 1,
		.pQueuePriorities = &priority,
	};
	const char *device_extensions[] = {VK_KHR_SWAPCHAIN_EXTENSION_NAME};
	VkDeviceCreateInfo device_info = {
		.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
		.queueCreateInfoCount = 1,
		.pQueueCreateInfos = &queue_info,
		.enabledExtensionCount = 1,
		.ppEnabledExtensionNames = device_extensions,
	};
	VkDevice device;
	if (vkCreateDevice(physical, &device_info, NULL, &device) != VK_SUCCESS)
		return 34;
	VkQueue queue;
	vkGetDeviceQueue(device, family, 0, &queue);
	VkSurfaceCapabilitiesKHR capabilities;
	if (vkGetPhysicalDeviceSurfaceCapabilitiesKHR(physical, target, &capabilities) !=
	    VK_SUCCESS)
		return 35;
	VkSurfaceFormatKHR formats[16];
	uint32_t format_count = 16;
	VkResult got = vkGetPhysicalDeviceSurfaceFormatsKHR(physical, target, &format_count, formats);
	if (got != VK_SUCCESS && got != VK_INCOMPLETE)
		return 36;
	int unorm = 0;
	for (uint32_t i = 0; i < format_count; i++)
		unorm |= formats[i].format == VK_FORMAT_B8G8R8A8_UNORM;
	if (!unorm)
		return 37;
	uint32_t images_wanted = capabilities.minImageCount < 2 ? 2 : capabilities.minImageCount;
	if (capabilities.maxImageCount != 0 && images_wanted > capabilities.maxImageCount)
		images_wanted = capabilities.maxImageCount;
	VkSwapchainCreateInfoKHR swapchain_info = {
		.sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
		.surface = target,
		.minImageCount = images_wanted,
		.imageFormat = VK_FORMAT_B8G8R8A8_UNORM,
		.imageColorSpace = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
		.imageExtent = {(uint32_t)s->width, (uint32_t)s->height},
		.imageArrayLayers = 1,
		.imageUsage = VK_IMAGE_USAGE_TRANSFER_DST_BIT,
		.imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
		.preTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
		.compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
		.presentMode = VK_PRESENT_MODE_FIFO_KHR,
		.clipped = VK_TRUE,
	};
	if (vkCreateSwapchainKHR(device, &swapchain_info, NULL, &s->swapchain) != VK_SUCCESS)
		return 38;
	uint32_t image_count = 8;
	got = vkGetSwapchainImagesKHR(device, s->swapchain, &image_count, s->images);
	if (got != VK_SUCCESS && got != VK_INCOMPLETE)
		return 39;
	s->device = device;
	s->queue = queue;
	s->family = family;
	return present(s, &COLOUR);
}

/* Logs `name` in with `password` through the Login channel. */
static int log_in(const char *name, const char *password)
{
	mikros_handle_t login = mikros_take_startup_handle(ROLE_USER, LOGIN_ID);
	if (login == 0)
		return 60;
	mikros_midl_arena_t arena;
	mikros_midl_arena_init(&arena, arena_memory, sizeof arena_memory);
	mikros_midl_string_t n = {name, strlen(name)};
	mikros_midl_string_t p = {password, strlen(password)};
	mikros_base_Status error;
	mikros_midl_status_t called =
		mikros_session_Login_Login(login, n, p, &error, INT64_MAX, &arena);
	/* A status that Login replied with: 100 and its value's magnitude. */
	if (called == MIKROS_MIDL_ERROR)
		return 100 - (int)error;
	return called == MIKROS_MIDL_OK ? 0 : 61;
}

/* Connects to the service manager's Control, which must be refused: 0
 * when it was, 70 when it was not, and 71 without a channel. */
static int control(void)
{
	mikros_handle_t ours, theirs;
	if (mikros_channel_create(0, &ours, &theirs) != 0)
		return 71;
	mikros_status_t connected = mikros_service_connect("mikros.service.Control", NULL, theirs);
	mikros_handle_close(ours);
	return connected == 0 ? 70 : 0;
}

/* Connects to the compositor as an ordinary client, through the session
 * manager's `wayland` protocol, and reads its globals: 0 when it did, 10
 * when there is no connection and 11 when the registry cannot be read. */
static int client(void)
{
	struct wl_display *display = wl_display_connect(NULL);
	if (!display) {
		fprintf(stderr, "greeter-probe: no connection (errno %d)\n", errno);
		return 10;
	}
	struct wl_registry *registry = wl_display_get_registry(display);
	if (!registry || wl_display_roundtrip(display) < 0) {
		fprintf(stderr, "greeter-probe: no registry (errno %d)\n", errno);
		return 11;
	}
	wl_registry_destroy(registry);
	wl_display_disconnect(display);
	return 0;
}

int wallpaper(const char *file);
int panel(void);
int window(void);
int protocols(void);

int main(int argc, char **argv)
{
	if (argc == 2 && strcmp(argv[1], "--control") == 0)
		return control();
	if (argc == 2 && strcmp(argv[1], "--client") == 0)
		return client();
	if (argc == 2 && strcmp(argv[1], "--window") == 0)
		return window();
	if (argc == 2 && strcmp(argv[1], "--protocols") == 0)
		return protocols();
	if (argc >= 2 && strcmp(argv[1], "--wallpaper") == 0)
		return wallpaper(argc >= 3 ? argv[2] : NULL);
	if (argc == 2 && strcmp(argv[1], "--panel") == 0)
		return panel();
	struct state s = {0};
	mikros_handle_t channel = mikros_take_startup_handle(ROLE_USER, WAYLAND_ID);
	int fd = channel == 0 ? -1 : mikros_fd_from_handle(channel, KIND_CHANNEL);
	struct wl_display *display = fd < 0 ? NULL : wl_display_connect_to_fd(fd);
	if (!display)
		return 10;
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, &s);
	if (wl_display_roundtrip(display) < 0 || !s.compositor || !s.wm_base)
		return 11;
	xdg_wm_base_add_listener(s.wm_base, &wm_base_listener, &s);
	if (s.seat)
		wl_seat_add_listener(s.seat, &seat_listener, &s);

	/* A fullscreen toplevel, which the compositor gives the screen's
	 * size. */
	struct wl_surface *surface = wl_compositor_create_surface(s.compositor);
	struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(s.wm_base, surface);
	xdg_surface_add_listener(xdg, &surface_listener, &s);
	struct xdg_toplevel *toplevel = xdg_surface_get_toplevel(xdg);
	xdg_toplevel_add_listener(toplevel, &toplevel_listener, &s);
	xdg_toplevel_set_fullscreen(toplevel, NULL);
	wl_surface_commit(surface);
	while (!s.configured)
		if (wl_display_dispatch(display) < 0)
			return 20;
	if (s.width <= 0 || s.height <= 0)
		return 21;

	int failed = draw(display, surface, &s);
	if (!failed && argc == 4 && strcmp(argv[1], "--login") == 0)
		failed = log_in(argv[2], argv[3]);
	/* The exit code, which the compositor logs, names the step that
	 * failed. */
	if (failed)
		return failed;
	while (wl_display_dispatch(display) >= 0) {
		if (s.typed == 1) {
			s.typed = 2;
			failed = present(&s, &TYPED);
			if (failed)
				return failed;
		}
	}
	return 0;
}
