# Built by cargo xtask with the sysroot's CMake toolchain file.
cmake_minimum_required(VERSION 4.0)
project(tally C)

add_library(tally-cmake SHARED tally.c)
target_compile_definitions(tally-cmake PRIVATE BUILD_SYSTEM="cmake")
set_target_properties(tally-cmake PROPERTIES VERSION 1.2.3 SOVERSION 1)
add_library(base-cmake SHARED base.c)
add_library(count-cmake STATIC count.c)
add_executable(tally-cmake-user main.c)
target_link_libraries(tally-cmake-user tally-cmake base-cmake count-cmake)
install(TARGETS tally-cmake base-cmake count-cmake tally-cmake-user)

# Libraries in the sysroot are found with their Mikros suffixes: the
# shared one where there is one, and the static one of a library that
# has only that.
find_library(MIKROS_LIBC c REQUIRED)
find_library(MIKROS_UNWIND unwind REQUIRED)
find_library(MIKROS_EXPERIMENTAL c++experimental REQUIRED)
if(NOT MIKROS_LIBC MATCHES "/libraries/libc[.]dlib$" OR
   NOT MIKROS_UNWIND MATCHES "/libraries/libunwind[.]dlib$" OR
   NOT MIKROS_EXPERIMENTAL MATCHES "/libraries/libc[+][+]experimental[.]slib$")
  message(FATAL_ERROR
    "found ${MIKROS_LIBC}, ${MIKROS_UNWIND} and ${MIKROS_EXPERIMENTAL}")
endif()
