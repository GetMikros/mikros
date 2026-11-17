/* The service functions of the C library (MIKSPEC-0068.R26), through
   which applications reach services, register their own protocols,
   control their own services, read their own log records, and use their
   public folders and components. Each returns the status of the call it
   makes; strings are NUL-terminated UTF-8, and one longer than the bound
   of its call fails with INVALID_ARGS. */

#ifndef MIKROS_SERVICES_H
#define MIKROS_SERVICES_H

#include <stdbool.h>
#include <stdint.h>
#include <mikros/syscalls.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The identity of a client of a connection. */
typedef struct {
    uint32_t pid;
    uint32_t user;
    char app_id[129];
    uint64_t koid;
} mikros_client_t;

/* A service's entry of the service manager's list. */
typedef struct {
    uint32_t state;
    uint32_t pid;
    uint32_t restarts;
    int32_t exit_status;
    bool enabled;
} mikros_service_status_t;

/* A record of the log. */
typedef struct {
    uint64_t sequence;
    int64_t timestamp;
    int64_t utc;
    uint32_t pid;
    char app_id[129];
    uint32_t severity;
    char message[1025];
} mikros_log_record_t;

/* Connects server, a channel end that it takes over in every case, to
   protocol, and to its named instance unless instance is NULL. */
mikros_status_t mikros_service_connect(const char *protocol,
                                       const char *instance,
                                       mikros_handle_t server);

/* Registers instance of protocol, named under the caller's app ID, and
   returns the end on which its connections come in provider. */
mikros_status_t mikros_service_register(const char *protocol,
                                        const char *instance,
                                        mikros_handle_t *provider);

/* Removes the caller's instance of protocol. */
mikros_status_t mikros_service_unregister(const char *protocol,
                                          const char *instance);

/* Returns a service's own provider end in provider; NOT_FOUND without
   one. */
mikros_status_t mikros_service_provider(mikros_handle_t *provider);

/* Waits for the next connection on provider and returns its protocol,
   its instance, empty for none, its server end and its client. */
mikros_status_t mikros_service_accept(mikros_handle_t provider,
                                      char protocol[129], char instance[65],
                                      mikros_handle_t *server,
                                      mikros_client_t *client);

/* Returns the service manager's entry of the service name; NOT_FOUND
   without one. */
mikros_status_t mikros_service_status(const char *name,
                                      mikros_service_status_t *status);

/* Start, stop and restart the service name. */
mikros_status_t mikros_service_start(const char *name);
mikros_status_t mikros_service_stop(const char *name);
mikros_status_t mikros_service_restart(const char *name);

/* Copies at most max of the caller's own log records after the sequence
   number after into records, and their number into count. */
mikros_status_t mikros_own_log_read(uint64_t after, mikros_log_record_t *records,
                                uint32_t max, uint32_t *count);

/* Creates the caller's public folder. */
mikros_status_t mikros_public_create(void);

/* Opens the public folder of the app id on server, which it takes
   over. */
mikros_status_t mikros_public_open(const char *id, mikros_handle_t server);

/* Installs the left-out optional component of the caller's app whose
   package ID is component. */
mikros_status_t mikros_component_add(const char *component);

/* Installs the component package file package, which it takes over, of
   the caller's app, or updates the installed one to it. */
mikros_status_t mikros_component_install(mikros_handle_t package);

#ifdef __cplusplus
}
#endif

#endif
