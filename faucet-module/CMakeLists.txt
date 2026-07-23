cmake_minimum_required(VERSION 3.20)
project(LezFaucetModule LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(DEFINED ENV{LOGOS_MODULE_BUILDER_ROOT})
    include($ENV{LOGOS_MODULE_BUILDER_ROOT}/cmake/LogosModule.cmake)
else()
    message(FATAL_ERROR "LogosModule.cmake not found. Set LOGOS_MODULE_BUILDER_ROOT.")
endif()

logos_module(
    NAME lez_faucet
    SOURCES
        src/lez_faucet_module.h
        src/lez_faucet_module.cpp
    EXTERNAL_LIBS
        lez_faucet_ffi
)
