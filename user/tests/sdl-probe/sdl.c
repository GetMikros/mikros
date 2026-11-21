/* The SDL part of the desktop run: a part of the system app
 * system.mikros.shell_probe, which the compositor starts in a user
 * session with its Wayland channel as the USER startup handle 1. It
 * opens a window with SDL3's Wayland driver, makes a Vulkan surface of it
 * with SDL, clears the window to one colour with Vulkan and presents it,
 * which the desktop run looks for on the screen, and then serves the
 * connection until the compositor closes it.
 *
 * It uses what the compositor serves and nothing else: no seat and no
 * shared memory, so SDL has no input and no cursors here. The channel is
 * connected to here, as the other parts do, and SDL is given the display.
 *
 * It prints nothing. It exits with the number of the step that failed,
 * which the compositor logs: 10 the channel, 20 SDL's start, 21 the
 * window, 22 SDL's Vulkan extensions, 30 to 39 Vulkan up to the
 * swapchain, 50 to 56 the frame.
 */

#include <mikros/startup.h>
#include <mikros/syscalls.h>
#include <stdint.h>
#include <string.h>
#include <wayland-client.h>
#define VK_USE_PLATFORM_WAYLAND_KHR
#include <vulkan/vulkan.h>
#include <SDL3/SDL.h>
#include <SDL3/SDL_vulkan.h>

enum { ROLE_USER = 16, WAYLAND_ID = 1, KIND_CHANNEL = 9 };

/* The size of the window, which the desktop run looks for in the corner
 * of the screen where the compositor puts a toplevel. */
enum { WIDTH = 128, HEIGHT = 96 };

/* The colour, in the bytes of a B8G8R8A8_UNORM image: red 230, green
 * 150, blue 20. */
static const VkClearColorValue COLOUR = {.float32 = {230 / 255.0f, 150 / 255.0f, 20 / 255.0f, 1}};

/* Clears the image `index` of the swapchain and presents it: one command
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

/* Vulkan on the window `window`, with SDL's extensions and surface, and
 * one frame presented. */
static int draw(SDL_Window *window)
{
	Uint32 extension_count = 0;
	const char *const *extensions = SDL_Vulkan_GetInstanceExtensions(&extension_count);
	if (!extensions)
		return 22;
	int wayland = 0;
	for (Uint32 i = 0; i < extension_count; i++)
		wayland |= strcmp(extensions[i], VK_KHR_WAYLAND_SURFACE_EXTENSION_NAME) == 0;
	if (!wayland)
		return 22;

	VkApplicationInfo app = {
		.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
		.pApplicationName = "sdl-probe",
		.apiVersion = VK_API_VERSION_1_1,
	};
	VkInstanceCreateInfo instance_info = {
		.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
		.pApplicationInfo = &app,
		.enabledExtensionCount = extension_count,
		.ppEnabledExtensionNames = extensions,
	};
	VkInstance instance;
	if (vkCreateInstance(&instance_info, NULL, &instance) != VK_SUCCESS)
		return 30;
	VkSurfaceKHR target;
	if (!SDL_Vulkan_CreateSurface(window, instance, NULL, &target))
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
	int width = 0, height = 0;
	SDL_GetWindowSizeInPixels(window, &width, &height);
	uint32_t images_wanted = capabilities.minImageCount < 2 ? 2 : capabilities.minImageCount;
	if (capabilities.maxImageCount != 0 && images_wanted > capabilities.maxImageCount)
		images_wanted = capabilities.maxImageCount;
	VkSwapchainCreateInfoKHR swapchain_info = {
		.sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
		.surface = target,
		.minImageCount = images_wanted,
		.imageFormat = VK_FORMAT_B8G8R8A8_UNORM,
		.imageColorSpace = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
		.imageExtent = {(uint32_t)width, (uint32_t)height},
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
	mikros_handle_t channel = mikros_take_startup_handle(ROLE_USER, WAYLAND_ID);
	int fd = channel == 0 ? -1 : mikros_fd_from_handle(channel, KIND_CHANNEL);
	struct wl_display *display = fd < 0 ? NULL : wl_display_connect_to_fd(fd);
	if (!display)
		return 10;

	SDL_SetHint(SDL_HINT_VIDEO_DRIVER, "wayland");
	SDL_SetPointerProperty(SDL_GetGlobalProperties(), SDL_PROP_GLOBAL_VIDEO_WAYLAND_WL_DISPLAY_POINTER,
			       display);
	if (!SDL_Init(SDL_INIT_VIDEO))
		return 20;
	SDL_Window *window = SDL_CreateWindow("sdl-probe", WIDTH, HEIGHT, SDL_WINDOW_VULKAN);
	if (!window)
		return 21;

	int failed = draw(window);
	if (failed)
		return failed;
	/* The window stays until the compositor closes the connection. */
	for (;;) {
		SDL_Event event;
		while (SDL_PollEvent(&event))
			if (event.type == SDL_EVENT_QUIT)
				return 0;
		if (wl_display_get_error(display) != 0)
			return 0;
		SDL_Delay(50);
	}
}
