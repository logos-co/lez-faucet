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
target_link_libraries(lez_faucet_module_tests PRIVATE Threads::Threads)
target_compile_options(lez_faucet_module_tests PRIVATE -Wall -Wextra -Wpedantic -Werror)

add_test(NAME lez_faucet_module_tests COMMAND lez_faucet_module_tests)
