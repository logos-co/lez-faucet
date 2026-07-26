cmake_minimum_required(VERSION 3.20)
project(LezFaucetModuleTests LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

enable_testing()
find_package(Threads REQUIRED)

add_executable(lez_faucet_module_tests
    ../src/lez_faucet_module.cpp
    mocks/mock_lez_faucet_ffi.cpp
    test_lez_faucet_module.cpp
)
target_include_directories(lez_faucet_module_tests PRIVATE
    ../src
    stubs
    mocks
)
# The static-surface and version-drift tests read the production sources, so
# they need their paths rather than a copy that could go stale.
target_compile_definitions(lez_faucet_module_tests PRIVATE
    MODULE_HEADER_PATH="${CMAKE_CURRENT_SOURCE_DIR}/../src/lez_faucet_module.h"
    MODULE_SOURCE_PATH="${CMAKE_CURRENT_SOURCE_DIR}/../src/lez_faucet_module.cpp"
    MODULE_METADATA_PATH="${CMAKE_CURRENT_SOURCE_DIR}/../metadata.json"
)
target_link_libraries(lez_faucet_module_tests PRIVATE Threads::Threads)
target_compile_options(lez_faucet_module_tests PRIVATE -Wall -Wextra -Wpedantic -Werror)

add_test(NAME lez_faucet_module_tests COMMAND lez_faucet_module_tests)
