/* A system test in C of Vulkan through the loader, which selftest runs as
 * it runs the others. It finds a physical device of the type CPU that
 * supports Vulkan 1.3, the CPU driver, and runs a compute shader that
 * writes the square of each index into a buffer of host-visible memory,
 * whose values it then checks. The shader, square.comp, is compiled to
 * SPIR-V on the build host into square.h.
 *
 * The memory is exportable, and the test imports it again through each
 * of its handle types, the Mikros ones and the descriptors of the C
 * library, and checks that the imports share its contents. The dispatch
 * signals an exportable timeline semaphore, a kernel timeline, which the
 * test then signals and reads through the kernel and through semaphores
 * imported from it. It exits with 0 when every check passed, and
 * otherwise with the number of the first check that failed.
 *
 * Last, the test checks the linear images that the compositor samples
 * its clients' buffers through: an image bound to a memory object that
 * another image uses shows its pixels, and a buffer copies into one
 * whatever its row length and offset. */

#define VK_USE_PLATFORM_MIKROS
#include <mikros/startup.h>
#include <stdio.h>
#include <string.h>
#include <vulkan/vulkan.h>
/* After the headers that declare its types. */
#include "square.h"

enum { COUNT = 1024, GROUP = 64 };

/* The kinds of the C library's descriptors of memory objects and of
 * other handles. */
enum { MEMORY_DESCRIPTOR = 4, HANDLE_DESCRIPTOR = 10 };

/* The device extensions that the test needs. */
static const char *const EXTENSIONS[] = {
    VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
    VK_MIKROS_EXTERNAL_MEMORY_HANDLE_EXTENSION_NAME,
    VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME,
    VK_MIKROS_EXTERNAL_SEMAPHORE_HANDLE_EXTENSION_NAME,
};
enum { EXTENSION_COUNT = sizeof EXTENSIONS / sizeof EXTENSIONS[0] };

/* The memory handle types of Mikros and of ported software. */
static const VkExternalMemoryHandleTypeFlags MEMORY_HANDLES =
    VK_EXTERNAL_MEMORY_HANDLE_TYPE_TOKEN_BIT_MIKROS |
    VK_EXTERNAL_MEMORY_HANDLE_TYPE_MEMORY_OBJECT_BIT_MIKROS |
    VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;

/* The semaphore handle types of Mikros and of ported software. */
static const VkExternalSemaphoreHandleTypeFlags SEMAPHORE_HANDLES =
    VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_TIMELINE_BIT_MIKROS |
    VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;

/* Five seconds, the longest that any wait of the test may take. */
static const uint64_t PATIENCE = 5000000000u;

static PFN_vkGetMemoryHandleMIKROS get_memory_handle;
static PFN_vkGetMemoryHandlePropertiesMIKROS memory_handle_properties;
static PFN_vkGetMemoryFdKHR get_memory_fd;
static PFN_vkGetSemaphoreHandleMIKROS get_semaphore_handle;
static PFN_vkImportSemaphoreHandleMIKROS import_semaphore_handle;
static PFN_vkGetSemaphoreFdKHR get_semaphore_fd;
static PFN_vkImportSemaphoreFdKHR import_semaphore_fd;

/* Whether `physical` offers every extension of EXTENSIONS. */
static int has_extensions(VkPhysicalDevice physical)
{
    VkExtensionProperties offered[512];
    uint32_t count = 512;
    if (vkEnumerateDeviceExtensionProperties(physical, NULL, &count, offered) < 0)
        return 0;
    for (uint32_t i = 0; i < EXTENSION_COUNT; i++) {
        int found = 0;
        for (uint32_t j = 0; j < count && !found; j++)
            found = strcmp(offered[j].extensionName, EXTENSIONS[i]) == 0;
        if (!found)
            return 0;
    }
    return 1;
}

/* Imports `size` bytes of memory of `type` with the import structure
 * `import`, and checks that they hold the squares, stores 7 in the first
 * value for the caller to find in its own memory, and frees them. Returns
 * 0, or `check` or a number after it for the check that failed. */
static int imported(VkDevice device, uint32_t type, VkDeviceSize size, const void *import,
                    int check)
{
    VkMemoryAllocateInfo info = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .pNext = import,
        .allocationSize = size,
        .memoryTypeIndex = type,
    };
    VkDeviceMemory memory;
    if (vkAllocateMemory(device, &info, NULL, &memory) != VK_SUCCESS)
        return check;
    uint32_t *values;
    if (vkMapMemory(device, memory, 0, VK_WHOLE_SIZE, 0, (void **)&values) != VK_SUCCESS)
        return check + 1;
    for (uint32_t i = 1; i < COUNT; i++)
        if (values[i] != i * i)
            return check + 2;
    values[0] = 7;
    vkUnmapMemory(device, memory);
    vkFreeMemory(device, memory, NULL);
    return 0;
}

/* Exports `memory`, of `type` and `size` bytes, as each handle type, and
 * imports it again. `values` is its mapping, whose first value each
 * import sets to 7. Returns 0 or the number of the check that failed. */
