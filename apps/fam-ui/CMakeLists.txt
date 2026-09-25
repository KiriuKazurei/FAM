# The design system both Qt faces compile from: Ant Design 5's tokens, the style sheet built out of
# them, and the one row this design paints by hand.
#
# It exists so there is exactly one copy of those files. The product shell and the design sample used
# to carry their own, which is the drift the "the sample is the baseline now" decision is meant to
# end - a token changed in one place and not the other produces two windows that disagree about what
# the design is, and only one of them is on screen at a time.
#
# Deliberate limits on this target:
#   * It links Qt only. No `famcore`, no Rust crate - the same boundary the sample had, so the
#     reference cannot acquire a dependency on the thing it references.
#   * It knows no model. `RowDelegate` is handed the data-role numbers and the size formatter it
#     should read (see `RowRoles`), because the shell's model and the sample's are different types
#     that happen to agree on the role names.
cmake_minimum_required(VERSION 3.22)
project(fam_ui LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_INCLUDE_CURRENT_DIR ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 REQUIRED COMPONENTS Widgets)

add_library(fam-ui STATIC
    src/theme.cpp
    src/rowdelegate.cpp)
target_include_directories(fam-ui PUBLIC src)
target_link_libraries(fam-ui PUBLIC Qt6::Widgets)
