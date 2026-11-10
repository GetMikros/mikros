# SPDX-License-Identifier: GPL-2.0
#
# Only Kconfig runs for this architecture: xtask compiles the btrfs
# shim's objects itself. ../../include/generated/autoconf.h is what
# Kconfig makes of configs/mikros_defconfig, in a tree of the pinned
# Linux release with this directory as arch/mikros, with the LLVM that
# xtask builds:
#
#   make LLVM=<llvm>/bin/ CLANG_TARGET_FLAGS_mikros=x86_64-linux-gnu \
#       ARCH=mikros allnoconfig \
#       KCONFIG_ALLCONFIG=arch/mikros/configs/mikros_defconfig
#   make LLVM=<llvm>/bin/ CLANG_TARGET_FLAGS_mikros=x86_64-linux-gnu \
#       ARCH=mikros syncconfig
#
# and timeconst.h is `echo 100 | bc -q kernel/time/timeconst.bc`, for
# CONFIG_HZ.
KBUILD_DEFCONFIG := mikros_defconfig
