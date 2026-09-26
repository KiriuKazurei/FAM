# Turning a built Qt executable into a folder someone else can double-click.
#
# This lives in `apps/fam-ui/cmake` rather than being written twice, once per Qt face, for the same
# reason the design system itself lives there: two copies of a packaging rule drift, and the drift
# shows up on the machine you handed the folder to rather than on the one that built it.
# `fam-ui` is a static library, so it needs no install rule of its own - its objects are already
# inside whichever executable linked it. That is why the tree holds three CMake projects and still
# ships two programs.
#
# Two things have to be true for the output to be worth calling a package, and neither is checked by
# the build succeeding:
#
#   1. The core's DLL has to come along. `fam-shell.exe` is linked against `famcore.dll` by import
#      library, so a missing DLL is a process that never reaches `main` - and on Windows that failure
#      is a dialog box, not a message you can read in a log.
#   2. Qt's own runtime has to come along: the platform plugin (`plugins/platforms/qwindows.dll`)
#      above all, because Qt refuses to start a GUI without it and says so on a console this app has
#      no console to say it on. `windeployqt` is the only tool that knows which plugins and QPA
#      libraries a given Qt build actually needs, so this calls it instead of guessing a file list.
#
# The proof that both are real is in the note that came with this: the installed executable is run
# with `PATH` reduced to the system directories, which is the one condition a developer's shell never
# satisfies by accident.

# `windeployqt` beside the Qt that `find_package(Qt6)` resolved to. `HINTS` is the part doing the
# work: `find_program` searches it before the `PATH` environment variable, so a second Qt on `PATH`
# cannot quietly supply a `windeployqt` that deploys a different Qt than the one this executable was
# linked against. `REQUIRED` is deliberate too - a packaging step that quietly skips the deployment
# produces a folder that looks complete and does not run.
#
# Pinning `qtpaths` alongside it was tried and dropped: windeployqt resolves its own Qt installation
# from where that executable sits, so with `PATH` reduced to the system directories it deployed
# correctly without any hint (measured - see the note). One fewer line claiming to be load-bearing.
function(fam_find_windeployqt out_var)
    find_program(_fam_windeployqt NAMES windeployqt windeployqt6
        HINTS "${Qt6_DIR}/../../../bin"
        REQUIRED)
    set(${out_var} "${_fam_windeployqt}" PARENT_SCOPE)
endfunction()

# Installs `<target>` (and any extra runtime DLLs named by `DLL`) into the prefix root, then deploys
# Qt into the same folder.
#
# Everything lands flat next to the executable rather than in `bin/` + `lib/`: this is a Windows
# program directory a person copies somewhere, and a subfolder the loader does not search would just
# move the failure later.
function(fam_deploy_qt)
    cmake_parse_arguments(PARSE_ARGV 0 FAMD "" "TARGET" "DLL")
    if(NOT FAMD_TARGET)
        message(FATAL_ERROR "fam_deploy_qt needs TARGET <executable>")
    endif()

    # Without this, a bare `cmake --install <build>` aims at `C:\Program Files\...` because that is
    # CMake's own default prefix. The build tree is the only place this project's output belongs.
    if(CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT)
        set(CMAKE_INSTALL_PREFIX "${CMAKE_BINARY_DIR}/install"
            CACHE PATH "Where `cmake --install` puts a runnable folder" FORCE)
    endif()

    install(TARGETS ${FAMD_TARGET} RUNTIME DESTINATION .)
    foreach(dependency IN LISTS FAMD_DLL)
        if(TARGET ${dependency})
            # The imported target's own runtime artifact - the same path the executable was linked
            # against, so a stale `-DFAMCORE_DLL=` cannot ship something other than what was tested.
            install(IMPORTED_RUNTIME_ARTIFACTS ${dependency} DESTINATION .)
        else()
            install(FILES ${dependency} DESTINATION .)
        endif()
    endforeach()

    fam_find_windeployqt(FAM_WINDEPLOYQT)
    # `install(CODE)` runs after the rules above in the same install pass, which is what lets
    # windeployqt inspect a linked binary that actually exists at that path. Every variable below is
    # escaped so it is the *generated install script* that expands it, not this configure run.
    #
    # The two flags turned off each have a reason in this codebase rather than in a recipe:
    #   --no-translations  neither face calls `installTranslator` and every string in them is an
    #                      English literal, so the `.qm` files would be dead weight in a folder
    #                      someone has to copy around.
    #   --no-quick-import  there is no QML anywhere in the tree - this is a Widgets-only build - and
    #                      without the flag windeployqt walks the Qt Quick plugin set.
    # Everything else (the platform plugin, the styles, the software rasteriser, the D3D compilers) is
    # left to windeployqt's own judgement about what the binary links. Trimming further is a decision
    # that could only be checked on a machine without a GPU driver, and this repo has no such machine
    # to check it on.
    install(CODE "
        set(_famd_exe \"\${CMAKE_INSTALL_PREFIX}/$<TARGET_FILE_NAME:${FAMD_TARGET}>\")
        if(NOT EXISTS \"\${_famd_exe}\")
            message(FATAL_ERROR \"installed executable not found: \${_famd_exe}\")
        endif()
        execute_process(
            COMMAND \"${FAM_WINDEPLOYQT}\"
                    --compiler-runtime
                    --no-translations
                    --no-quick-import
                    \"\${_famd_exe}\"
            RESULT_VARIABLE _famd_rc
            OUTPUT_VARIABLE _famd_out
            ERROR_VARIABLE _famd_err)
        if(NOT _famd_rc EQUAL 0)
            message(FATAL_ERROR \"windeployqt failed (exit \${_famd_rc}):\n\${_famd_out}\${_famd_err}\")
        endif()
        message(STATUS \"deployed Qt runtime for $<TARGET_FILE_NAME:${FAMD_TARGET}>\")
    ")
endfunction()
