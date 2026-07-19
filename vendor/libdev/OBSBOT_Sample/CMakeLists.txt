cmake_minimum_required(VERSION 3.16)

project(OBSBOT_Sample)

# Prohibit in-source builds
message(STATUS "CMAKE_BINARY_DIR = ${CMAKE_BINARY_DIR}")
message(STATUS "CMAKE_SOURCE_DIR = ${CMAKE_SOURCE_DIR}")
if ("${CMAKE_BINARY_DIR}" STREQUAL "${CMAKE_SOURCE_DIR}")
    message(FATAL_ERROR "RM: You cannot build in a source directory (or any directory with CMakeLists.txt file). Please"
            "make a build subdirectory. Feel free to remove CMakeCache.txt and CMakeFiles.")
endif ()

set(CMAKE_CXX_STANDARD 11)
set(CXX_STANDARD_REQUIRED true)

set(OBSBOT_Target "OBSBOT_Sample")

add_executable(${OBSBOT_Target} main.cpp)

# set sdk path
message(STATUS "PROJECT_SOURCE_DIR = ${PROJECT_SOURCE_DIR}")
set(SDK_INCLUDE_PATH ${PROJECT_SOURCE_DIR}}/../include)
if (APPLE)
    if (CMAKE_OSX_ARCHITECTURES STREQUAL "arm64")
        set(SDK_LINK_PATH ${PROJECT_SOURCE_DIR}/../macos/arm64-release)
    else ()
        set(SDK_LINK_PATH ${PROJECT_SOURCE_DIR}/../macos/x86_64-release)
    endif ()
else ()
    if (CMAKE_BUILD_TYPE STREQUAL "Debug")
        set(SDK_LINK_PATH ${PROJECT_SOURCE_DIR}/../windows/win64-debug)
    else ()
        set(SDK_LINK_PATH ${PROJECT_SOURCE_DIR}/../windows/win64-release)
    endif ()
endif ()

target_sources(${OBSBOT_Target} PRIVATE main.cpp)

include_directories(${CMAKE_SOURCE_DIR}/../include)

if (APPLE)
    target_link_directories(${OBSBOT_Target} PRIVATE "${SDK_LINK_PATH}")
    target_link_libraries(${OBSBOT_Target} PRIVATE dev)
    set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -Wno-deprecated-declarations -Wno-switch")
    set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -Wall -g -stdlib=libc++")
else ()
    target_compile_definitions(${OBSBOT_Target} PRIVATE UNICODE _UNICODE _CRT_SECURE_NO_WARNINGS _CRT_NONSTDC_NO_WARNINGS)
    target_link_options(${OBSBOT_Target} PRIVATE "LINKER:/SAFESEH:NO")

    # 为 release 版本也生成 pdb 符号文件
    set(CMAKE_C_FLAGS_RELEASE "${CMAKE_C_FLAGS_RELEASE} /Zi")
    set(CMAKE_CXX_FLAGS_RELEASE "${CMAKE_CXX_FLAGS_RELEASE} /Zi")
    set(CMAKE_SHARED_LINKER_FLAGS_RELEASE "${CMAKE_SHARED_LINKER_FLAGS_RELEASE} /DEBUG /OPT:REF /OPT:ICF")

    target_compile_options(${OBSBOT_Target} PRIVATE /wd4091 /wd4200 /wd4267 /wd4819 /wd4834 /wd4996)
    set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} /utf-8")

    target_link_directories(${OBSBOT_Target} PRIVATE "${SDK_LINK_PATH}")

    target_link_libraries(${OBSBOT_Target} PRIVATE libdev)
endif ()

# install
set(PROJECT_RUN_DIR ${CMAKE_BINARY_DIR}/rundir)

# target
install(TARGETS ${OBSBOT_Target} DESTINATION ${PROJECT_RUN_DIR})
# depend lib
if (APPLE)
    install(DIRECTORY ${SDK_LINK_PATH}/ DESTINATION ${PROJECT_RUN_DIR})
else ()
    install(DIRECTORY ${SDK_LINK_PATH}/ DESTINATION ${PROJECT_RUN_DIR})
endif ()
