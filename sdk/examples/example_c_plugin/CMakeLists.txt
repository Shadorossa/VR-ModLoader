cmake_minimum_required(VERSION 3.15)
project(example_c_plugin C)

# 64-bit Windows only (the game is a 64-bit process): configure with -A x64
add_library(example_c_plugin SHARED plugin.c)
set_target_properties(example_c_plugin PROPERTIES
    C_STANDARD 11
    PREFIX ""                               # example_c_plugin.dll, as mod.toml says
    RUNTIME_OUTPUT_DIRECTORY_RELEASE "${CMAKE_BINARY_DIR}/dist/example_c_plugin"
    RUNTIME_OUTPUT_DIRECTORY_DEBUG   "${CMAKE_BINARY_DIR}/dist/example_c_plugin")
target_include_directories(example_c_plugin PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/../..")   # sdk/evt_plugin.h
if(MSVC)
    target_compile_options(example_c_plugin PRIVATE /W4)
endif()

# mod.toml + config.toml next to the DLL, so <build>/dist/example_c_plugin is the installable mod folder
add_custom_command(TARGET example_c_plugin POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_directory "${CMAKE_CURRENT_SOURCE_DIR}/mod" "${CMAKE_BINARY_DIR}/dist/example_c_plugin")
