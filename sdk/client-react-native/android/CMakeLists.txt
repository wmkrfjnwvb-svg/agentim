# The C++ TurboModule for Android (docs/12 §2.12), built into the app by
# React Native's autolinking (react-native.config.cjs): the module and the
# JSI binding, linked to the native core shipped in jniLibs/<abi> (16 KB
# pages; packaged into the app as an imported library).
cmake_minimum_required(VERSION 3.13)
project(agentim_rn)

set(ROOT ${CMAKE_CURRENT_LIST_DIR}/..)

add_library(agentim_core SHARED IMPORTED)
set_target_properties(agentim_core PROPERTIES
  IMPORTED_LOCATION ${ROOT}/android/jniLibs/${ANDROID_ABI}/libagentim_client.so)

add_library(agentim_rn STATIC
  ${ROOT}/cpp/NativeAgentIM.cpp
  ${ROOT}/cpp/AgentIMJsi.cpp)
target_include_directories(agentim_rn PUBLIC ${ROOT}/cpp ${ROOT}/include)
# React Native's headers need C++20.
target_compile_features(agentim_rn PUBLIC cxx_std_20)
target_compile_options(agentim_rn PRIVATE -Wall -Wextra -Werror)
target_link_libraries(agentim_rn
  react_codegen_AgentIMSpec
  ReactAndroid::jsi
  ReactAndroid::reactnative
  agentim_core)
