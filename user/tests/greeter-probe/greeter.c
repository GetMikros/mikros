/* The greeter of the desktop run: the one part of the system app
 * system.mikros.greeter_probe, which the compositor starts in greeter
 * mode with its Wayland channel as the USER startup handle 1. It shows a
 * fullscreen toplevel, which Vulkan clears to one colour and presents
 * through the Wayland WSI, and which the desktop run looks for on the
 * screen; then it serves the connection until the compositor closes it.
 * It exits with the number of the step that failed, if one does. */

#include <mikros/startup.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <wayland-client.h>
#define VK_USE_PLATFORM_WAYLAND_KHR
#include <vulkan/vulkan.h>
#include "xdg-shell-client-protocol.h"

enum { ROLE_USER = 16, WAYLAND_ID = 1, KIND_CHANNEL = 9 };

/* The colour, in the bytes of a B8G8R8A8_UNORM image: red 200, green 40,
 * blue 120. */
static const VkClearColorValue COLOUR = {.float32 = {200 / 255.0f, 40 / 255.0f, 120 / 255.0f, 1}};

struct state {
	struct wl_compositor *compositor;
	struct xdg_wm_base *wm_base;
	int32_t width, height;
	int configured;
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
		   const char *interface, uint32_t version)
{
	struct state *s = data;
	if (strcmp(interface, wl_compositor_interface.name) == 0 && version >= 4)
		s->compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
	else if (strcmp(interface, xdg_wm_base_interface.name) == 0)
		s->wm_base = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
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

/* Clears the image `image` of the swapchain and presents it: one command
 * buffer that waits for the acquisition and signals the presentation. */
static int present(VkDevice device, VkQueue queue, uint32_t family, VkSwapchainKHR swapchain,
		   const VkImage *images)
{
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
			     &COLOUR, 1, &range);
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
	VkSwapchainKHR swapchain;
	if (vkCreateSwapchainKHR(device, &swapchain_info, NULL, &swapchain) != VK_SUCCESS)
		return 38;
	VkImage images[8];
	uint32_t image_count = 8;
	got = vkGetSwapchainImagesKHR(device, swapchain, &image_count, images);
	if (got != VK_SUCCESS && got != VK_INCOMPLETE)
		return 39;
	return present(device, queue, family, swapchain, images);
}

int main(void)
{
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
	if (failed) {
		fprintf(stderr, "greeter-probe: step %d failed\n", failed);
		return failed;
	}
	while (wl_display_dispatch(display) >= 0)
		;
	return 0;
}
