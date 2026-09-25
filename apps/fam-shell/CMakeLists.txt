cmake_minimum_required(VERSION 3.22)
project(fam_shell LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_INCLUDE_CURRENT_DIR ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 REQUIRED COMPONENTS Core Widgets)

# The core is built by cargo:
#   cargo +stable-x86_64-pc-windows-gnu build -p fam-ffi --target x86_64-pc-windows-gnu
set(FAMCORE_IMPLIB "" CACHE FILEPATH "cargo import library, e.g. libfamcore.dll.a")
set(FAMCORE_DLL "" CACHE FILEPATH "cargo famcore.dll, used at run time")

if(NOT FAMCORE_IMPLIB)
    message(FATAL_ERROR "pass -DFAMCORE_IMPLIB=<path to libfamcore.dll.a>")
endif()
if(NOT EXISTS "${FAMCORE_IMPLIB}")
    message(FATAL_ERROR "import library not found: ${FAMCORE_IMPLIB}")
endif()

add_library(famcore SHARED IMPORTED)
set_target_properties(famcore PROPERTIES
    IMPORTED_IMPLIB "${FAMCORE_IMPLIB}"
    IMPORTED_LOCATION "${FAMCORE_DLL}")

# The design system, compiled once and shared with the design sample. The binary dir is given
# explicitly because this project is configured on its own (`cmake -S apps/fam-shell`), so the
# subdirectory's outputs have no default home inside this build tree.
add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/../fam-ui ${CMAKE_BINARY_DIR}/fam-ui)

add_executable(fam-shell WIN32
    src/main.cpp
    src/mainwindow.cpp
    src/bridge.cpp
    src/famlistmodel.cpp
    src/columnchain.cpp
    src/drivelist.cpp
    src/iconcache.cpp)
target_include_directories(fam-shell PRIVATE include src)
# The icon layer talks to the shell's image lists directly: shell32 for SHGetFileInfoW and
# SHGetImageList, comctl32 for the IImageList vtable, ole32 for the STA the worker lives in.
target_link_libraries(fam-shell PRIVATE Qt6::Widgets famcore fam-ui shell32 comctl32 ole32)

# Console target so the ABI can be verified without a display server.
add_executable(abi-smoke
    tools/abi_smoke.cpp
    src/bridge.cpp)
target_include_directories(abi-smoke PRIVATE include src)
target_link_libraries(abi-smoke PRIVATE Qt6::Core famcore)
