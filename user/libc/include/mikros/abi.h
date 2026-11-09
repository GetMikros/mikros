/* The ABI level of the running system. */

#ifndef MIKROS_ABI_H
#define MIKROS_ABI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* MIKSPEC-0068.R19 */
uint32_t mikros_abi_level(void);

#ifdef __cplusplus
}
#endif

#endif
