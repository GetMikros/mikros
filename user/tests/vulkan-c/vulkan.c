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
 * otherwise with the number of the first check that failed. */

#define VK_USE_PLATFORM_MIKROS
#include <mikros/startup.h>
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