static int share_memory(VkDevice device, VkDeviceMemory memory, uint32_t type,
                        VkDeviceSize size, uint32_t *values)
{
    /* A memory object, and a descriptor that holds one. */
    VkMemoryGetHandleInfoMIKROS get = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_GET_HANDLE_INFO_MIKROS,
        .memory = memory,
        .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_MEMORY_OBJECT_BIT_MIKROS,
    };
    mikros_handle_t handle;
    if (get_memory_handle(device, &get, &handle) != VK_SUCCESS || handle == 0)
        return 70;
    VkMemoryHandlePropertiesMIKROS properties = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_HANDLE_PROPERTIES_MIKROS,
    };
    if (memory_handle_properties(device, get.handleType, handle, &properties) != VK_SUCCESS ||
        !(properties.memoryTypeBits & (1u << type)))
        return 71;
    VkImportMemoryHandleInfoMIKROS import = {
        .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_HANDLE_INFO_MIKROS,
        .handleType = get.handleType,
        .handle = handle,
    };
    values[0] = 0;
    int failed = imported(device, type, size, &import, 72);
    if (failed != 0)
        return failed;
    if (values[0] != 7)
        return 75;
    VkMemoryGetFdInfoKHR get_fd = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
        .memory = memory,
        .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
    };
    VkImportMemoryFdInfoKHR import_fd = {
        .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
        .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
    };
    if (get_memory_fd(device, &get_fd, &import_fd.fd) != VK_SUCCESS)
        return 76;
    mikros_handle_t held;
    if (mikros_handle_from_fd(import_fd.fd, &held) != 0)
        return 77;
    mikros_handle_close(held);
    values[0] = 0;
    failed = imported(device, type, size, &import_fd, 78);
    if (failed != 0)
        return failed;
    if (values[0] != 7)
        return 81;

    /* An export token, and a descriptor that holds one. */
    get.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_TOKEN_BIT_MIKROS;
    import.handleType = get.handleType;
    if (get_memory_handle(device, &get, &import.handle) != VK_SUCCESS || import.handle == 0)
        return 82;
    values[0] = 0;
    failed = imported(device, type, size, &import, 83);
    if (failed != 0)
        return failed;
    if (values[0] != 7)
        return 86;
    if (get_memory_handle(device, &get, &handle) != VK_SUCCESS || handle == 0)
        return 87;
    import_fd.fd = mikros_fd_from_handle(handle, HANDLE_DESCRIPTOR);
    if (import_fd.fd < 0)
        return 88;
    values[0] = 0;
    failed = imported(device, type, size, &import_fd, 89);
    if (failed != 0)
        return failed;
    if (values[0] != 7)
        return 92;
    return 0;
}

/* A new timeline semaphore at `value`, exportable as `handles`, or
 * VK_NULL_HANDLE. */
static VkSemaphore timeline(VkDevice device, uint64_t value,
                            VkExternalSemaphoreHandleTypeFlags handles)
{
    VkExportSemaphoreCreateInfo export_info = {
        .sType = VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO,
        .handleTypes = handles,
    };
    VkSemaphoreTypeCreateInfo type_info = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO,
        .pNext = handles != 0 ? &export_info : NULL,
        .semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE,
        .initialValue = value,
    };
    VkSemaphoreCreateInfo info = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO,
        .pNext = &type_info,
    };
    VkSemaphore semaphore;
    if (vkCreateSemaphore(device, &info, NULL, &semaphore) != VK_SUCCESS)
        return VK_NULL_HANDLE;
    return semaphore;
}

/* Whether `semaphore` reaches `value` within PATIENCE. */
static int reaches(VkDevice device, VkSemaphore semaphore, uint64_t value)
{
    VkSemaphoreWaitInfo wait = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_WAIT_INFO,
        .semaphoreCount = 1,
        .pSemaphores = &semaphore,
        .pValues = &value,
    };
    return vkWaitSemaphores(device, &wait, PATIENCE) == VK_SUCCESS;
}

/* Exports `semaphore`, which the dispatch has signalled with 1, as a
 * kernel timeline, and checks it through the kernel and through
 * semaphores imported from it, with a submission to `queue` that waits
 * for a value before anything signals it. Returns 0 or the number of the
 * check that failed. */
