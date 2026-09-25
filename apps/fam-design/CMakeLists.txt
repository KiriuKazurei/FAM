# The Ant Design 5 design sample, on its own.
#
# Deliberately separate from `apps/fam-shell`: this target links Qt only - no `famcore`, no Rust
# crate - so styling the design can never change what the product's window paints. The shell keeps
# its own appearance until someone decides to adopt this, and that decision is a separate change.
cmake_minimum_required(VERSION 3.22)
project(fam_design LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_INCLUDE_CURRENT_DIR ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 REQUIRED COMPONENTS Core Widgets)

# The same design system the product shell compiles - one copy of the tokens, the style sheet and the
# row, so the reference and the product cannot disagree about what the design is.
add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/../fam-ui ${CMAKE_BINARY_DIR}/fam-ui)

add_executable(fam-design WIN32
    src/main.cpp
    src/preview.cpp
    src/previewmodel.cpp)
target_include_directories(fam-design PRIVATE src)
target_link_libraries(fam-design PRIVATE Qt6::Widgets fam-ui)
