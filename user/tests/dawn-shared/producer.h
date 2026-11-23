// The producer of the test dawn_shared: Vulkan used directly, to make an image
// whose memory and a timeline semaphore can be exported, to clear the image,
// and to wait for and read what Dawn does with it.

#ifndef USER_TESTS_DAWN_SHARED_PRODUCER_H_
#define USER_TESTS_DAWN_SHARED_PRODUCER_H_

#include <stdint.h>
#include <stdio.h>
#include <vulkan/vulkan.h>

constexpr uint32_t kSize = 64;
// The row of a copy to a buffer is a multiple of 256 bytes, and 64 BGRA pixels
// are 256.
constexpr uint32_t kRowBytes = kSize * 4;
constexpr uint64_t kImageBytes = static_cast<uint64_t>(kRowBytes) * kSize;

// Prints that step `step` failed, and returns the step, which is the exit
// status of the test.
inline int fail(int step, const char* what) {
    printf("dawn-shared: failed at step %d: %s\n", step, what);
    return step;
}

struct Producer {
    VkInstance instance = VK_NULL_HANDLE;
    VkPhysicalDevice physical = VK_NULL_HANDLE;
    VkDevice device = VK_NULL_HANDLE;
    VkQueue queue = VK_NULL_HANDLE;
    VkCommandPool pool = VK_NULL_HANDLE;
    // What Dawn is given to import the image: the create info and the
    // allocation of the image's memory.
    VkImageCreateInfo imageInfo = {};
    VkExternalMemoryImageCreateInfo externalInfo = {};
    // The formats that a view of the image may have: Dawn reinterprets a BGRA8
    // texture as sRGB, so the image is created mutable, with both formats.
    VkImageFormatListCreateInfo formatList = {};
    VkFormat viewFormats[2] = {};
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    VkDeviceSize allocationSize = 0;
    uint32_t memoryTypeIndex = 0;
    // The timeline that the producer signals, and the semaphore that imports
    // the one that Dawn exports.
    VkSemaphore timeline = VK_NULL_HANDLE;
    VkSemaphore imported = VK_NULL_HANDLE;
    VkBuffer readback = VK_NULL_HANDLE;
    VkDeviceMemory readbackMemory = VK_NULL_HANDLE;
    PFN_vkGetMemoryFdKHR getMemoryFd = nullptr;
    PFN_vkGetSemaphoreFdKHR getSemaphoreFd = nullptr;
    PFN_vkImportSemaphoreFdKHR importSemaphoreFd = nullptr;
    PFN_vkWaitSemaphoresKHR waitSemaphores = nullptr;
};

// Steps 1 to 7 of the test: the device, the image with its memory and the
// timeline, the handles of those, and a first clear of the image to red that
// signals the timeline to 1. Returns 0, or the step that failed.
int make_producer(Producer& p, int* memoryHandle, int* timelineHandle);

// Clears the image, which is in the layout `from`, to the colour, and signals
// the timeline to `value`. The image is left in a layout that a shader reads.
bool producer_clear(Producer& p, VkImageLayout from, float red, float green, float blue,
                    uint64_t value);

// Reads the image back through Vulkan, from the layout `from`, and counts the
// pixels that are not `expected`.
bool producer_pixels_are(Producer& p, VkImageLayout from, const uint8_t (&expected)[4],
                         int* wrong);

// Imports the semaphore that Dawn exports into a timeline of the producer, the
// first time, and waits for `value` on it.
bool producer_waits(Producer& p, int handle, uint64_t value);

#endif  // USER_TESTS_DAWN_SHARED_PRODUCER_H_