static int share_timeline(VkDevice device, VkQueue queue, VkSemaphore semaphore)
{
    VkSemaphoreGetHandleInfoMIKROS get = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_GET_HANDLE_INFO_MIKROS,
        .semaphore = semaphore,
        .handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_TIMELINE_BIT_MIKROS,
    };
    mikros_handle_t kernel;
    uint64_t value = 0;
    if (get_semaphore_handle(device, &get, &kernel) != VK_SUCCESS || kernel == 0)
        return 100;
    if (mikros_timeline_get(kernel, &value) != 0 || value != 1)
        return 101;

    /* A batch that waits for 2 and then signals 3, before the kernel
     * signals 2. */
    const uint64_t wait_value = 2, signal_value = 3;
    VkPipelineStageFlags stage = VK_PIPELINE_STAGE_ALL_COMMANDS_BIT;
    VkTimelineSemaphoreSubmitInfo values = {
        .sType = VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO,
        .waitSemaphoreValueCount = 1,
        .pWaitSemaphoreValues = &wait_value,
        .signalSemaphoreValueCount = 1,
        .pSignalSemaphoreValues = &signal_value,
    };
    VkSubmitInfo submit = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .pNext = &values,
        .waitSemaphoreCount = 1,
        .pWaitSemaphores = &semaphore,
        .pWaitDstStageMask = &stage,
        .signalSemaphoreCount = 1,
        .pSignalSemaphores = &semaphore,
    };
    if (vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE) != VK_SUCCESS)
        return 102;
    if (mikros_timeline_signal(kernel, 2) != 0)
        return 103;
    if (!reaches(device, semaphore, 3))
        return 104;

    /* A semaphore of the same kernel timeline, imported from a copy of
     * the handle. */
    VkSemaphore second = timeline(device, 0, 0);
    if (second == VK_NULL_HANDLE)
        return 105;
    VkImportSemaphoreHandleInfoMIKROS import = {
        .sType = VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_HANDLE_INFO_MIKROS,
        .semaphore = second,
        .handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_TIMELINE_BIT_MIKROS,
    };
    if (get_semaphore_handle(device, &get, &import.handle) != VK_SUCCESS)
        return 106;
    if (import_semaphore_handle(device, &import) != VK_SUCCESS)
        return 107;
    if (vkGetSemaphoreCounterValue(device, second, &value) != VK_SUCCESS || value != 3)
        return 108;

    /* And one imported from a descriptor that holds the timeline, which
     * the host signals. */
    VkSemaphore third = timeline(device, 0, 0);
    if (third == VK_NULL_HANDLE)
        return 109;
    VkSemaphoreGetFdInfoKHR get_fd = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR,
        .semaphore = semaphore,
        .handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT,
    };
    VkImportSemaphoreFdInfoKHR import_fd = {
        .sType = VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_FD_INFO_KHR,
        .semaphore = third,
        .handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT,
    };
    if (get_semaphore_fd(device, &get_fd, &import_fd.fd) != VK_SUCCESS)
        return 110;
    if (import_semaphore_fd(device, &import_fd) != VK_SUCCESS)
        return 111;
    VkSemaphoreSignalInfo signal = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_SIGNAL_INFO,
        .semaphore = third,
        .value = 4,
    };
    if (vkSignalSemaphore(device, &signal) != VK_SUCCESS)
        return 112;
    if (mikros_timeline_get(kernel, &value) != 0 || value != 4 || !reaches(device, second, 4))
        return 113;

    vkDestroySemaphore(device, third, NULL);
    vkDestroySemaphore(device, second, NULL);
    mikros_handle_close(kernel);
    return 0;
}

/* The linear image of the compositor's sampled client buffers: a 2D image
 * of IMAGE_FORMAT, IMAGE_WIDTH by IMAGE_HEIGHT pixels, whose rows are not
 * a multiple of the cache line in length. */
enum { IMAGE_WIDTH = 100, IMAGE_HEIGHT = 8 };
static const VkFormat IMAGE_FORMAT = VK_FORMAT_B8G8R8A8_UNORM;
static const VkImageUsageFlags IMAGE_USAGE = VK_IMAGE_USAGE_SAMPLED_BIT |
                                             VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
                                             VK_IMAGE_USAGE_TRANSFER_DST_BIT;

/* The value of the pixel at `column` of `row` in the images' test pattern. */
static uint32_t pattern(uint32_t row, uint32_t column)
{
    return 0xff000000u | (row << 12) | column;
}

/* The index of a memory type of `bits` that the host maps without flushes,
 * or UINT32_MAX. */
static uint32_t visible_type(VkPhysicalDevice physical, uint32_t bits)
{
    VkPhysicalDeviceMemoryProperties properties;
    vkGetPhysicalDeviceMemoryProperties(physical, &properties);
    const VkMemoryPropertyFlags visible =
        VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    for (uint32_t i = 0; i < properties.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (properties.memoryTypes[i].propertyFlags & visible) == visible)
            return i;
    return UINT32_MAX;
}

/* Creates a linear image of the pattern's size that `handles` export and
 * import, and returns it, or VK_NULL_HANDLE. */
