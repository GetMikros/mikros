// The Dawn part of the Dawn run's desktop run: the switcher part of the
// system app system.mikros.shell_probe, which the compositor starts in a
// user session with its Wayland channel as the USER startup handle 1. It
// opens a window with SDL3's Wayland driver and gives Dawn, through its
// C API, the window's Wayland surface. Dawn presents on the CPU Vulkan
// driver: the frame is cleared to one colour, and a triangle that covers
// all of it is drawn in another with a render pipeline, so that the desktop
// run, which looks for the second colour on every pixel of the window,
// fails when only the clear worked. Then it serves the connection until the
// compositor closes it.
//
// It writes a line to standard error as each stage starts, and a failure is
// also its exit status, which the compositor logs: 11 the channel, 20 SDL's
// start, 21 the window, 22 the window's Wayland pointers, 30 to 37 Dawn up
// to the surface and its capabilities, 40 and 41 the shader and the
// pipeline, 50 to 53 the frame. A watchdog thread exits with 100 plus the
// number of the stage that hung, and stops once the frame is presented.
// (MIKSPEC-0063.R21)

#include <dawn/webgpu.h>

#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include <atomic>

#include <SDL3/SDL.h>
#include <mikros/startup.h>
#include <mikros/syscalls.h>
#include <wayland-client.h>

