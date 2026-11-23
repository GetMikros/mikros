// A GPU image shared between Vulkan and Dawn on the CPU Vulkan driver, which
// selftest runs as a system test. A producer that uses Vulkan directly makes
// an image whose memory and a timeline semaphore can be exported, clears the
// image and signals the timeline to 1. A Dawn device in the same process
// imports the memory as shared texture memory and the timeline as a shared
// fence, begins access with the value 1, reads the pixels back, draws into
// the texture, and ends access. The producer imports the semaphore that
// Dawn exports, waits for the value that Dawn gives, and reads the pixels
// back. Then the producer clears the image again and signals 2, and a second
// round checks that the values grow and that Dawn's semaphore stays the same.
//
// It prints a line for each step and exits with 0 when every pixel is right,
// and with the number of the first step that failed otherwise.
//
// Steps: 1 Vulkan instance, 2 physical device, 3 device, 4 image and memory,
// 5 the memory's handle, 6 the timeline and its handle, 7 the producer's
// first clear, 10 Dawn instance, 11 adapter, 12 device, 13 features, 14 memory
// import, 15 fence import, 16 texture, 17 begin access, 20 first read, 21 a
// wrong first pixel, 22 drawing, 23 end access, 24 the exported semaphore,
// 25 the producer's wait, 26 a wrong pixel of the producer's read, 30 the
// producer's second clear, 31 second begin access, 32 a wrong second pixel,
// 33 second end access, 34 a value that did not grow, 35 the producer's second
// wait, 40 an error or a lost device that Dawn reported.

#include <dawn/webgpu.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

#include "producer.h"