static VkImage linear_image(VkDevice device, VkExternalMemoryHandleTypeFlags handles)
{
    VkExternalMemoryImageCreateInfo external = {
        .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
        .handleTypes = handles,
    };
    VkImageCreateInfo info = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        .pNext = handles != 0 ? &external : NULL,
        .imageType = VK_IMAGE_TYPE_2D,
        .format = IMAGE_FORMAT,
        .extent = {IMAGE_WIDTH, IMAGE_HEIGHT, 1},
        .mipLevels = 1,
        .arrayLayers = 1,
        .samples = VK_SAMPLE_COUNT_1_BIT,
        .tiling = VK_IMAGE_TILING_LINEAR,
        .usage = IMAGE_USAGE,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
        .initialLayout = VK_IMAGE_LAYOUT_PREINITIALIZED,
    };
    VkImage image;
    if (vkCreateImage(device, &info, NULL, &image) != VK_SUCCESS)
        return VK_NULL_HANDLE;
    return image;
}

/* Records `record` into a new command buffer of `pool`, submits it to
 * `queue` and waits for it. */
static int run_commands(VkDevice device, VkQueue queue, VkCommandPool pool,
                        void (*record)(VkCommandBuffer, const void *), const void *what)
{
    VkCommandBufferAllocateInfo info = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    VkCommandBuffer commands;
    if (vkAllocateCommandBuffers(device, &info, &commands) != VK_SUCCESS)
        return -1;
    VkCommandBufferBeginInfo begin = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    if (vkBeginCommandBuffer(commands, &begin) != VK_SUCCESS)
        return -1;
    record(commands, what);
    if (vkEndCommandBuffer(commands) != VK_SUCCESS)
        return -1;
    VkSubmitInfo submit = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers = &commands,
    };
    if (vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE) != VK_SUCCESS ||
        vkQueueWaitIdle(queue) != VK_SUCCESS)
        return -1;
    vkFreeCommandBuffers(device, pool, 1, &commands);
    return 0;
}

/* A copy between a buffer and a linear image. */
struct copy {
    VkBuffer buffer;
    VkImage image;
    VkDeviceSize offset;
    uint32_t row_length;
    int to_image;
};

static void record_copy(VkCommandBuffer commands, const void *what)
{
    const struct copy *copy = what;
    VkImageMemoryBarrier barrier = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_HOST_WRITE_BIT,
        .dstAccessMask = copy->to_image ? VK_ACCESS_TRANSFER_WRITE_BIT : VK_ACCESS_TRANSFER_READ_BIT,
        .oldLayout = VK_IMAGE_LAYOUT_PREINITIALIZED,
        .newLayout = copy->to_image ? VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL
                                    : VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .image = copy->image,
        .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1},
    };
    vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_HOST_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0,
                         NULL, 0, NULL, 1, &barrier);
    VkBufferImageCopy region = {
        .bufferOffset = copy->offset,
        .bufferRowLength = copy->row_length,
        .imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
        .imageExtent = {IMAGE_WIDTH, IMAGE_HEIGHT, 1},
    };
    if (copy->to_image)
        vkCmdCopyBufferToImage(commands, copy->buffer, copy->image,
                               VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);
    else
        vkCmdCopyImageToBuffer(commands, copy->image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                               copy->buffer, 1, &region);
}

/* A buffer of `size` bytes with host-visible memory, mapped at `*mapped`.
 * Returns 0 or `check` or the number after it. */
static int mapped_buffer(VkPhysicalDevice physical, VkDevice device, VkDeviceSize size,
                         VkBuffer *buffer, VkDeviceMemory *memory, uint8_t **mapped, int check)
{
    VkBufferCreateInfo info = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = size,
        .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT,
    };
    if (vkCreateBuffer(device, &info, NULL, buffer) != VK_SUCCESS)
        return check;
    VkMemoryRequirements needs;
    vkGetBufferMemoryRequirements(device, *buffer, &needs);
    VkMemoryAllocateInfo allocate = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = needs.size,
        .memoryTypeIndex = visible_type(physical, needs.memoryTypeBits),
    };
    if (allocate.memoryTypeIndex == UINT32_MAX ||
        vkAllocateMemory(device, &allocate, NULL, memory) != VK_SUCCESS ||
        vkBindBufferMemory(device, *buffer, *memory, 0) != VK_SUCCESS ||
        vkMapMemory(device, *memory, 0, VK_WHOLE_SIZE, 0, (void **)mapped) != VK_SUCCESS)
        return check + 1;
    return 0;
}

/* Checks what the compositor's sampled client buffers rely on: a linear
 * image takes the memory object of another image, at an offset, and
 * shows its pixels, and a buffer of another row length and offset copies
 * into one. Reports the row pitch that the driver gives the images, as
 * the client's row length must equal it for an import without a copy.
 * Returns 0 or the number of the check that failed. */