namespace {

enum { ROLE_USER = 16, WAYLAND_ID = 1, KIND_CHANNEL = 9 };

// The size of the window, which the desktop run looks for in the corner of
// the screen where the compositor puts a toplevel.
constexpr int kWidth = 128;
constexpr int kHeight = 96;
// How long one wait for one of Dawn's callbacks may take, and how long the
// whole run may take before the watchdog ends it. The CPU driver takes about
// ten seconds to start.
constexpr int64_t kWaitMilliseconds = 40000;
constexpr int64_t kWatchdogMilliseconds = 150000;

// The colour that the frame is cleared to, and the colour of the triangle
// that covers it, whose bytes, 30, 190 and 90, are exact in the format.
constexpr WGPUColor kClear = {1.0, 0.0, 1.0, 1.0};
const char kShader[] = R"(
@vertex
fn vs(@builtin(vertex_index) i : u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(corners[i], 0.0, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(30.0 / 255.0, 190.0 / 255.0, 90.0 / 255.0, 1.0);
}
)";

// The stage that the program is in, which is the exit status of its failure,
// for the watchdog.
std::atomic<int> g_stage{1};
// Set once the frame is presented, which ends the watchdog.
std::atomic<bool> g_presented{false};

int64_t now_milliseconds() {
    timespec now = {};
    clock_gettime(CLOCK_MONOTONIC, &now);
    return static_cast<int64_t>(now.tv_sec) * 1000 + now.tv_nsec / 1000000;
}

void* watchdog(void*) {
    const int64_t deadline = now_milliseconds() + kWatchdogMilliseconds;
    for (;;) {
        timespec pause = {1, 0};
        nanosleep(&pause, nullptr);
        if (g_presented) {
            return nullptr;
        }
        if (now_milliseconds() >= deadline) {
            _exit(100 + g_stage.load());
        }
    }
}

int stage(int number) {
    g_stage = number;
    fprintf(stderr, "dawn-sdl: stage %d\n", number);
    return number;
}

WGPUStringView text(const char* s) {
    WGPUStringView view = {};
    view.data = s;
    view.length = WGPU_STRLEN;
    return view;
}

std::atomic<bool> g_error{false};

void on_error(WGPUDevice const*, WGPUErrorType, WGPUStringView, void*, void*) {
    g_error = true;
}

void on_lost(WGPUDevice const*, WGPUDeviceLostReason reason, WGPUStringView, void*, void*) {
    if (reason != WGPUDeviceLostReason_Destroyed) {
        g_error = true;
    }
}

struct Adapter {
    WGPUAdapter adapter = nullptr;
    bool done = false;
};

void on_adapter(WGPURequestAdapterStatus status, WGPUAdapter adapter, WGPUStringView, void* user,
                void*) {
    auto* result = static_cast<Adapter*>(user);
    if (status == WGPURequestAdapterStatus_Success) {
        result->adapter = adapter;
    }
    result->done = true;
}

struct Device {
    WGPUDevice device = nullptr;
    bool done = false;
};

void on_device(WGPURequestDeviceStatus status, WGPUDevice device, WGPUStringView, void* user,
               void*) {
    auto* result = static_cast<Device*>(user);
    if (status == WGPURequestDeviceStatus_Success) {
        result->device = device;
    }
    result->done = true;
}

// Lets Dawn run its callbacks until `done` is set, for at most
// kWaitMilliseconds by the clock.
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

// Dawn on the Wayland surface `surface` of `display`, one frame presented.
int draw(void* display, void* surface, int width, int height) {
    stage(30);
    WGPUInstance instance = wgpuCreateInstance(nullptr);
    if (!instance) {
        return 30;
    }

    stage(31);
    Adapter adapter;
    WGPURequestAdapterOptions options = {};
    options.backendType = WGPUBackendType_Vulkan;
    WGPURequestAdapterCallbackInfo adapter_info = {};
    adapter_info.mode = WGPUCallbackMode_AllowProcessEvents;
    adapter_info.callback = on_adapter;
    adapter_info.userdata1 = &adapter;
    wgpuInstanceRequestAdapter(instance, &options, adapter_info);
    if (!wait_for(instance, adapter.done) || !adapter.adapter) {
        return 31;
    }

    stage(32);
    Device device;
    WGPUDeviceDescriptor descriptor = {};
    descriptor.label = text("dawn-sdl");
    descriptor.deviceLostCallbackInfo.mode = WGPUCallbackMode_AllowProcessEvents;
    descriptor.deviceLostCallbackInfo.callback = on_lost;
    descriptor.uncapturedErrorCallbackInfo.callback = on_error;
    WGPURequestDeviceCallbackInfo device_info = {};
    device_info.mode = WGPUCallbackMode_AllowProcessEvents;
    device_info.callback = on_device;
    device_info.userdata1 = &device;
    wgpuAdapterRequestDevice(adapter.adapter, &descriptor, device_info);
    if (!wait_for(instance, device.done) || !device.device) {
        return 32;
    }
    WGPUQueue queue = wgpuDeviceGetQueue(device.device);

    stage(33);
    WGPUSurfaceSourceWaylandSurface wayland = {};
    wayland.chain.sType = WGPUSType_SurfaceSourceWaylandSurface;
    wayland.display = display;
    wayland.surface = surface;
    WGPUSurfaceDescriptor surface_descriptor = {};
    surface_descriptor.nextInChain = &wayland.chain;
    surface_descriptor.label = text("window");
    WGPUSurface target = wgpuInstanceCreateSurface(instance, &surface_descriptor);
    if (!target) {
        return 33;
    }

    stage(34);
    WGPUSurfaceCapabilities capabilities = {};
    if (wgpuSurfaceGetCapabilities(target, adapter.adapter, &capabilities) != WGPUStatus_Success) {
        return 34;
    }
    bool format = false, opaque = false;
    for (size_t i = 0; i < capabilities.formatCount; i++) {
        format |= capabilities.formats[i] == WGPUTextureFormat_BGRA8Unorm;
    }
    for (size_t i = 0; i < capabilities.alphaModeCount; i++) {
        opaque |= capabilities.alphaModes[i] == WGPUCompositeAlphaMode_Opaque;
    }
    wgpuSurfaceCapabilitiesFreeMembers(capabilities);
    if (!format) {
        return stage(35);
    }
    if (!opaque) {
        return stage(36);
    }

    stage(37);
    WGPUSurfaceConfiguration configuration = {};
    configuration.device = device.device;
    configuration.format = WGPUTextureFormat_BGRA8Unorm;
    configuration.usage = WGPUTextureUsage_RenderAttachment;
    configuration.width = static_cast<uint32_t>(width);
    configuration.height = static_cast<uint32_t>(height);
    configuration.alphaMode = WGPUCompositeAlphaMode_Opaque;
    configuration.presentMode = WGPUPresentMode_Fifo;
    wgpuSurfaceConfigure(target, &configuration);

    stage(40);
    WGPUShaderSourceWGSL wgsl = {};
    wgsl.chain.sType = WGPUSType_ShaderSourceWGSL;
    wgsl.code = text(kShader);
    WGPUShaderModuleDescriptor module_descriptor = {};
    module_descriptor.nextInChain = &wgsl.chain;
    WGPUShaderModule module = wgpuDeviceCreateShaderModule(device.device, &module_descriptor);
    if (!module) {
        return 40;
    }

    stage(41);
    WGPUColorTargetState color_target = {};
    color_target.format = WGPUTextureFormat_BGRA8Unorm;
    color_target.writeMask = WGPUColorWriteMask_All;
    WGPUFragmentState fragment = {};
    fragment.module = module;
    fragment.entryPoint = text("fs");
    fragment.targetCount = 1;
    fragment.targets = &color_target;
    WGPURenderPipelineDescriptor pipeline_descriptor = {};
    pipeline_descriptor.vertex.module = module;
    pipeline_descriptor.vertex.entryPoint = text("vs");
    pipeline_descriptor.primitive.topology = WGPUPrimitiveTopology_TriangleList;
    pipeline_descriptor.primitive.frontFace = WGPUFrontFace_CCW;
    pipeline_descriptor.primitive.cullMode = WGPUCullMode_None;
    pipeline_descriptor.multisample.count = 1;
    pipeline_descriptor.multisample.mask = 0xFFFFFFFF;
    pipeline_descriptor.fragment = &fragment;
    WGPURenderPipeline pipeline = wgpuDeviceCreateRenderPipeline(device.device, &pipeline_descriptor);
    if (!pipeline) {
        return 41;
    }

    stage(50);
    WGPUSurfaceTexture current = {};
    wgpuSurfaceGetCurrentTexture(target, &current);
    if (!current.texture || (current.status != WGPUSurfaceGetCurrentTextureStatus_SuccessOptimal &&
                             current.status != WGPUSurfaceGetCurrentTextureStatus_SuccessSuboptimal)) {
        return 50;
    }
    WGPUTextureView view = wgpuTextureCreateView(current.texture, nullptr);
    if (!view) {
        return 50;
    }

    stage(51);
    WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(device.device, nullptr);
    WGPURenderPassColorAttachment attachment = {};
    attachment.view = view;
    attachment.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    attachment.loadOp = WGPULoadOp_Clear;
    attachment.storeOp = WGPUStoreOp_Store;
    attachment.clearValue = kClear;
    WGPURenderPassDescriptor pass_descriptor = {};
    pass_descriptor.colorAttachmentCount = 1;
    pass_descriptor.colorAttachments = &attachment;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(encoder, &pass_descriptor);
    wgpuRenderPassEncoderSetPipeline(pass, pipeline);
    wgpuRenderPassEncoderDraw(pass, 3, 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    WGPUCommandBuffer commands = wgpuCommandEncoderFinish(encoder, nullptr);
    if (!commands) {
        return 51;
    }
    wgpuQueueSubmit(queue, 1, &commands);

    stage(52);
    if (wgpuSurfacePresent(target) != WGPUStatus_Success) {
        return 52;
    }
    g_presented = true;
    // An error that the device reported on the way is a failure, whatever
    // the pixels turn out to be.
    wgpuInstanceProcessEvents(instance);
    if (g_error) {
        return stage(53);
    }
    // Nothing of Dawn's is released: the window stays until the compositor
    // closes the connection.
    return 0;
}

}  // namespace

int main() {
    pthread_t thread;
    if (pthread_create(&thread, nullptr, watchdog, nullptr) == 0) {
        pthread_detach(thread);
    }

    stage(11);
    mikros_handle_t channel = mikros_take_startup_handle(ROLE_USER, WAYLAND_ID);
    int fd = channel == 0 ? -1 : mikros_fd_from_handle(channel, KIND_CHANNEL);
    wl_display* display = fd < 0 ? nullptr : wl_display_connect_to_fd(fd);
    if (!display) {
        return 11;
    }

    stage(20);
    SDL_SetHint(SDL_HINT_VIDEO_DRIVER, "wayland");
    SDL_SetPointerProperty(SDL_GetGlobalProperties(), SDL_PROP_GLOBAL_VIDEO_WAYLAND_WL_DISPLAY_POINTER,
                           display);
    if (!SDL_Init(SDL_INIT_VIDEO)) {
        return 20;
    }
    stage(21);
    SDL_Window* window = SDL_CreateWindow("dawn-sdl", kWidth, kHeight, SDL_WINDOW_VULKAN);
    if (!window) {
        return 21;
    }
    stage(22);
    SDL_PropertiesID properties = SDL_GetWindowProperties(window);
    void* wl_display_pointer =
        SDL_GetPointerProperty(properties, SDL_PROP_WINDOW_WAYLAND_DISPLAY_POINTER, nullptr);
    void* wl_surface_pointer =
        SDL_GetPointerProperty(properties, SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER, nullptr);
    if (!wl_display_pointer || !wl_surface_pointer) {
        return 22;
    }
    int width = 0, height = 0;
    SDL_GetWindowSizeInPixels(window, &width, &height);

    int failed = draw(wl_display_pointer, wl_surface_pointer, width, height);
    if (failed) {
        return failed;
    }
    // SDL is pumped, as it needs to be for the window to stay mapped.
    for (;;) {
        SDL_Event event;
        while (SDL_PollEvent(&event)) {
            if (event.type == SDL_EVENT_QUIT) {
                return 0;
            }
        }
        if (wl_display_get_error(display) != 0) {
            return 0;
        }
        SDL_Delay(50);
    }
}
