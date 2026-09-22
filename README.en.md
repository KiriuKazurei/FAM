# FileAutoManage（FAM）

#### Description
FAM is a general-purpose resource manager for browsing, locating and lightly editing large,
numerous files. Whole-volume name search is delegated to the **Everything SDK**; FAM itself keeps
only the lazy per-directory index that browsing needs. Images, thumbnails, RAW parsing and AI
inference are out of scope and handled by a separate project. File tags are persisted in SQLite.

#### Status
The Go / Java / Electron / C++ stacks were removed and the code rebuilt as a Rust workspace plus a
Qt front-end: `fam-core` (config, Windows path keys), `fam-svc-store` (SQLite tags),
`fam-svc-enumerate` (one-level directory reads), `fam-svc-walk` (recursive enumeration, and the
enumeration search backend), `fam-svc-index` (the lazy directory index, view windows, and the search
router with its scope rules), `fam-svc-everything` (the Everything client: runtime binding, state
probe, query compilation, paged results), `fam-sched` (events merged into batched row deltas),
`fam-view` (positioned row model - v1, awaiting removal), `fam-ffi` (the ABI producing
`famcore.dll`: v1 JSON, v2 fixed-size row buffers with windowed reads), `fam-bench` (measurements
and the floor gate), `apps/fam-cli` and `apps/fam-shell` (Qt6 Widgets).

The rebuild is under way: browsing and search both run on the lazy directory index with streamed,
virtualised results, and a name search is routed between Everything and enumeration by which of them
can answer the whole request - under one shared set of scope rules. `docs/legacy-specs.md` records
what the old system was; `docs/notes/` records what each increment decided.

#### Build
The CLI and tests use the default toolchain:

```bash
cargo test --workspace
cargo build --release -p fam-cli
```

`famcore.dll` must be built with the GNU toolchain — the shell uses Qt's MinGW kit, which cannot
link MSVC objects — and Qt's bundled posix MinGW has to lead PATH, because the MinGW already on
this machine uses the MCF thread model and breaks GNU linking:

```bash
export PATH="/d/Applications/Qt/6.11/Tools/mingw1310_64/bin:$PATH"
cargo +stable-x86_64-pc-windows-gnu build -p fam-ffi --target x86_64-pc-windows-gnu
```

For UI screenshots use the native `windows` platform plugin: `QT_QPA_PLATFORM=offscreen` has no
font database, so every glyph including ASCII renders as a tofu box.

#### Software Architecture (the legacy system, as it was before the rebuild)

This section describes what was **replaced**. It is kept because it explains what the rebuild is
avoiding; one point is now the opposite of the code: the index *is* self-built (`fam-svc-index`),
and Everything is one optional backend behind it.

Single process, no gateway layer and no multi-language tiering:

- **Core** — Rust monolith; file watching uses a bounded queue that drops when full, consumed
  serially, pushing changes to the UI
- **UI** — native Qt, list backed by `QAbstractListModel`, rows inserted in batches
- **Search** — Everything SDK by file name, scoped by directory and suffix; no self-built index
- **Persistence** — SQLite for tags and file paths, see `docs/sqlDesign.md`

#### Documentation
- `docs/legacy-specs.md` — required reading for the migration; specs that existed only in code
- `docs/sqlDesign.md`, `docs/代码辅助器.md` — still partly applicable
- `docs/history/` — obsolete designs kept for intent only, **do not implement from them**

Old-stack source is recoverable from the `archive/pre-rust-migration` tag; the pre-migration
workspace snapshot is `84342b5`, which is the line-number baseline for `docs/legacy-specs.md`.

#### Installation

1.  Not available yet

#### Instructions

1.  Not available yet

#### Contribution

1.  Fork the repository
2.  Create Feat_xxx branch
3.  Commit your code
4.  Create Pull Request

#### Gitee Feature

1.  You can use Readme\_XXX.md to support different languages, such as Readme\_en.md, Readme\_zh.md
2.  Gitee blog [blog.gitee.com](https://blog.gitee.com)
3.  Explore open source project [https://gitee.com/explore](https://gitee.com/explore)
4.  The most valuable open source project [GVP](https://gitee.com/gvp)
5.  The manual of Gitee [https://gitee.com/help](https://gitee.com/help)
6.  The most popular members  [https://gitee.com/gitee-stars/](https://gitee.com/gitee-stars/)