static int linear_images(VkPhysicalDevice physical, VkDevice device, VkQueue queue,
                         uint32_t family)
{
    const VkExternalMemoryHandleTypeFlags handle = VK_EXTERNAL_MEMORY_HANDLE_TYPE_MEMORY_OBJECT_BIT_MIKROS;

    /* The driver offers the format as a sampled, shareable linear image. */
    VkFormatProperties format;
    vkGetPhysicalDeviceFormatProperties(physical, IMAGE_FORMAT, &format);
    if (!(format.linearTilingFeatures & VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT))
        return 120;
    VkPhysicalDeviceExternalImageFormatInfo external_info = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO,
        .handleType = handle,
    };
    VkPhysicalDeviceImageFormatInfo2 image_info = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2,
        .pNext = &external_info,
        .format = IMAGE_FORMAT,
        .type = VK_IMAGE_TYPE_2D,
        .tiling = VK_IMAGE_TILING_LINEAR,
        .usage = IMAGE_USAGE,
    };
    VkExternalImageFormatProperties external = {
        .sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES,
    };
    VkImageFormatProperties2 properties = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2,
        .pNext = &external,
    };
    const VkExternalMemoryFeatureFlags shared =
        VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT | VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT;
    if (vkGetPhysicalDeviceImageFormatProperties2(physical, &image_info, &properties) !=
            VK_SUCCESS ||
        (external.externalMemoryProperties.externalMemoryFeatures & shared) != shared)
        return 121;

    /* The first image, in memory that has room for an offset. */
    VkImage first = linear_image(device, handle);
    if (first == VK_NULL_HANDLE)
        return 122;
    VkMemoryRequirements needs;
    vkGetImageMemoryRequirements(device, first, &needs);
    const VkDeviceSize offset = needs.alignment;
    VkExportMemoryAllocateInfo export_info = {
        .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
        .handleTypes = handle,
    };
    VkMemoryAllocateInfo allocate = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .pNext = &export_info,
        .allocationSize = offset + needs.size,
        .memoryTypeIndex = visible_type(physical, needs.memoryTypeBits),
    };
    VkDeviceMemory memory;
    if (allocate.memoryTypeIndex == UINT32_MAX ||
        vkAllocateMemory(device, &allocate, NULL, &memory) != VK_SUCCESS)
        return 123;
    if (vkBindImageMemory(device, first, memory, offset) != VK_SUCCESS)
        return 124;
    VkImageSubresource colour = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0};
    VkSubresourceLayout layout;
    vkGetImageSubresourceLayout(device, first, &colour, &layout);
    const uint32_t tight = IMAGE_WIDTH * 4;
    printf("vulkan-c: a linear image of %u bytes a row has a pitch of %llu, and starts at %llu\n",
           tight, (unsigned long long)layout.rowPitch, (unsigned long long)layout.offset);
    if (layout.rowPitch < tight || layout.rowPitch % 4 != 0)
        return 125;

    /* Its pixels, written by the host through the memory. */
    uint8_t *pixels;
    if (vkMapMemory(device, memory, 0, VK_WHOLE_SIZE, 0, (void **)&pixels) != VK_SUCCESS)
        return 126;
    for (uint32_t row = 0; row < IMAGE_HEIGHT; row++)
        for (uint32_t column = 0; column < IMAGE_WIDTH; column++)
            ((uint32_t *)(pixels + offset + layout.offset + row * layout.rowPitch))[column] =
                pattern(row, column);

    /* The same memory imported, with a second image bound to it at the
     * same offset, which a copy reads back. */
    VkMemoryGetHandleInfoMIKROS get = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_GET_HANDLE_INFO_MIKROS,
        .memory = memory,
        .handleType = handle,
    };
    VkImportMemoryHandleInfoMIKROS import = {
        .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_HANDLE_INFO_MIKROS,
        .handleType = handle,
    };
    if (get_memory_handle(device, &get, &import.handle) != VK_SUCCESS || import.handle == 0)
        return 127;
    allocate.pNext = &import;
    VkDeviceMemory imported_memory;
    if (vkAllocateMemory(device, &allocate, NULL, &imported_memory) != VK_SUCCESS)
        return 128;
    VkImage second = linear_image(device, handle);
    if (second == VK_NULL_HANDLE || vkBindImageMemory(device, second, imported_memory, offset) != VK_SUCCESS)
        return 129;
    VkCommandPoolCreateInfo pool_info = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = family,
    };
    VkCommandPool pool;
    if (vkCreateCommandPool(device, &pool_info, NULL, &pool) != VK_SUCCESS)
        return 130;
    VkBuffer readback;
    VkDeviceMemory readback_memory;
    uint8_t *read;
    int failed = mapped_buffer(physical, device, tight * IMAGE_HEIGHT, &readback, &readback_memory,
                               &read, 131);
    if (failed != 0)
        return failed;
    struct copy from_image = {.buffer = readback, .image = second};
    if (run_commands(device, queue, pool, record_copy, &from_image) != 0)
        return 133;
    for (uint32_t row = 0; row < IMAGE_HEIGHT; row++)
        for (uint32_t column = 0; column < IMAGE_WIDTH; column++)
            if (((uint32_t *)(read + row * tight))[column] != pattern(row, column))
                return 134;

    /* A client buffer whose rows are longer than the image's and that
     * starts after eight bytes, copied into a new image, which the host
     * reads through the image's own row pitch. */
    const uint32_t stride = tight + 12;
    const VkDeviceSize start = 8;
    VkBuffer client;
    VkDeviceMemory client_memory;
    uint8_t *client_pixels;
    failed = mapped_buffer(physical, device, start + (VkDeviceSize)stride * IMAGE_HEIGHT, &client,
                           &client_memory, &client_pixels, 135);
    if (failed != 0)
        return failed;
    for (uint32_t row = 0; row < IMAGE_HEIGHT; row++)
        for (uint32_t column = 0; column < IMAGE_WIDTH; column++)
            ((uint32_t *)(client_pixels + start + row * stride))[column] = pattern(row, column);
    VkImage target = linear_image(device, 0);
    if (target == VK_NULL_HANDLE)
        return 137;
    vkGetImageMemoryRequirements(device, target, &needs);
    allocate.pNext = NULL;
    allocate.allocationSize = needs.size;
    allocate.memoryTypeIndex = visible_type(physical, needs.memoryTypeBits);
    VkDeviceMemory target_memory;
    if (allocate.memoryTypeIndex == UINT32_MAX ||
        vkAllocateMemory(device, &allocate, NULL, &target_memory) != VK_SUCCESS ||
        vkBindImageMemory(device, target, target_memory, 0) != VK_SUCCESS)
        return 138;
    struct copy to_image = {
        .buffer = client, .image = target, .offset = start, .row_length = stride / 4, .to_image = 1,
    };
    if (run_commands(device, queue, pool, record_copy, &to_image) != 0)
        return 139;
    vkGetImageSubresourceLayout(device, target, &colour, &layout);
    uint8_t *target_pixels;
    if (vkMapMemory(device, target_memory, 0, VK_WHOLE_SIZE, 0, (void **)&target_pixels) != VK_SUCCESS)
        return 140;
    for (uint32_t row = 0; row < IMAGE_HEIGHT; row++)
        for (uint32_t column = 0; column < IMAGE_WIDTH; column++)
            if (((uint32_t *)(target_pixels + layout.offset + row * layout.rowPitch))[column] !=
                pattern(row, column))
                return 141;

    vkDestroyCommandPool(device, pool, NULL);
    return 0;
}

