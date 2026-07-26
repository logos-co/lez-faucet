cmake_minimum_required(VERSION 3.14)
project(LEZFaucetUIPlugin LANGUAGES CXX)

if(DEFINED ENV{LOGOS_MODULE_BUILDER_ROOT})
    include($ENV{LOGOS_MODULE_BUILDER_ROOT}/cmake/LogosModule.cmake)
else()
    message(FATAL_ERROR "LogosModule.cmake not found. Set LOGOS_MODULE_BUILDER_ROOT.")
endif()

logos_module(
    NAME lez_faucet_ui
    REP_FILE src/FaucetBackend.rep
    SOURCES
        src/FaucetBackend.h
        src/FaucetBackend.cpp
    INCLUDE_DIRS
        src
    FIND_PACKAGES
        Qt6Gui
    LINK_LIBRARIES
        Qt6::Gui
)
