# Everything SDK (vendored)

- Source: `https://www.voidtools.com/Everything-SDK.zip`
- Retrieved: 2026-09-20
- Archive SHA-256: `f5716d9513cce6b462b5170a0a2e7e081e191d9bcac6774f5decc061497df443` (238,231 bytes)
- `EVERYTHING_SDK_VERSION` in the header: 2; `dll/Everything64.dll` stamped 2026-08-10
- Licence: MIT, full text at the top of `include/Everything.h` — keep that notice in place

## What is kept

| Path | Why |
|---|---|
| `include/Everything.h` | The C API we bind against: 88 declared functions plus 30 `A`/`W` alias macros; `dll/Everything64.dll` exports 89 of those names |
| `ipc/everything_ipc.h` | Raw request/reply protocol, for reference and for a future no-DLL client |
| `dll/Everything64.dll` | Client library shipped next to the app; it talks to the running Everything service |
| `lib/Everything64.lib` | Kept for MSVC builds only |
| `example/dll.test.c` | Minimal upstream usage example, useful when checking binding semantics |

The archive's C#/VB/Java/etc. sample trees were dropped: they are not part of our build and
carry their own stale build outputs.

## How FAM uses it

`lib/Everything64.lib` is an **MSVC** import library, and the shell builds with Qt's MinGW kit, so
we do not link it. The core loads `Everything64.dll` at run time (`LoadLibraryExW` with
`LOAD_LIBRARY_SEARCH_APPLICATION_DIR` plus an explicit path) and resolves the handful of
functions it needs. That keeps the dependency optional at run time: with Everything not installed
or not running, name search falls back to the lazy directory index instead of failing to start.

Everything itself is **not** vendored here and is not installed on the development machine, so the
SDK path can be verified for loading and graceful degradation, but real result data requires
Everything to be present and indexing.
