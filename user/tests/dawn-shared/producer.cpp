// The producer of the test dawn_shared, in Vulkan.

#include "producer.h"

#include <string.h>
#include <unistd.h>

namespace {

// How long the producer waits for the semaphore of Dawn.
constexpr uint64_t kWaitNanoseconds = 30ull * 1000 * 1000 * 1000;

uint32_t find_memory_type(const Producer& p, uint32_t bits, VkMemoryPropertyFlags wanted) {
    VkPhysicalDeviceMemoryProperties properties;
    vkGetPhysicalDeviceMemoryProperties(p.physical, &properties);
    for (uint32_t i = 0; i < properties.memoryTypeCount; i++) {
        if ((bits & (1u << i)) != 0 &&
            (properties.memoryTypes[i].propertyFlags & wanted) == wanted) {
            return i;
        }
    }
    for (uint32_t i = 0; i < properties.memoryTypeCount; i++) {
        if ((bits & (1u << i)) != 0) {
            return i;
        }
    }
    return UINT32_MAX;
}

VkSemaphore make_timeline(const Producer& p, bool exportable) {
    VkExportSemaphoreCreateInfo exportInfo = {};
    exportInfo.sType = VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO;
    exportInfo.handleTypes = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;
    VkSemaphoreTypeCreateInfo typeInfo = {};
    typeInfo.sType = VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO;
    typeInfo.pNext = exportable ? &exportInfo : nullptr;
    typeInfo.semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE;
    typeInfo.initialValue = 0;
    VkSemaphoreCreateInfo info = {};
    info.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO;
    info.pNext = &typeInfo;
    VkSemaphore semaphore = VK_NULL_HANDLE;
    if (vkCreateSemaphore(p.device, &info, nullptr, &semaphore) != VK_SUCCESS) {
        return VK_NULL_HANDLE;
    }
    return semaphore;
}

void barrier(VkCommandBuffer commands, VkImage image, VkImageLayout from, VkImageLayout to,
             VkAccessFlags source, VkAccessFlags destination) {
    VkImageMemoryBarrier b = {};
    b.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    b.srcAccessMask = source;
    b.dstAccessMask = destination;
    b.oldLayout = from;
    b.newLayout = to;
    b.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b.image = image;
    b.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                         VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 0, nullptr, 0, nullptr, 1, &b);
}

// Records and submits one command buffer. With `signalValue` not 0 the submit
// signals the producer's timeline to it.
bool submit(Producer& p, bool (*record)(Producer&, VkCommandBuffer, const void*), const void* data,
            uint64_t signalValue) {
    VkCommandBufferAllocateInfo allocate = {};
    allocate.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
    allocate.commandPool = p.pool;
    allocate.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    allocate.commandBufferCount = 1;
    VkCommandBuffer commands = VK_NULL_HANDLE;
    if (vkAllocateCommandBuffers(p.device, &allocate, &commands) != VK_SUCCESS) {
        return false;
    }
    VkCommandBufferBeginInfo begin = {};
    begin.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    if (vkBeginCommandBuffer(commands, &begin) != VK_SUCCESS || !record(p, commands, data) ||
        vkEndCommandBuffer(commands) != VK_SUCCESS) {
        return false;
    }

    VkTimelineSemaphoreSubmitInfo timelineInfo = {};
    timelineInfo.sType = VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO;
    timelineInfo.signalSemaphoreValueCount = 1;
    timelineInfo.pSignalSemaphoreValues = &signalValue;
    VkSubmitInfo info = {};
    info.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
    info.commandBufferCount = 1;
    info.pCommandBuffers = &commands;
    if (signalValue != 0) {
        info.pNext = &timelineInfo;
        info.signalSemaphoreCount = 1;
        info.pSignalSemaphores = &p.timeline;
    }
    return vkQueueSubmit(p.queue, 1, &info, VK_NULL_HANDLE) == VK_SUCCESS;
}

struct Clear {
    VkImageLayout from;
    float red, green, blue;
};