namespace {

constexpr int64_t kWaitMilliseconds = 12000;

// What the image holds after each clear and drawing, as the bytes of the
// format B8G8R8A8: red from the producer's first clear, green from Dawn's pass,
// and blue from the producer's second clear.
constexpr uint8_t kRed[4] = {0, 0, 255, 255};
constexpr uint8_t kBlue[4] = {255, 0, 0, 255};
constexpr uint8_t kGreen[4] = {0, 255, 0, 255};

bool g_finished = false;
bool g_error = false;

int64_t now_milliseconds() {
    timespec now = {};
    clock_gettime(CLOCK_MONOTONIC, &now);
    return static_cast<int64_t>(now.tv_sec) * 1000 + now.tv_nsec / 1000000;
}

WGPUStringView text(const char* s) {
    WGPUStringView view = {};
    view.data = s;
    view.length = WGPU_STRLEN;
    return view;
}

void print(const char* what, WGPUStringView message) {
    printf("dawn-shared: %s: %.*s\n", what, static_cast<int>(message.length),
           message.data ? message.data : "");
}

void on_error(WGPUDevice const*, WGPUErrorType type, WGPUStringView message, void*, void*) {
    if (g_finished) {
        return;
    }
    g_error = true;
    printf("dawn-shared: uncaptured error of type %d\n", static_cast<int>(type));
    print("message", message);
}

void on_lost(WGPUDevice const*, WGPUDeviceLostReason reason, WGPUStringView message, void*,
             void*) {
    if (g_finished || reason == WGPUDeviceLostReason_Destroyed) {
        return;
    }
    g_error = true;
    printf("dawn-shared: device lost, reason %d\n", static_cast<int>(reason));
    print("message", message);
}

struct Adapter {
    WGPUAdapter adapter = nullptr;
    bool done = false;
};

void on_adapter(WGPURequestAdapterStatus status, WGPUAdapter adapter, WGPUStringView message,
                void* user, void*) {
    auto* result = static_cast<Adapter*>(user);
    if (status == WGPURequestAdapterStatus_Success) {
        result->adapter = adapter;
    } else {
        print("adapter request", message);
    }
    result->done = true;
}

struct Device {
    WGPUDevice device = nullptr;
    bool done = false;
};

void on_device(WGPURequestDeviceStatus status, WGPUDevice device, WGPUStringView message,
               void* user, void*) {
    auto* result = static_cast<Device*>(user);
    if (status == WGPURequestDeviceStatus_Success) {
        result->device = device;
    } else {
        print("device request", message);
    }
    result->done = true;
}

struct Mapping {
    WGPUMapAsyncStatus status = WGPUMapAsyncStatus_Error;
    bool done = false;
};

void on_map(WGPUMapAsyncStatus status, WGPUStringView message, void* user, void*) {
    auto* result = static_cast<Mapping*>(user);
    result->status = status;
    result->done = true;
    if (status != WGPUMapAsyncStatus_Success) {
        print("map", message);
    }
}

bool wait_for(WGPUInstance instance, const bool& done) {
    const int64_t deadline = now_milliseconds() + kWaitMilliseconds;
    for (;;) {
        wgpuInstanceProcessEvents(instance);
        if (done) {
            return true;
        }
        if (now_milliseconds() >= deadline) {
            return false;
        }
        timespec pause = {0, 1000000};
        nanosleep(&pause, nullptr);
    }
}

struct Consumer {
    WGPUInstance instance = nullptr;
    WGPUAdapter adapter = nullptr;
    WGPUDevice device = nullptr;
    WGPUQueue queue = nullptr;
    WGPUBuffer readback = nullptr;
    WGPUSharedTextureMemory memory = nullptr;
    WGPUSharedFence fence = nullptr;
    WGPUTexture texture = nullptr;
};

// Begins access to the texture after the fence's value `value`, with the
// layouts that the producer left the image in.
bool begin_access(Consumer& c, uint64_t value, int32_t layout) {
    WGPUSharedTextureMemoryVkImageLayoutBeginState layouts = {};
    layouts.chain.sType = WGPUSType_SharedTextureMemoryVkImageLayoutBeginState;
    layouts.oldLayout = layout;
    layouts.newLayout = layout;
    WGPUSharedTextureMemoryBeginAccessDescriptor begin = {};
    begin.nextInChain = &layouts.chain;
    begin.initialized = true;
    begin.fenceCount = 1;
    begin.fences = &c.fence;
    begin.signaledValueCount = 1;
    begin.signaledValues = &value;
    return wgpuSharedTextureMemoryBeginAccess(c.memory, c.texture, &begin) == WGPUStatus_Success;
}

// Copies the texture into the buffer and counts the pixels that are not
// `expected`.
bool consumer_pixels_are(Consumer& c, const uint8_t (&expected)[4], int* wrong) {
    WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(c.device, nullptr);
    WGPUTexelCopyTextureInfo source = {};
    source.texture = c.texture;
    source.aspect = WGPUTextureAspect_All;
    WGPUTexelCopyBufferInfo destination = {};
    destination.buffer = c.readback;
    destination.layout.bytesPerRow = kRowBytes;
    destination.layout.rowsPerImage = kSize;
    WGPUExtent3D size = {kSize, kSize, 1};
    wgpuCommandEncoderCopyTextureToBuffer(encoder, &source, &destination, &size);
    WGPUCommandBuffer commands = wgpuCommandEncoderFinish(encoder, nullptr);
    if (!commands) {
        return false;
    }
    wgpuQueueSubmit(c.queue, 1, &commands);
    wgpuCommandBufferRelease(commands);
    wgpuCommandEncoderRelease(encoder);

    Mapping mapping;
    WGPUBufferMapCallbackInfo info = {};
    info.mode = WGPUCallbackMode_AllowProcessEvents;
    info.callback = on_map;
    info.userdata1 = &mapping;
    wgpuBufferMapAsync(c.readback, WGPUMapMode_Read, 0, kImageBytes, info);
    if (!wait_for(c.instance, mapping.done) || mapping.status != WGPUMapAsyncStatus_Success) {
        return false;
    }
    const uint8_t* bytes = static_cast<const uint8_t*>(
        wgpuBufferGetConstMappedRange(c.readback, 0, kImageBytes));
    if (!bytes) {
        return false;
    }
    *wrong = 0;
    for (uint32_t i = 0; i < kSize * kSize; i++) {
        if (memcmp(bytes + i * 4, expected, 4) != 0) {
            if (*wrong < 2) {
                printf("dawn-shared: Dawn's pixel %u is %u %u %u %u\n", i, bytes[i * 4],
                       bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]);
            }
            (*wrong)++;
        }
    }
    wgpuBufferUnmap(c.readback);
    return true;
}

