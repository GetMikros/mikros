// A compute shader run through Dawn's C API on the CPU Vulkan driver, which
// selftest runs as a system test: it squares 1024 numbers on the device and
// reads them back. It prints a line for each step, and exits with 0 when
// every number is right and with the number of the first step that failed
// otherwise, so that a failed run says how far Dawn got.
//
// Steps: 1 instance, 2 adapter, 3 device, 4 buffers, 5 shader, 6 pipeline,
// 7 bind group, 8 commands, 9 reading the results back, 10 a wrong result,
// 11 an error or a lost device that Dawn reported.

#include <dawn/webgpu.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

namespace {

constexpr uint32_t kCount = 1024;
constexpr uint64_t kBytes = kCount * sizeof(uint32_t);
// How long to wait for any one of Dawn's callbacks. selftest gives the whole
// test a minute, scaled by the speed of the machine, so each wait is short
// enough for a failed step to be reported before selftest stops the test.
constexpr int64_t kWaitMilliseconds = 12000;

const char kShader[] = R"(
@group(0) @binding(0) var<storage, read_write> values : array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id : vec3<u32>) {
    let i = id.x;
    if (i < arrayLength(&values)) {
        values[i] = values[i] * values[i];
    }
}
)";

// Set when a step is over, so that what Dawn says while the program takes
// its objects down is not counted as an error.
bool g_finished = false;
// Set when Dawn reports an error, or that the device is lost.
bool g_error = false;

WGPUStringView text(const char* s) {
    WGPUStringView view = {};
    view.data = s;
    view.length = WGPU_STRLEN;
    return view;
}

void print(const char* what, WGPUStringView message) {
    printf("webgpu: %s: %.*s\n", what, static_cast<int>(message.length),
           message.data ? message.data : "");
}

int fail(int step, const char* what) {
    printf("webgpu: failed at step %d: %s\n", step, what);
    return step;
}

void on_error(WGPUDevice const*, WGPUErrorType type, WGPUStringView message, void*, void*) {
    if (g_finished) {
        return;
    }
    g_error = true;
    printf("webgpu: uncaptured error of type %d\n", static_cast<int>(type));
    print("message", message);
}

void on_lost(WGPUDevice const*, WGPUDeviceLostReason reason, WGPUStringView message, void*,
             void*) {
    if (g_finished || reason == WGPUDeviceLostReason_Destroyed) {
        return;
    }
    g_error = true;
    printf("webgpu: device lost, reason %d\n", static_cast<int>(reason));
    print("message", message);
}

struct Adapter {
    WGPURequestAdapterStatus status = WGPURequestAdapterStatus_Error;
    WGPUAdapter adapter = nullptr;
    bool done = false;
};

void on_adapter(WGPURequestAdapterStatus status, WGPUAdapter adapter, WGPUStringView message,
                void* user, void*) {
    auto* result = static_cast<Adapter*>(user);
    result->status = status;
    result->adapter = adapter;
    result->done = true;
    if (status != WGPURequestAdapterStatus_Success) {
        print("adapter request", message);
    }
}

struct Device {
    WGPURequestDeviceStatus status = WGPURequestDeviceStatus_Error;
    WGPUDevice device = nullptr;
    bool done = false;
};

void on_device(WGPURequestDeviceStatus status, WGPUDevice device, WGPUStringView message,
               void* user, void*) {
    auto* result = static_cast<Device*>(user);
    result->status = status;
    result->device = device;
    result->done = true;
    if (status != WGPURequestDeviceStatus_Success) {
        print("device request", message);
    }
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

int64_t now_milliseconds() {
    timespec now = {};
    clock_gettime(CLOCK_MONOTONIC, &now);
    return static_cast<int64_t>(now.tv_sec) * 1000 + now.tv_nsec / 1000000;
}

// Lets Dawn run its callbacks until `done` is set, for at most
// kWaitMilliseconds by the clock, whatever a sleep takes, and says how long
// it took, so that a run on a slow machine shows how much room the wait has.
bool wait_for(WGPUInstance instance, const bool& done) {
    const int64_t start = now_milliseconds();
    const int64_t deadline = start + kWaitMilliseconds;
    for (;;) {
        wgpuInstanceProcessEvents(instance);
        if (done) {
            printf("webgpu: waited %lld ms\n", static_cast<long long>(now_milliseconds() - start));
            return true;
        }
        if (now_milliseconds() >= deadline) {
            return false;
        }
        timespec pause = {0, 1000000};
        nanosleep(&pause, nullptr);
    }
}

bool request_adapter(WGPUInstance instance, bool fallback, Adapter* result) {
    WGPURequestAdapterOptions options = {};
    options.backendType = WGPUBackendType_Vulkan;
    options.forceFallbackAdapter = fallback;
    WGPURequestAdapterCallbackInfo info = {};
    info.mode = WGPUCallbackMode_AllowProcessEvents;
    info.callback = on_adapter;
    info.userdata1 = result;
    const int64_t start = now_milliseconds();
    wgpuInstanceRequestAdapter(instance, &options, info);
    printf("webgpu: the adapter request took %lld ms to start\n",
           static_cast<long long>(now_milliseconds() - start));
    return wait_for(instance, result->done) && result->status == WGPURequestAdapterStatus_Success &&
           result->adapter != nullptr;
}

}  // namespace

