/* The system calls that the C bindings' runtime makes, which the tests
 * answer from the mock kernel in src/mock.rs. */

#ifndef MIKROS_SYSCALLS_H
#define MIKROS_SYSCALLS_H

#include <stdint.h>

typedef uint32_t mikros_handle_t;
typedef int64_t mikros_status_t;

mikros_status_t mikros_handle_close(mikros_handle_t handle);
mikros_status_t mikros_handle_replace(mikros_handle_t handle, uint32_t rights, mikros_handle_t *out);
mikros_status_t mikros_object_get_info(mikros_handle_t handle, uint32_t topic, uint8_t *buf,
                                       uint64_t len, uint64_t *out_len);
mikros_status_t mikros_object_wait_one(mikros_handle_t handle, uint32_t signals, int64_t deadline,
                                       uint32_t *observed);
mikros_status_t mikros_channel_write(mikros_handle_t channel, const uint8_t *bytes, uint64_t len,
                                     const mikros_handle_t *handles, uint64_t count);
mikros_status_t mikros_channel_read(mikros_handle_t channel, uint8_t *bytes, uint64_t bytes_cap,
                                    mikros_handle_t *handles, uint64_t handles_cap,
                                    uint64_t *out_len, uint64_t *out_count);
mikros_status_t mikros_channel_call(mikros_handle_t channel, uint32_t options, int64_t deadline,
                                    const uint8_t *call_args, uint8_t *out);

#endif
