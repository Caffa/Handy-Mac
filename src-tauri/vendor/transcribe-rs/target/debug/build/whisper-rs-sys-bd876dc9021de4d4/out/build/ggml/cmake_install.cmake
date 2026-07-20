# Install script for directory: /Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml

# Set the install prefix
if(NOT DEFINED CMAKE_INSTALL_PREFIX)
  set(CMAKE_INSTALL_PREFIX "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out")
endif()
string(REGEX REPLACE "/$" "" CMAKE_INSTALL_PREFIX "${CMAKE_INSTALL_PREFIX}")

# Set the install configuration name.
if(NOT DEFINED CMAKE_INSTALL_CONFIG_NAME)
  if(BUILD_TYPE)
    string(REGEX REPLACE "^[^A-Za-z0-9_]+" ""
           CMAKE_INSTALL_CONFIG_NAME "${BUILD_TYPE}")
  else()
    set(CMAKE_INSTALL_CONFIG_NAME "RelWithDebInfo")
  endif()
  message(STATUS "Install configuration: \"${CMAKE_INSTALL_CONFIG_NAME}\"")
endif()

# Set the component getting installed.
if(NOT CMAKE_INSTALL_COMPONENT)
  if(COMPONENT)
    message(STATUS "Install component: \"${COMPONENT}\"")
    set(CMAKE_INSTALL_COMPONENT "${COMPONENT}")
  else()
    set(CMAKE_INSTALL_COMPONENT)
  endif()
endif()

# Is this installation the result of a crosscompile?
if(NOT DEFINED CMAKE_CROSSCOMPILING)
  set(CMAKE_CROSSCOMPILING "FALSE")
endif()

# Set path to fallback-tool for dependency-resolution.
if(NOT DEFINED CMAKE_OBJDUMP)
  set(CMAKE_OBJDUMP "/usr/bin/objdump")
endif()

if(NOT CMAKE_INSTALL_LOCAL_ONLY)
  # Include the install script for the subdirectory.
  include("/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/build/ggml/src/cmake_install.cmake")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE STATIC_LIBRARY FILES "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/build/ggml/src/libggml.a")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libggml.a" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libggml.a")
    execute_process(COMMAND "/usr/bin/ranlib" "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libggml.a")
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include" TYPE FILE FILES
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-cpu.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-alloc.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-backend.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-blas.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-cann.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-cpp.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-cuda.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-opt.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-metal.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-rpc.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-sycl.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-vulkan.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-webgpu.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/ggml-zendnn.h"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/whisper.cpp/ggml/include/gguf.h"
    )
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE STATIC_LIBRARY FILES "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/build/ggml/src/libggml-base.a")
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libggml-base.a" AND
     NOT IS_SYMLINK "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libggml-base.a")
    execute_process(COMMAND "/usr/bin/ranlib" "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libggml-base.a")
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib/cmake/ggml" TYPE FILE FILES
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/build/ggml/ggml-config.cmake"
    "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/build/ggml/ggml-version.cmake"
    )
endif()

string(REPLACE ";" "\n" CMAKE_INSTALL_MANIFEST_CONTENT
       "${CMAKE_INSTALL_MANIFEST_FILES}")
if(CMAKE_INSTALL_LOCAL_ONLY)
  file(WRITE "/Users/caffae/Local-Projects-2026/Handy-Fork/Handy-Mac/src-tauri/vendor/transcribe-rs/target/debug/build/whisper-rs-sys-bd876dc9021de4d4/out/build/ggml/install_local_manifest.txt"
     "${CMAKE_INSTALL_MANIFEST_CONTENT}")
endif()
