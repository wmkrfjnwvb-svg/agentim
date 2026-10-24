# Builds the two native libraries the Dart side opens: agentim_client (the
# Rust core and runtime, through cargo) and agentim_dart (the FFI shim).
cmake_minimum_required(VERSION 3.10)
project(agentim_flutter_library LANGUAGES C)

# Flutter reaches the plugin through a symlink: paths start from the real one.
get_filename_component(HERE "${CMAKE_CURRENT_SOURCE_DIR}" REALPATH)
set(AGENTIM_RUST "${HERE}/../../../client-rust")
set(CARGO_ENV "")
if (ANDROID)
  # The Rust target for this ABI, linked by the NDK's compiler, 16 KB pages.
  if (CMAKE_ANDROID_ARCH_ABI STREQUAL "arm64-v8a")
    set(RUST_TARGET aarch64-linux-android)
  elseif (CMAKE_ANDROID_ARCH_ABI STREQUAL "armeabi-v7a")
    set(RUST_TARGET armv7-linux-androideabi)
  elseif (CMAKE_ANDROID_ARCH_ABI STREQUAL "x86_64")
    set(RUST_TARGET x86_64-linux-android)
  else()
    set(RUST_TARGET i686-linux-android)
  endif()
  string(TOUPPER "${RUST_TARGET}" T)
  string(REPLACE "-" "_" T "${T}")
  string(REPLACE "-" "_" t "${RUST_TARGET}")
  set(CARGO_ENV
    "CARGO_TARGET_${T}_LINKER=${CMAKE_C_COMPILER}"
    "CC_${t}=${CMAKE_C_COMPILER}"
    "AR_${t}=${CMAKE_AR}"
    "CARGO_TARGET_${T}_RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=--target=${CMAKE_C_COMPILER_TARGET}")
  set(CARGO_TARGET_ARG --target ${RUST_TARGET})
  set(RUST_OUT "${CMAKE_CURRENT_BINARY_DIR}/cargo/${RUST_TARGET}/release")
else()
  set(CARGO_TARGET_ARG "")
  set(RUST_OUT "${CMAKE_CURRENT_BINARY_DIR}/cargo/release")
endif()
set(RUST_LIB "${RUST_OUT}/${CMAKE_SHARED_LIBRARY_PREFIX}agentim_client${CMAKE_SHARED_LIBRARY_SUFFIX}")

# Always asked: cargo itself knows what changed.
add_custom_target(agentim_client_build ALL
  COMMAND ${CMAKE_COMMAND} -E env ${CARGO_ENV}
          cargo build --release -p agentim-client ${CARGO_TARGET_ARG}
          --manifest-path "${AGENTIM_RUST}/Cargo.toml"
          --target-dir "${CMAKE_CURRENT_BINARY_DIR}/cargo"
  BYPRODUCTS "${RUST_LIB}"
  COMMENT "cargo build -p agentim-client ${CARGO_TARGET_ARG}"
  VERBATIM)
add_library(agentim_client SHARED IMPORTED GLOBAL)
set_target_properties(agentim_client PROPERTIES IMPORTED_LOCATION "${RUST_LIB}")
if (WIN32)
  set_target_properties(agentim_client PROPERTIES IMPORTED_IMPLIB "${RUST_LIB}.lib")
endif()
add_dependencies(agentim_client agentim_client_build)

add_library(agentim_dart SHARED "${HERE}/../../src/agentim_dart.c")
target_include_directories(agentim_dart PRIVATE "${AGENTIM_RUST}/native/include")
target_link_libraries(agentim_dart PRIVATE agentim_client)
if (ANDROID)
  target_link_options(agentim_dart PRIVATE "-Wl,-z,max-page-size=16384")
endif()
