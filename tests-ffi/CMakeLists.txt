cmake_minimum_required(VERSION 3.15.3)
project(yrs-ffi-tests)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_OSX_ARCHITECTURES "x86_64")

message(STATUS "current project dir: " ${PROJECT_SOURCE_DIR})
add_executable(yrs-ffi-tests main.cpp)

if (WIN32)
    target_compile_options(yrs-ffi-tests PRIVATE "$<$<CXX_COMPILER_ID:MSVC>:/utf-8>")
    add_custom_target(yrs-deps
            # DEBUG
            COMMAND ${CMAKE_COMMAND} -E copy "${PROJECT_SOURCE_DIR}/../target/debug/yrs.lib" "${PROJECT_SOURCE_DIR}/lib"
    )
else ()
    add_custom_target(yrs-deps
            # DEBUG
            COMMAND ${CMAKE_COMMAND} -E copy "${PROJECT_SOURCE_DIR}/../target/debug/libyrs.a" "${PROJECT_SOURCE_DIR}/lib"
    )
endif ()

include_directories(${PROJECT_SOURCE_DIR}/lib)
include_directories(${PROJECT_SOURCE_DIR}/include)

link_directories(${PROJECT_SOURCE_DIR}/lib)
add_dependencies(yrs-ffi-tests yrs-deps)
find_library(
        YRS_LIB
        NAMES yrs libyrs # what to look for
        HINTS "${PROJECT_SOURCE_DIR}/../target/debug/" # where to look
        NO_DEFAULT_PATH # do not search system default paths
)

link_libraries(ws2_32 wsock32 userenv)

if (WIN32)
    target_link_libraries(yrs-ffi-tests LINK_PUBLIC ${YRS_LIB} wsock32 ws2_32 userenv bcrypt)
else ()
    target_link_libraries(yrs-ffi-tests LINK_PUBLIC ${YRS_LIB} pthread dl)
endif ()