bool record_clear(Producer& p, VkCommandBuffer commands, const void* data) {
    const Clear& clear = *static_cast<const Clear*>(data);
    barrier(commands, p.image, clear.from, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 0,
            VK_ACCESS_TRANSFER_WRITE_BIT);
    VkClearColorValue color = {};
    color.float32[0] = clear.red;
    color.float32[1] = clear.green;
    color.float32[2] = clear.blue;
    color.float32[3] = 1.0f;
    VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    vkCmdClearColorImage(commands, p.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &color, 1, &range);
    barrier(commands, p.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_ACCESS_TRANSFER_WRITE_BIT,
            VK_ACCESS_SHADER_READ_BIT);
    return true;
}

bool record_read(Producer& p, VkCommandBuffer commands, const void* data) {
    const VkImageLayout from = *static_cast<const VkImageLayout*>(data);
    barrier(commands, p.image, from, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_MEMORY_WRITE_BIT,
            VK_ACCESS_TRANSFER_READ_BIT);
    VkBufferImageCopy region = {};
    region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.imageExtent = {kSize, kSize, 1};
    vkCmdCopyImageToBuffer(commands, p.image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, p.readback, 1,
                           &region);
    return true;
}

}  // namespace

bool producer_clear(Producer& p, VkImageLayout from, float red, float green, float blue,
                    uint64_t value) {
    Clear clear = {from, red, green, blue};
    return submit(p, record_clear, &clear, value);
}

bool producer_pixels_are(Producer& p, VkImageLayout from, const uint8_t (&expected)[4],
                         int* wrong) {
    if (!submit(p, record_read, &from, 0) || vkQueueWaitIdle(p.queue) != VK_SUCCESS) {
        return false;
    }
    void* mapped = nullptr;
    if (vkMapMemory(p.device, p.readbackMemory, 0, kImageBytes, 0, &mapped) != VK_SUCCESS) {
        return false;
    }
    const uint8_t* bytes = static_cast<const uint8_t*>(mapped);
    *wrong = 0;
    for (uint32_t i = 0; i < kSize * kSize; i++) {
        if (memcmp(bytes + i * 4, expected, 4) != 0) {
            if (*wrong < 2) {
                printf("dawn-shared: producer pixel %u is %u %u %u %u\n", i, bytes[i * 4],
                       bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]);
            }
            (*wrong)++;
        }
    }
    vkUnmapMemory(p.device, p.readbackMemory);
    return true;
}

bool producer_waits(Producer& p, int handle, uint64_t value) {
    if (p.imported == VK_NULL_HANDLE) {
        p.imported = make_timeline(p, false);
        if (p.imported == VK_NULL_HANDLE) {
            return false;
        }
        VkImportSemaphoreFdInfoKHR import = {};
        import.sType = VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_FD_INFO_KHR;
        import.semaphore = p.imported;
        import.flags = 0;
        import.handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;
        // The import takes the descriptor.
        import.fd = dup(handle);
        if (import.fd < 0 || p.importSemaphoreFd(p.device, &import) != VK_SUCCESS) {
            printf("dawn-shared: the import of Dawn's semaphore failed\n");
            return false;
        }
    }
    VkSemaphoreWaitInfo wait = {};
    wait.sType = VK_STRUCTURE_TYPE_SEMAPHORE_WAIT_INFO;
    wait.semaphoreCount = 1;
    wait.pSemaphores = &p.imported;
    wait.pValues = &value;
    const VkResult result = p.waitSemaphores(p.device, &wait, kWaitNanoseconds);
    if (result != VK_SUCCESS) {
        printf("dawn-shared: waiting for the value %llu gave %d\n",
               static_cast<unsigned long long>(value), static_cast<int>(result));
        return false;
    }
    return true;
}

