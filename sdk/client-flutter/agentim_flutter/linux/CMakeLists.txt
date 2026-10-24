cmake_minimum_required(VERSION 3.10)
set(PROJECT_NAME "agentim_flutter")
project(${PROJECT_NAME} LANGUAGES C CXX)
add_subdirectory("${CMAKE_CURRENT_SOURCE_DIR}/../src" "${CMAKE_CURRENT_BINARY_DIR}/shared")
# Both go into the app bundle's lib folder, where Dart opens them by name.
set(agentim_flutter_bundled_libraries
  $<TARGET_FILE:agentim_dart>
  $<TARGET_FILE:agentim_client>
  PARENT_SCOPE
)