// A render pass that clears the texture to green.
bool draw_green(Consumer& c) {
    WGPUTextureView view = wgpuTextureCreateView(c.texture, nullptr);
    if (!view) {
        return false;
    }
    WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(c.device, nullptr);
    WGPURenderPassColorAttachment attachment = {};
    attachment.view = view;
    attachment.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    attachment.loadOp = WGPULoadOp_Clear;
    attachment.storeOp = WGPUStoreOp_Store;
    attachment.clearValue = {0.0, 1.0, 0.0, 1.0};
    WGPURenderPassDescriptor descriptor = {};
    descriptor.colorAttachmentCount = 1;
    descriptor.colorAttachments = &attachment;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(encoder, &descriptor);
    wgpuRenderPassEncoderEnd(pass);
    WGPUCommandBuffer commands = wgpuCommandEncoderFinish(encoder, nullptr);
    if (!commands) {
        return false;
    }
    wgpuQueueSubmit(c.queue, 1, &commands);
    wgpuCommandBufferRelease(commands);
    wgpuRenderPassEncoderRelease(pass);
    wgpuCommandEncoderRelease(encoder);
    wgpuTextureViewRelease(view);
    return true;
}

// Ends access and exports what Dawn gives: the handle of its semaphore, which
// stays open, the value to wait for and the layout to read the image from.
bool end_access(Consumer& c, int* handle, uint64_t* value, int32_t* layout) {
    WGPUSharedTextureMemoryVkImageLayoutEndState layouts = {};
    layouts.chain.sType = WGPUSType_SharedTextureMemoryVkImageLayoutEndState;
    WGPUSharedTextureMemoryEndAccessState end = {};
    end.nextInChain = &layouts.chain;
    if (wgpuSharedTextureMemoryEndAccess(c.memory, c.texture, &end) != WGPUStatus_Success ||
        end.fenceCount != 1 || end.signaledValueCount != 1) {
        printf("dawn-shared: end access gave %zu fences and %zu values\n", end.fenceCount,
               end.signaledValueCount);
        return false;
    }
    WGPUSharedFenceVkSemaphoreOpaqueFDExportInfo fd = {};
    fd.chain.sType = WGPUSType_SharedFenceVkSemaphoreOpaqueFDExportInfo;
    fd.handle = -1;
    WGPUSharedFenceExportInfo info = {};
    info.nextInChain = &fd.chain;
    wgpuSharedFenceExportInfo(end.fences[0], &info);
    const bool ok = info.type == WGPUSharedFenceType_VkSemaphoreOpaqueFD && fd.handle >= 0;
    if (ok) {
        // The handle is Dawn's: the producer works on a duplicate.
        *handle = dup(fd.handle);
        *value = end.signaledValues[0];
        *layout = layouts.newLayout;
    }
    wgpuSharedTextureMemoryEndAccessStateFreeMembers(end);
    return ok && *handle >= 0;
}

}  // namespace