int make_producer(Producer& p, int* memoryHandle, int* timelineHandle) {
    VkApplicationInfo app = {};
    app.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO;
    app.pApplicationName = "dawn-shared";
    app.apiVersion = VK_API_VERSION_1_1;
    VkInstanceCreateInfo instanceInfo = {};
    instanceInfo.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO;
    instanceInfo.pApplicationInfo = &app;
    if (vkCreateInstance(&instanceInfo, nullptr, &p.instance) != VK_SUCCESS) {
        return fail(1, "no Vulkan instance");
    }
    printf("dawn-shared: Vulkan instance created\n");

    uint32_t count = 1;
    if (vkEnumeratePhysicalDevices(p.instance, &count, &p.physical) < 0 || count == 0) {
        return fail(2, "no physical device");
    }
    uint32_t families = 1;
    VkQueueFamilyProperties family = {};
    vkGetPhysicalDeviceQueueFamilyProperties(p.physical, &families, &family);
    if (families == 0 || (family.queueFlags & VK_QUEUE_GRAPHICS_BIT) == 0) {
        return fail(2, "no graphics queue");
    }

    const char* extensions[] = {
        VK_KHR_EXTERNAL_MEMORY_EXTENSION_NAME,
        VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
        VK_KHR_EXTERNAL_SEMAPHORE_EXTENSION_NAME,
        VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME,
        VK_KHR_TIMELINE_SEMAPHORE_EXTENSION_NAME,
    };
    VkPhysicalDeviceTimelineSemaphoreFeatures timelineFeatures = {};
    timelineFeatures.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES;
    timelineFeatures.timelineSemaphore = VK_TRUE;
    const float priority = 1.0f;
    VkDeviceQueueCreateInfo queueInfo = {};
    queueInfo.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO;
    queueInfo.queueFamilyIndex = 0;
    queueInfo.queueCount = 1;
    queueInfo.pQueuePriorities = &priority;
    VkDeviceCreateInfo deviceInfo = {};
    deviceInfo.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO;
    deviceInfo.pNext = &timelineFeatures;
    deviceInfo.queueCreateInfoCount = 1;
    deviceInfo.pQueueCreateInfos = &queueInfo;
    deviceInfo.enabledExtensionCount = sizeof extensions / sizeof extensions[0];
    deviceInfo.ppEnabledExtensionNames = extensions;
    VkResult made = vkCreateDevice(p.physical, &deviceInfo, nullptr, &p.device);
    if (made != VK_SUCCESS) {
        printf("dawn-shared: vkCreateDevice gave %d\n", static_cast<int>(made));
        return fail(3, "no device with the external memory and semaphore extensions");
    }
    vkGetDeviceQueue(p.device, 0, 0, &p.queue);
    p.getMemoryFd = reinterpret_cast<PFN_vkGetMemoryFdKHR>(
        vkGetDeviceProcAddr(p.device, "vkGetMemoryFdKHR"));
    p.getSemaphoreFd = reinterpret_cast<PFN_vkGetSemaphoreFdKHR>(
        vkGetDeviceProcAddr(p.device, "vkGetSemaphoreFdKHR"));
    p.importSemaphoreFd = reinterpret_cast<PFN_vkImportSemaphoreFdKHR>(
        vkGetDeviceProcAddr(p.device, "vkImportSemaphoreFdKHR"));
    p.waitSemaphores = reinterpret_cast<PFN_vkWaitSemaphoresKHR>(
        vkGetDeviceProcAddr(p.device, "vkWaitSemaphoresKHR"));
    if (!p.getMemoryFd || !p.getSemaphoreFd || !p.importSemaphoreFd || !p.waitSemaphores) {
        return fail(3, "a function of the extensions is missing");
    }
    VkCommandPoolCreateInfo poolInfo = {};
    poolInfo.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
    poolInfo.queueFamilyIndex = 0;
    if (vkCreateCommandPool(p.device, &poolInfo, nullptr, &p.pool) != VK_SUCCESS) {
        return fail(3, "no command pool");
    }
    printf("dawn-shared: Vulkan device created\n");

    // The image Dawn will import: optimal tiling, with the usages that the
    // texture needs. Its create info is also what Dawn is given.
    p.externalInfo.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
    p.externalInfo.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
    p.viewFormats[0] = VK_FORMAT_B8G8R8A8_UNORM;
    p.viewFormats[1] = VK_FORMAT_B8G8R8A8_SRGB;
    p.formatList.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO;
    p.formatList.viewFormatCount = 2;
    p.formatList.pViewFormats = p.viewFormats;
    p.externalInfo.pNext = &p.formatList;
    p.imageInfo.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    p.imageInfo.pNext = &p.externalInfo;
    p.imageInfo.flags = VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
    p.imageInfo.imageType = VK_IMAGE_TYPE_2D;
    p.imageInfo.format = VK_FORMAT_B8G8R8A8_UNORM;
    p.imageInfo.extent = {kSize, kSize, 1};
    p.imageInfo.mipLevels = 1;
    p.imageInfo.arrayLayers = 1;
    p.imageInfo.samples = VK_SAMPLE_COUNT_1_BIT;
    p.imageInfo.tiling = VK_IMAGE_TILING_OPTIMAL;
    p.imageInfo.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
                        VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    p.imageInfo.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    p.imageInfo.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    if (vkCreateImage(p.device, &p.imageInfo, nullptr, &p.image) != VK_SUCCESS) {
        return fail(4, "no image");
    }
    VkMemoryRequirements requirements;
    vkGetImageMemoryRequirements(p.device, p.image, &requirements);
    p.memoryTypeIndex =
        find_memory_type(p, requirements.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    if (p.memoryTypeIndex == UINT32_MAX) {
        return fail(4, "no memory type");
    }
    p.allocationSize = requirements.size;
    VkMemoryDedicatedAllocateInfo dedicated = {};
    dedicated.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
    dedicated.image = p.image;
    VkExportMemoryAllocateInfo exportMemory = {};
    exportMemory.sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO;
    exportMemory.pNext = &dedicated;
    exportMemory.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
    VkMemoryAllocateInfo allocate = {};
    allocate.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate.pNext = &exportMemory;
    allocate.allocationSize = p.allocationSize;
    allocate.memoryTypeIndex = p.memoryTypeIndex;
    made = vkAllocateMemory(p.device, &allocate, nullptr, &p.memory);
    if (made != VK_SUCCESS || vkBindImageMemory(p.device, p.image, p.memory, 0) != VK_SUCCESS) {
        printf("dawn-shared: vkAllocateMemory gave %d\n", static_cast<int>(made));
        return fail(4, "no exportable memory");
    }
    printf("dawn-shared: image and memory created, %llu bytes\n",
           static_cast<unsigned long long>(p.allocationSize));

    VkMemoryGetFdInfoKHR memoryFd = {};
    memoryFd.sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR;
    memoryFd.memory = p.memory;
    memoryFd.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
    if (p.getMemoryFd(p.device, &memoryFd, memoryHandle) != VK_SUCCESS || *memoryHandle < 0) {
        return fail(5, "the memory has no handle");
    }
    printf("dawn-shared: memory handle %d\n", *memoryHandle);

    p.timeline = make_timeline(p, true);
    VkSemaphoreGetFdInfoKHR semaphoreFd = {};
    semaphoreFd.sType = VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR;
    semaphoreFd.semaphore = p.timeline;
    semaphoreFd.handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;
    if (p.timeline == VK_NULL_HANDLE ||
        p.getSemaphoreFd(p.device, &semaphoreFd, timelineHandle) != VK_SUCCESS ||
        *timelineHandle < 0) {
        return fail(6, "the timeline has no handle");
    }
    printf("dawn-shared: timeline handle %d\n", *timelineHandle);

    // The buffer that the producer reads the image into.
    VkBufferCreateInfo bufferInfo = {};
    bufferInfo.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO;
    bufferInfo.size = kImageBytes;
    bufferInfo.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    bufferInfo.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    if (vkCreateBuffer(p.device, &bufferInfo, nullptr, &p.readback) != VK_SUCCESS) {
        return fail(7, "no readback buffer");
    }
    VkMemoryRequirements bufferRequirements;
    vkGetBufferMemoryRequirements(p.device, p.readback, &bufferRequirements);
    VkMemoryAllocateInfo bufferAllocate = {};
    bufferAllocate.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    bufferAllocate.allocationSize = bufferRequirements.size;
    bufferAllocate.memoryTypeIndex =
        find_memory_type(p, bufferRequirements.memoryTypeBits,
                         VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    if (vkAllocateMemory(p.device, &bufferAllocate, nullptr, &p.readbackMemory) != VK_SUCCESS ||
        vkBindBufferMemory(p.device, p.readback, p.readbackMemory, 0) != VK_SUCCESS) {
        return fail(7, "no readback memory");
    }

    // The first clear, to red, which the timeline's value 1 says is done.
    if (!producer_clear(p, VK_IMAGE_LAYOUT_UNDEFINED, 1.0f, 0.0f, 0.0f, 1)) {
        return fail(7, "the first clear was not submitted");
    }
    printf("dawn-shared: the producer cleared the image to red and signaled the value 1\n");
    return 0;
}

