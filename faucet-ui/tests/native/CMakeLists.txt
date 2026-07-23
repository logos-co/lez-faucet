cmake_minimum_required(VERSION 3.14)
project(FaucetBackendNativeTests LANGUAGES CXX)

include(CTest)

add_executable(
    faucet_backend_native_tests
    test_external_recipient_state.cpp
)
target_compile_features(faucet_backend_native_tests PRIVATE cxx_std_17)
target_compile_options(faucet_backend_native_tests PRIVATE -Wall -Wextra -Werror)
target_include_directories(
    faucet_backend_native_tests
    PRIVATE
        ../../src
)

add_test(NAME faucet_backend_native_tests COMMAND faucet_backend_native_tests)