int main(void)
{
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "vulkan-c",
        .apiVersion = VK_API_VERSION_1_3,
    };
    VkInstanceCreateInfo instance_info = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkInstance instance;
    if (vkCreateInstance(&instance_info, NULL, &instance) != VK_SUCCESS)
        return 10;

    /* The CPU driver, with Vulkan 1.3. */
    VkPhysicalDevice devices[8];
    uint32_t count = 8;
    if (vkEnumeratePhysicalDevices(instance, &count, devices) < 0 || count == 0)
        return 11;
    VkPhysicalDevice physical = VK_NULL_HANDLE;
    VkPhysicalDeviceProperties properties;
    for (uint32_t i = 0; i < count && physical == VK_NULL_HANDLE; i++) {
        vkGetPhysicalDeviceProperties(devices[i], &properties);
        if (properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_CPU)
            physical = devices[i];
    }
    if (physical == VK_NULL_HANDLE)
        return 12;
    if (properties.apiVersion < VK_API_VERSION_1_3)
        return 13;

    /* A queue that computes. */
    VkQueueFamilyProperties families[8];
    uint32_t family_count = 8;
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &family_count, families);
    uint32_t family = family_count;
    for (uint32_t i = 0; i < family_count; i++)
        if (families[i].queueFlags & VK_QUEUE_COMPUTE_BIT) {
            family = i;
            break;
        }
    if (family == family_count)
        return 20;
    float priority = 1.0f;
    VkDeviceQueueCreateInfo queue_info = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = family,
        .queueCount = 1,
        .pQueuePriorities = &priority,
    };
    if (!has_extensions(physical))
        return 22;
    VkDeviceCreateInfo device_info = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &queue_info,
        .enabledExtensionCount = EXTENSION_COUNT,
        .ppEnabledExtensionNames = EXTENSIONS,
    };
    VkDevice device;
    if (vkCreateDevice(physical, &device_info, NULL, &device) != VK_SUCCESS)
        return 21;
    get_memory_handle =
        (PFN_vkGetMemoryHandleMIKROS)vkGetDeviceProcAddr(device, "vkGetMemoryHandleMIKROS");
    memory_handle_properties = (PFN_vkGetMemoryHandlePropertiesMIKROS)vkGetDeviceProcAddr(
        device, "vkGetMemoryHandlePropertiesMIKROS");
    get_memory_fd = (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(device, "vkGetMemoryFdKHR");
    get_semaphore_handle = (PFN_vkGetSemaphoreHandleMIKROS)vkGetDeviceProcAddr(
        device, "vkGetSemaphoreHandleMIKROS");
    import_semaphore_handle = (PFN_vkImportSemaphoreHandleMIKROS)vkGetDeviceProcAddr(
        device, "vkImportSemaphoreHandleMIKROS");
    get_semaphore_fd = (PFN_vkGetSemaphoreFdKHR)vkGetDeviceProcAddr(device, "vkGetSemaphoreFdKHR");
    import_semaphore_fd =
        (PFN_vkImportSemaphoreFdKHR)vkGetDeviceProcAddr(device, "vkImportSemaphoreFdKHR");
    if (get_memory_handle == NULL || memory_handle_properties == NULL || get_memory_fd == NULL ||
        get_semaphore_handle == NULL || import_semaphore_handle == NULL ||
        get_semaphore_fd == NULL || import_semaphore_fd == NULL)
        return 23;
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);

    /* A storage buffer in host-visible, coherent memory, which every
     * handle type exports and imports. */
    VkPhysicalDeviceExternalBufferInfo external_info = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_BUFFER_INFO,
        .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
    };
    const VkExternalMemoryFeatureFlags shared =
        VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT | VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT;
    for (uint32_t bit = 1; bit != 0; bit <<= 1) {
        if (!(MEMORY_HANDLES & bit))
            continue;
        external_info.handleType = bit;
        VkExternalBufferProperties external = {
            .sType = VK_STRUCTURE_TYPE_EXTERNAL_BUFFER_PROPERTIES,
        };
        vkGetPhysicalDeviceExternalBufferProperties(physical, &external_info, &external);
        if ((external.externalMemoryProperties.externalMemoryFeatures & shared) != shared ||
            (external.externalMemoryProperties.compatibleHandleTypes & MEMORY_HANDLES) !=
                MEMORY_HANDLES)
            return 34;
    }
    VkExternalMemoryBufferCreateInfo external_buffer = {
        .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO,
        .handleTypes = MEMORY_HANDLES,
    };
    VkBufferCreateInfo buffer_info = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .pNext = &external_buffer,
        .size = COUNT * sizeof(uint32_t),
        .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
    };
    VkBuffer buffer;
    if (vkCreateBuffer(device, &buffer_info, NULL, &buffer) != VK_SUCCESS)
        return 30;
    VkMemoryRequirements needs;
    vkGetBufferMemoryRequirements(device, buffer, &needs);
    VkPhysicalDeviceMemoryProperties memory_properties;
    vkGetPhysicalDeviceMemoryProperties(physical, &memory_properties);
    const VkMemoryPropertyFlags visible =
        VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    uint32_t type = memory_properties.memoryTypeCount;
    for (uint32_t i = 0; i < memory_properties.memoryTypeCount; i++)
        if ((needs.memoryTypeBits & (1u << i)) &&
            (memory_properties.memoryTypes[i].propertyFlags & visible) == visible) {
            type = i;
            break;
        }
    if (type == memory_properties.memoryTypeCount)
        return 31;
    VkExportMemoryAllocateInfo export_info = {
        .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
        .handleTypes = MEMORY_HANDLES,
    };
    VkMemoryAllocateInfo allocate_info = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .pNext = &export_info,
        .allocationSize = needs.size,
        .memoryTypeIndex = type,
    };
    VkDeviceMemory memory;
    if (vkAllocateMemory(device, &allocate_info, NULL, &memory) != VK_SUCCESS)
        return 32;
    if (vkBindBufferMemory(device, buffer, memory, 0) != VK_SUCCESS)
        return 33;

    /* The pipeline of the shader, and its one buffer. */
    VkShaderModuleCreateInfo module_info = {
        .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        .codeSize = sizeof square_spv,
        .pCode = square_spv,
    };
    VkShaderModule module;
    if (vkCreateShaderModule(device, &module_info, NULL, &module) != VK_SUCCESS)
        return 40;
    VkDescriptorSetLayoutBinding binding = {
        .binding = 0,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .descriptorCount = 1,
        .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT,
    };
    VkDescriptorSetLayoutCreateInfo set_layout_info = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount = 1,
        .pBindings = &binding,
    };
    VkDescriptorSetLayout set_layout;
    if (vkCreateDescriptorSetLayout(device, &set_layout_info, NULL, &set_layout) != VK_SUCCESS)
        return 41;
    VkPipelineLayoutCreateInfo layout_info = {
        .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount = 1,
        .pSetLayouts = &set_layout,
    };
    VkPipelineLayout layout;
    if (vkCreatePipelineLayout(device, &layout_info, NULL, &layout) != VK_SUCCESS)
        return 42;
    VkComputePipelineCreateInfo pipeline_info = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage =
            {
                .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                .stage = VK_SHADER_STAGE_COMPUTE_BIT,
                .module = module,
                .pName = "main",
            },
        .layout = layout,
    };
    VkPipeline pipeline;
    if (vkCreateComputePipelines(device, VK_NULL_HANDLE, 1, &pipeline_info, NULL, &pipeline) !=
        VK_SUCCESS)
        return 43;
    VkDescriptorPoolSize pool_size = {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1};
    VkDescriptorPoolCreateInfo pool_info = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        .maxSets = 1,
        .poolSizeCount = 1,
        .pPoolSizes = &pool_size,
    };
    VkDescriptorPool pool;
    if (vkCreateDescriptorPool(device, &pool_info, NULL, &pool) != VK_SUCCESS)
        return 44;
    VkDescriptorSetAllocateInfo set_info = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        .descriptorPool = pool,
        .descriptorSetCount = 1,
        .pSetLayouts = &set_layout,
    };
    VkDescriptorSet set;
    if (vkAllocateDescriptorSets(device, &set_info, &set) != VK_SUCCESS)
        return 45;
    VkDescriptorBufferInfo described = {buffer, 0, VK_WHOLE_SIZE};
    VkWriteDescriptorSet write = {
        .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
        .dstSet = set,
        .dstBinding = 0,
        .descriptorCount = 1,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .pBufferInfo = &described,
    };
    vkUpdateDescriptorSets(device, 1, &write, 0, NULL);

    /* A timeline semaphore that every handle type exports and imports. */
    VkSemaphoreTypeCreateInfo timeline_type = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO,
        .semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE,
    };
    for (uint32_t bit = 1; bit != 0; bit <<= 1) {
        if (!(SEMAPHORE_HANDLES & bit))
            continue;
        VkPhysicalDeviceExternalSemaphoreInfo external_semaphore_info = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_SEMAPHORE_INFO,
            .pNext = &timeline_type,
            .handleType = bit,
        };
        VkExternalSemaphoreProperties external = {
            .sType = VK_STRUCTURE_TYPE_EXTERNAL_SEMAPHORE_PROPERTIES,
        };
        vkGetPhysicalDeviceExternalSemaphoreProperties(physical, &external_semaphore_info,
                                                       &external);
        const VkExternalSemaphoreFeatureFlags both =
            VK_EXTERNAL_SEMAPHORE_FEATURE_EXPORTABLE_BIT |
            VK_EXTERNAL_SEMAPHORE_FEATURE_IMPORTABLE_BIT;
        if ((external.externalSemaphoreFeatures & both) != both)
            return 46;
    }
    VkSemaphore semaphore = timeline(device, 0, SEMAPHORE_HANDLES);
    if (semaphore == VK_NULL_HANDLE)
        return 47;

    /* One dispatch, which signals the semaphore with 1. */
    VkCommandPoolCreateInfo command_pool_info = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = family,
    };
    VkCommandPool command_pool;
    if (vkCreateCommandPool(device, &command_pool_info, NULL, &command_pool) != VK_SUCCESS)
        return 50;
    VkCommandBufferAllocateInfo command_info = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = command_pool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    VkCommandBuffer commands;
    if (vkAllocateCommandBuffers(device, &command_info, &commands) != VK_SUCCESS)
        return 51;
    VkCommandBufferBeginInfo begin = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    if (vkBeginCommandBuffer(commands, &begin) != VK_SUCCESS)
        return 52;
    vkCmdBindPipeline(commands, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
    vkCmdBindDescriptorSets(commands, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &set, 0, NULL);
    vkCmdDispatch(commands, COUNT / GROUP, 1, 1);
    if (vkEndCommandBuffer(commands) != VK_SUCCESS)
        return 53;
    const uint64_t done = 1;
    VkTimelineSemaphoreSubmitInfo submit_values = {
        .sType = VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO,
        .signalSemaphoreValueCount = 1,
        .pSignalSemaphoreValues = &done,
    };
    VkSubmitInfo submit = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .pNext = &submit_values,
        .commandBufferCount = 1,
        .pCommandBuffers = &commands,
        .signalSemaphoreCount = 1,
        .pSignalSemaphores = &semaphore,
    };
    if (vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE) != VK_SUCCESS)
        return 54;
    if (!reaches(device, semaphore, done))
        return 55;

    /* The squares. */
    uint32_t *values;
    if (vkMapMemory(device, memory, 0, VK_WHOLE_SIZE, 0, (void **)&values) != VK_SUCCESS)
        return 60;
    for (uint32_t i = 0; i < COUNT; i++)
        if (values[i] != i * i)
            return 61;
    int failed = share_memory(device, memory, type, needs.size, values);
    if (failed != 0)
        return failed;
    vkUnmapMemory(device, memory);
    failed = share_timeline(device, queue, semaphore);
    if (failed != 0)
        return failed;
    if (vkQueueWaitIdle(queue) != VK_SUCCESS)
        return 56;
    failed = linear_images(physical, device, queue, family);
    if (failed != 0)
        return failed;

    vkDestroySemaphore(device, semaphore, NULL);
    vkDestroyCommandPool(device, command_pool, NULL);
    vkDestroyDescriptorPool(device, pool, NULL);
    vkDestroyPipeline(device, pipeline, NULL);
    vkDestroyPipelineLayout(device, layout, NULL);
    vkDestroyDescriptorSetLayout(device, set_layout, NULL);
    vkDestroyShaderModule(device, module, NULL);
    vkFreeMemory(device, memory, NULL);
    vkDestroyBuffer(device, buffer, NULL);
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    return 0;
}
