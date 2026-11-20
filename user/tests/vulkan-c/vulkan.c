/* A system test in C of Vulkan through the loader, which selftest runs as
 * it runs the others. It finds a physical device of the type CPU that
 * supports Vulkan 1.3, the CPU driver, and runs a compute shader that
 * writes the square of each index into a buffer of host-visible memory,
 * whose values it then checks. The shader, square.comp, is compiled to
 * SPIR-V on the build host into square.h. It exits with 0 when every
 * check passed, and otherwise with the number of the first check that
 * failed. */

#include <string.h>
#include <vulkan/vulkan.h>
/* After the headers that declare its types. */
#include "square.h"

enum { COUNT = 1024, GROUP = 64 };

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
    VkDeviceCreateInfo device_info = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &queue_info,
    };
    VkDevice device;
    if (vkCreateDevice(physical, &device_info, NULL, &device) != VK_SUCCESS)
        return 21;
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);

    /* A storage buffer in host-visible, coherent memory. */
    VkBufferCreateInfo buffer_info = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
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
    VkMemoryAllocateInfo allocate_info = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
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

    /* One dispatch, waited for. */
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
    VkSubmitInfo submit = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers = &commands,
    };
    if (vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE) != VK_SUCCESS)
        return 54;
    if (vkQueueWaitIdle(queue) != VK_SUCCESS)
        return 55;

    /* The squares. */
    uint32_t *values;
    if (vkMapMemory(device, memory, 0, VK_WHOLE_SIZE, 0, (void **)&values) != VK_SUCCESS)
        return 60;
    for (uint32_t i = 0; i < COUNT; i++)
        if (values[i] != i * i)
            return 61;
    vkUnmapMemory(device, memory);

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