int main() {
    // Unbuffered, so that a crash or a stop by selftest loses no line.
    setvbuf(stdout, nullptr, _IONBF, 0);
    printf("webgpu: start\n");

    WGPUInstance instance = wgpuCreateInstance(nullptr);
    if (!instance) {
        return fail(1, "wgpuCreateInstance returned nothing");
    }
    printf("webgpu: instance created\n");

    // The first request finds the CPU driver, which is slow to start: the
    // call itself takes seconds, and the wait for its answer none. The
    // fallback adapter is only a second chance if that finds nothing.
    Adapter adapter;
    if (!request_adapter(instance, false, &adapter)) {
        printf("webgpu: no Vulkan adapter, asking for the fallback one\n");
        adapter = Adapter();
        if (!request_adapter(instance, true, &adapter)) {
            return fail(2, "no adapter");
        }
        printf("webgpu: the adapter is the fallback one\n");
    }
    WGPUAdapterInfo about = {};
    if (wgpuAdapterGetInfo(adapter.adapter, &about) == WGPUStatus_Success) {
        printf("webgpu: adapter vendor %.*s, device %.*s, backend %d, type %d\n",
               static_cast<int>(about.vendor.length), about.vendor.data ? about.vendor.data : "",
               static_cast<int>(about.device.length), about.device.data ? about.device.data : "",
               static_cast<int>(about.backendType), static_cast<int>(about.adapterType));
        wgpuAdapterInfoFreeMembers(about);
    }

    WGPUDeviceDescriptor descriptor = {};
    descriptor.label = text("webgpu test");
    descriptor.deviceLostCallbackInfo.mode = WGPUCallbackMode_AllowProcessEvents;
    descriptor.deviceLostCallbackInfo.callback = on_lost;
    descriptor.uncapturedErrorCallbackInfo.callback = on_error;
    Device device;
    WGPURequestDeviceCallbackInfo device_info = {};
    device_info.mode = WGPUCallbackMode_AllowProcessEvents;
    device_info.callback = on_device;
    device_info.userdata1 = &device;
    wgpuAdapterRequestDevice(adapter.adapter, &descriptor, device_info);
    if (!wait_for(instance, device.done) || device.status != WGPURequestDeviceStatus_Success ||
        !device.device) {
        return fail(3, "no device");
    }
    printf("webgpu: device created\n");
    WGPUQueue queue = wgpuDeviceGetQueue(device.device);

    WGPUBufferDescriptor storage_descriptor = {};
    storage_descriptor.label = text("values");
    storage_descriptor.usage = WGPUBufferUsage_Storage | WGPUBufferUsage_CopySrc |
                               WGPUBufferUsage_CopyDst;
    storage_descriptor.size = kBytes;
    WGPUBuffer storage = wgpuDeviceCreateBuffer(device.device, &storage_descriptor);
    WGPUBufferDescriptor readback_descriptor = {};
    readback_descriptor.label = text("readback");
    readback_descriptor.usage = WGPUBufferUsage_MapRead | WGPUBufferUsage_CopyDst;
    readback_descriptor.size = kBytes;
    WGPUBuffer readback = wgpuDeviceCreateBuffer(device.device, &readback_descriptor);
    if (!storage || !readback) {
        return fail(4, "no buffers");
    }
    uint32_t input[kCount];
    for (uint32_t i = 0; i < kCount; i++) {
        input[i] = i;
    }
    wgpuQueueWriteBuffer(queue, storage, 0, input, kBytes);
    printf("webgpu: buffers created\n");

    WGPUShaderSourceWGSL wgsl = {};
    wgsl.chain.sType = WGPUSType_ShaderSourceWGSL;
    wgsl.code = text(kShader);
    WGPUShaderModuleDescriptor module_descriptor = {};
    module_descriptor.nextInChain = &wgsl.chain;
    module_descriptor.label = text("square");
    WGPUShaderModule module = wgpuDeviceCreateShaderModule(device.device, &module_descriptor);
    if (!module) {
        return fail(5, "no shader module");
    }
    printf("webgpu: shader module created\n");

    WGPUComputePipelineDescriptor pipeline_descriptor = {};
    pipeline_descriptor.label = text("square");
    pipeline_descriptor.compute.module = module;
    pipeline_descriptor.compute.entryPoint = text("main");
    WGPUComputePipeline pipeline =
        wgpuDeviceCreateComputePipeline(device.device, &pipeline_descriptor);
    if (!pipeline) {
        return fail(6, "no compute pipeline");
    }
    printf("webgpu: compute pipeline created\n");

    WGPUBindGroupLayout layout = wgpuComputePipelineGetBindGroupLayout(pipeline, 0);
    WGPUBindGroupEntry entry = {};
    entry.binding = 0;
    entry.buffer = storage;
    entry.offset = 0;
    entry.size = kBytes;
    WGPUBindGroupDescriptor group_descriptor = {};
    group_descriptor.label = text("values");
    group_descriptor.layout = layout;
    group_descriptor.entryCount = 1;
    group_descriptor.entries = &entry;
    WGPUBindGroup group = wgpuDeviceCreateBindGroup(device.device, &group_descriptor);
    if (!group) {
        return fail(7, "no bind group");
    }

    WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(device.device, nullptr);
    WGPUComputePassEncoder pass = wgpuCommandEncoderBeginComputePass(encoder, nullptr);
    wgpuComputePassEncoderSetPipeline(pass, pipeline);
    wgpuComputePassEncoderSetBindGroup(pass, 0, group, 0, nullptr);
    wgpuComputePassEncoderDispatchWorkgroups(pass, kCount / 64, 1, 1);
    wgpuComputePassEncoderEnd(pass);
    wgpuCommandEncoderCopyBufferToBuffer(encoder, storage, 0, readback, 0, kBytes);
    WGPUCommandBuffer commands = wgpuCommandEncoderFinish(encoder, nullptr);
    if (!commands) {
        return fail(8, "no command buffer");
    }
    wgpuQueueSubmit(queue, 1, &commands);
    printf("webgpu: commands submitted\n");

    Mapping mapping;
    WGPUBufferMapCallbackInfo map_info = {};
    map_info.mode = WGPUCallbackMode_AllowProcessEvents;
    map_info.callback = on_map;
    map_info.userdata1 = &mapping;
    wgpuBufferMapAsync(readback, WGPUMapMode_Read, 0, kBytes, map_info);
    if (!wait_for(instance, mapping.done) || mapping.status != WGPUMapAsyncStatus_Success) {
        return fail(9, "the results could not be read back");
    }
    const uint32_t* output =
        static_cast<const uint32_t*>(wgpuBufferGetConstMappedRange(readback, 0, kBytes));
    if (!output) {
        return fail(9, "no mapped range");
    }
    int wrong = 0;
    for (uint32_t i = 0; i < kCount; i++) {
        if (output[i] != i * i) {
            if (wrong < 4) {
                printf("webgpu: value %u is %u, not %u\n", i, output[i], i * i);
            }
            wrong++;
        }
    }
    wgpuBufferUnmap(readback);
    if (wrong != 0) {
        printf("webgpu: %d of %u values are wrong\n", wrong, kCount);
        return 10;
    }
    printf("webgpu: all %u values are right\n", kCount);

    g_finished = true;
    wgpuCommandBufferRelease(commands);
    wgpuComputePassEncoderRelease(pass);
    wgpuCommandEncoderRelease(encoder);
    wgpuBindGroupRelease(group);
    wgpuBindGroupLayoutRelease(layout);
    wgpuComputePipelineRelease(pipeline);
    wgpuShaderModuleRelease(module);
    wgpuBufferRelease(readback);
    wgpuBufferRelease(storage);
    wgpuQueueRelease(queue);
    wgpuDeviceRelease(device.device);
    wgpuAdapterRelease(adapter.adapter);
    wgpuInstanceRelease(instance);
    if (g_error) {
        return fail(11, "Dawn reported an error");
    }
    printf("webgpu: done\n");
    return 0;
}