int main() {
    setvbuf(stdout, nullptr, _IONBF, 0);
    printf("dawn-shared: start\n");

    Producer producer;
    int memoryHandle = -1;
    int timelineHandle = -1;
    if (int step = make_producer(producer, &memoryHandle, &timelineHandle)) {
        return step;
    }

    Consumer c;
    c.instance = wgpuCreateInstance(nullptr);
    if (!c.instance) {
        return fail(10, "wgpuCreateInstance returned nothing");
    }
    Adapter adapter;
    WGPURequestAdapterOptions options = {};
    options.backendType = WGPUBackendType_Vulkan;
    WGPURequestAdapterCallbackInfo adapterInfo = {};
    adapterInfo.mode = WGPUCallbackMode_AllowProcessEvents;
    adapterInfo.callback = on_adapter;
    adapterInfo.userdata1 = &adapter;
    wgpuInstanceRequestAdapter(c.instance, &options, adapterInfo);
    if (!wait_for(c.instance, adapter.done) || !adapter.adapter) {
        return fail(11, "no adapter");
    }
    c.adapter = adapter.adapter;
    const WGPUFeatureName features[] = {WGPUFeatureName_SharedTextureMemoryOpaqueFD,
                                        WGPUFeatureName_SharedFenceVkSemaphoreOpaqueFD};
    for (WGPUFeatureName feature : features) {
        if (!wgpuAdapterHasFeature(c.adapter, feature)) {
            printf("dawn-shared: the adapter lacks the feature 0x%x\n",
                   static_cast<unsigned>(feature));
            return fail(13, "the adapter does not offer the shared texture and fence features");
        }
    }
    printf("dawn-shared: the adapter offers the shared texture and fence features\n");

    WGPUDeviceDescriptor descriptor = {};
    descriptor.label = text("dawn-shared");
    descriptor.requiredFeatureCount = 2;
    descriptor.requiredFeatures = features;
    descriptor.deviceLostCallbackInfo.mode = WGPUCallbackMode_AllowProcessEvents;
    descriptor.deviceLostCallbackInfo.callback = on_lost;
    descriptor.uncapturedErrorCallbackInfo.callback = on_error;
    Device device;
    WGPURequestDeviceCallbackInfo deviceInfo = {};
    deviceInfo.mode = WGPUCallbackMode_AllowProcessEvents;
    deviceInfo.callback = on_device;
    deviceInfo.userdata1 = &device;
    wgpuAdapterRequestDevice(c.adapter, &descriptor, deviceInfo);
    if (!wait_for(c.instance, device.done) || !device.device) {
        return fail(12, "no device");
    }
    c.device = device.device;
    c.queue = wgpuDeviceGetQueue(c.device);
    printf("dawn-shared: Dawn device created\n");

    WGPUBufferDescriptor bufferDescriptor = {};
    bufferDescriptor.label = text("readback");
    bufferDescriptor.usage = WGPUBufferUsage_MapRead | WGPUBufferUsage_CopyDst;
    bufferDescriptor.size = kImageBytes;
    c.readback = wgpuDeviceCreateBuffer(c.device, &bufferDescriptor);

    WGPUSharedTextureMemoryOpaqueFDDescriptor memoryDescriptor = {};
    memoryDescriptor.chain.sType = WGPUSType_SharedTextureMemoryOpaqueFDDescriptor;
    memoryDescriptor.vkImageCreateInfo = &producer.imageInfo;
    memoryDescriptor.memoryFD = memoryHandle;
    memoryDescriptor.memoryTypeIndex = producer.memoryTypeIndex;
    memoryDescriptor.allocationSize = producer.allocationSize;
    memoryDescriptor.dedicatedAllocation = true;
    WGPUSharedTextureMemoryDescriptor memoryImport = {};
    memoryImport.nextInChain = &memoryDescriptor.chain;
    memoryImport.label = text("producer image");
    c.memory = wgpuDeviceImportSharedTextureMemory(c.device, &memoryImport);
    if (!c.memory) {
        return fail(14, "the memory was not imported");
    }
    printf("dawn-shared: the memory was imported\n");

    // The fence takes the descriptor: the producer keeps its own.
    WGPUSharedFenceVkSemaphoreOpaqueFDDescriptor fenceDescriptor = {};
    fenceDescriptor.chain.sType = WGPUSType_SharedFenceVkSemaphoreOpaqueFDDescriptor;
    fenceDescriptor.handle = dup(timelineHandle);
    WGPUSharedFenceDescriptor fenceImport = {};
    fenceImport.nextInChain = &fenceDescriptor.chain;
    fenceImport.label = text("producer timeline");
    c.fence = wgpuDeviceImportSharedFence(c.device, &fenceImport);
    if (!c.fence) {
        return fail(15, "the timeline was not imported as a fence");
    }
    c.texture = wgpuSharedTextureMemoryCreateTexture(c.memory, nullptr);
    if (!c.texture) {
        return fail(16, "no texture");
    }
    printf("dawn-shared: the timeline was imported and the texture created\n");

    const int32_t readable = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    if (!begin_access(c, 1, readable)) {
        return fail(17, "begin access failed");
    }
    int wrong = 0;
    if (!consumer_pixels_are(c, kRed, &wrong)) {
        return fail(20, "Dawn could not read the pixels");
    }
    if (wrong != 0) {
        printf("dawn-shared: %d pixels are not red\n", wrong);
        return fail(21, "Dawn did not see the producer's clear");
    }
    printf("dawn-shared: Dawn read the producer's red pixels\n");

    if (!draw_green(c)) {
        return fail(22, "the drawing failed");
    }
    int handle = -1;
    uint64_t value = 0;
    int32_t layout = 0;
    if (!end_access(c, &handle, &value, &layout)) {
        return fail(23, "end access failed");
    }
    printf("dawn-shared: end access gave the value %llu, handle %d, layout %d\n",
           static_cast<unsigned long long>(value), handle, layout);
    if (!producer_waits(producer, handle, value)) {
        return fail(25, "the producer could not wait for Dawn's semaphore");
    }
    close(handle);
    if (!producer_pixels_are(producer, static_cast<VkImageLayout>(layout), kGreen, &wrong)) {
        return fail(25, "the producer could not read the pixels");
    }
    if (wrong != 0) {
        printf("dawn-shared: %d pixels are not green\n", wrong);
        return fail(26, "the producer did not see Dawn's drawing");
    }
    printf("dawn-shared: the producer read Dawn's green pixels\n");

    // A second round, in which the producer signals the value 2.
    // The producer's read left the image in the layout of a transfer source.
    if (!producer_clear(producer, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, 0.0f, 0.0f, 1.0f, 2)) {
        return fail(30, "the second clear was not submitted");
    }
    if (!begin_access(c, 2, readable)) {
        return fail(31, "the second begin access failed");
    }
    if (!consumer_pixels_are(c, kBlue, &wrong)) {
        return fail(32, "Dawn could not read the pixels again");
    }
    if (wrong != 0) {
        printf("dawn-shared: %d pixels are not blue\n", wrong);
        return fail(32, "Dawn did not see the second clear");
    }
    printf("dawn-shared: Dawn read the producer's blue pixels\n");
    int second_handle = -1;
    uint64_t second_value = 0;
    int32_t second_layout = 0;
    if (!end_access(c, &second_handle, &second_value, &second_layout)) {
        return fail(33, "the second end access failed");
    }
    printf("dawn-shared: the second end access gave the value %llu\n",
           static_cast<unsigned long long>(second_value));
    if (second_value <= value) {
        return fail(34, "the value did not grow");
    }
    if (!producer_waits(producer, second_handle, second_value)) {
        return fail(35, "the producer could not wait for the second value");
    }
    close(second_handle);
    printf("dawn-shared: the producer waited for the value %llu\n",
           static_cast<unsigned long long>(second_value));

    g_finished = true;
    wgpuTextureRelease(c.texture);
    wgpuSharedFenceRelease(c.fence);
    wgpuSharedTextureMemoryRelease(c.memory);
    wgpuBufferRelease(c.readback);
    wgpuQueueRelease(c.queue);
    wgpuDeviceRelease(c.device);
    wgpuAdapterRelease(c.adapter);
    wgpuInstanceRelease(c.instance);
    if (g_error) {
        return fail(40, "Dawn reported an error");
    }
    printf("dawn-shared: done\n");
    return 0;
}
