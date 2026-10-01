# FileAutoManage（FAM）

#### Description
FAM is a general-purpose resource manager for browsing, locating and lightly editing large,
numerous files. Whole-volume name search is delegated to the **Everything SDK**; FAM itself keeps
only the lazy per-directory index that browsing needs. Images, thumbnails, RAW parsing and AI
inference are out of scope and handled by a separate project. File tags are persisted in SQLite.

#### Status
The Go / Java / Electron / C++ stacks were removed and the code rebuilt as a Rust workspace plus a
Qt front-end: `fam-core` (config, Windows path keys), `fam-svc-store` (SQLite tags and per-path
notes, schema v3 - a covering index on the tag name, library-wide merge and a prune command),
`fam-svc-enumerate` (one-level directory reads), `fam-svc-walk` (recursive enumeration, and the
enumeration search backend), `fam-svc-audit` (the audit trail: append-only JSONL, one file per UTC
day, an SHA-256 hash chain per session), `fam-svc-auth` (MCP credentials: three tiers as verb
whitelists, workspace-scoped, salted-digest storage), `fam-svc-mcp` (the agent-facing surface: a
line-delimited JSON-RPC 2.0 protocol, 15 tools, and the gate - tier, then scope, then exclusion,
then reparse), `fam-svc-fs` (move, copy and delete in exactly one place: `transfer(from, to,
Move|Copy)` and `remove_path(path)`, with the rules - never overwrite, a directory is never copied,
a non-empty directory is refused by name - so an agent and a person hit the same sentence), `fam-svc-index` (the lazy directory index, view windows, and the search
router with its scope rules), `fam-svc-everything` (the Everything client: runtime binding, state
probe, query compilation, paged results), `fam-sched` (events merged into batched row deltas),
`fam-ffi` (the ABI producing `famcore.dll`: rows as fixed-size buffers with windowed reads, control
payloads as JSON - v1's view and watch entry points were deleted in increment 9),
`fam-bench` (measurements and the floor gate), `apps/fam-cli`, `apps/fam-mcp` (the stdio server
process) and `apps/fam-shell` (Qt6 Widgets, the product face).
`apps/fam-ui` is the design system in exactly one place - Ant Design 5's tokens turned into a style
sheet, plus the one row this design paints by hand - and it links `Qt6::Widgets` only: no `famcore`,
no Rust crate. Both Qt faces compile that one copy, so a token changed for the product cannot stay
old in the reference. `apps/fam-design` is now the design baseline and its capture tool
(`--shot`, `--theme`, and the measured contrast table on stdout); product features do not grow there.
See `docs/notes/design-sample-wears-ant-design-5.md` for the colour decisions and
`docs/notes/shell-wears-the-ant-design-baseline.md` for the adoption.

The rebuild is under way: browsing and search both run on the lazy directory index with streamed,
virtualised results, and a name search is routed between Everything and enumeration by which of them
can answer the whole request - under one shared set of scope rules. The interface is now Miller
columns: one directory, one v2 token and one model per column, a per-column sort control, and a drive
sidebar; each row's shell icon is resolved from its extension, so no paint ever opens a path.
The split is a display choice behind a `Split` button on the toolbar: collapsed, this is one
browsing surface filling the window; expanded, it is two independent ones - each with its own column
chain, its own result list and its own search token, because two panes sharing a token would have one
pane's poll rewriting the other's rows. Dragging a row into another column really moves the file:
plain drag is a move, Ctrl is a copy, the destination is the folder that column shows, the name is
kept, and a name that already exists there is refused rather than replaced. The drag payload is a
private type (`application/x-fam-paths`), so dragging to or from Windows Explorer does not work -
deliberately, because a second route to the same files would bypass `fam_path_op`, bypass
"never overwrite", and leave nothing in the audit trail.
Tags are now visible and removable on the human side too: `Tags…` opens a dialog listing the tags on
that path, with add, remove (a two-step confirm) and per-file rename. It shows **one row per tag
name** with its sources spelled out (`contract (manual, auto)`) because that is what the wire does -
`fam_tag_remove` deletes by name across sources, and with an empty name list its SQL drops the
`tag IN (...)` clause and deletes every tag the file has. So "nothing selected" is refused at the
operation layer rather than sent.
Tags are painted on every row - a ladder that keeps the name its floor, the band folds into a `+N`
counter, and the data comes from a beat-driven per-column cache, so a paint still never queries
anything. **Search by tag is answered by the library**: `tag:name` in the search box (several mean
all of them; tags match exactly) is served by the tag store as a search backend, hydrating each hit
with size, time and kind so it paints as an ordinary row - a name index is never asked, a capability
bit stops it before `covers` does; the agent's `fs_search` and `fam search` route the same way, and
the Scope drops tag hits outside the roots. The **vocabulary strip** under the toolbar is the same
library's other gesture: one chip per tag with the file count, click to search, click again to go
back - the box is the one place the query lives, the chips write it and read their marks back from
it, and the strip hides itself while the library holds no tags. A chip **on a row** is clickable too.
**Renaming and merging are one sentence**: `fam tag merge <from> <into>` (the agent's `tags_merge`,
the 15th tool, and the strip's right-click menu run the same store operation) moves every row of a
name onto the target, collapsing the ones that land on an identity the target already holds - with
no target yet it is a rename, with one it is the merge that turns `pdf` and `PDF` back into one name.
`fam store prune` reclaims `files` rows whose path is gone (tags and notes go with them by cascade;
a dry run is the default).
**Notes reached the human side**: `Note…` opens a free-text dialog (not a tag, never searchable,
8 KiB of UTF-8; a blank save is refused - use Clear), the CLI's `fam note get|set|clear` answers with
the agent's sentences, and files carrying a note light a 6px dot at the row's right edge, fed by the
same kind of batch read the tag chips use.
**Transfers are jobs, not calls**: `fam_transfer_begin/poll/cancel` - the drop queues, the beat
reports bytes, Esc asks for a stop, and a large cross-volume copy no longer holds the GUI thread.
The copy itself runs in 1 MiB chunks, so the cancel's granularity is a chunk and "cancelled" means
the partial was swept. **A mistake has a step back**: Ctrl+Z pops the session's transfer stack - a
move is undone by moving back, a copy's duplicate is removed by the same delete primitive the
agent's `fs_delete` answers through. And while our own payload hovers over a column, that column's
border lights up in the brand blue - the destination is the column's folder, so the feedback goes to
the whole column, not to a line between two rows.
The shell renders through Fusion rather than the native Windows style, because wherever the style
sheet is silent the native style draws its own gradients and focus glows and disagrees with the token
set; `--theme light|dark|system` picks the scheme at start-up. File rows are painted by a delegate -
name in demi-bold, size in the tertiary text colour, kind as a Tag chip - and a folder no longer
prints `0 B`, because that zero means "nothing wrote a size here", not "this has no size".
Readability is measured, not copied: `Theme::contrastReport()` recomputes the WCAG ratios, and the
shell's `--self-check` log carries the table (18 gated pairs, the exempt ones still named).
Every request the core answers leaves one line in the audit trail, and the core refuses to start if
that trail cannot be written - while **reading it back is only possible through the CLI**
(`fam audit tail|verify|stats`): `famcore.dll` exports no read surface at all. The cost is measured,
not assumed: 41 µs per append in the debug build, 24 lines and 5.8 KB for a full browsing session.
Agent credentials live on the same side: **issuing, revoking and rotating are human-only**
(`fam mcp token add|list|revoke|rotate`), and `fam-svc-auth` is not a dependency of `fam-ffi` - so
there is no token, permission or audit entry point on the ABI surface. Not a runtime check:
that path does not link. The agent-facing surface (`apps/fam-mcp`, stdio) now exists, and it cannot
manage credentials either: the 15 tool names are a snapshot test, and any name containing `token`,
`grant`, `revoke`, `rotate`, `audit`, `config`, `secret`, `credential` or `mcp` turns it red.
The three tiers are verb whitelists rather than a comparable ladder, no
token reaches outside its own workspaces - an unresolved spelling (`.` or `..`) counts as outside, a
junction below a granted root does not count as inside, and the program's own trail and credential
file are unreachable through the tools. The secret is printed once at `add` and kept only as a
per-token salted digest - one authentication measured at 1.6 µs. Every tool call, refused ones
included, leaves a line in the trail; a request that reaches the gate measures 246 µs (the gate) and
750 µs (the whole call), because the price is in the metadata syscalls, not in the comparisons.
`docs/legacy-specs.md` records what the old system was; `docs/notes/` records what each increment
decided.

#### Build
The CLI and tests use the default toolchain:

```bash
cargo test --workspace
cargo build --release -p fam-cli
```

The agent-facing side is `apps/fam-mcp`, a stdio process that needs no Qt:

```bash
cargo build --release -p fam-mcp
fam mcp token add organizer --tier edit --workspace "D:/inbox"   # printed once
FAM_MCP_TOKEN=fam_xxxxxxxx_... \
  target/release/fam-mcp.exe --config <path/to/config.toml>
```

There is **no `--token` flag**: a token on the command line is visible to every other process on the
machine, while an environment variable is visible only to this one. Without a credential the process
refuses to start and says what is missing, rather than launching something a client cannot connect
to; that refusal and all logging go to stderr, and stdout carries protocol only.
`docs/notes/mcp-server-four-questions-before-the-disk.md` records the bounds of the tools, the
measured cost per request, and what was left undone (no HTTP transport, no ranged reads, no rate
limiting).

`famcore.dll` must be built with the GNU toolchain — the shell uses Qt's MinGW kit, which cannot
link MSVC objects — and Qt's bundled posix MinGW has to lead PATH, because the MinGW already on
this machine uses the MCF thread model and breaks GNU linking:

```bash
export PATH="/d/Applications/Qt/6.11/Tools/mingw1310_64/bin:$PATH"
cargo +stable-x86_64-pc-windows-gnu build -p fam-ffi --target x86_64-pc-windows-gnu
```

For UI screenshots use the native `windows` platform plugin: `QT_QPA_PLATFORM=offscreen` has no
font database, so every glyph including ASCII renders as a tofu box.

Running the shell **on this development machine** needs three runtimes on PATH at once - Qt's,
MinGW's, and the GNU target directory holding `famcore.dll`:

```bash
PATH="<Qt>/6.11.2/mingw_64/bin:<Qt>/Tools/mingw1310_64/bin:target/x86_64-pc-windows-gnu/debug" \
  apps/fam-shell/build/fam-shell.exe --watch <dir> --self-check <shot.png> --self-check-mutate
```

Handing it to somebody else is a different thing, and it goes through `cmake --install`: the
executable, the `famcore.dll` it is linked against, and whatever `windeployqt` decides that binary
needs from Qt (the platform plugin, the styles, the MinGW runtime) all land in one folder that does
not care about those three PATH entries.

```bash
cmake --install apps/fam-shell/build --prefix dist/fam-shell
cmake --install apps/fam-design/build --prefix dist/fam-design
```

What that buys is exactly what it has to be checked against - the folder starts with nothing but the
Windows system directories on PATH:

```cmd
set PATH=C:\Windows\system32;C:\Windows
dist\fam-shell\fam-shell.exe --config <config.toml> --watch <dir> --self-check shot.png
```

Both Qt faces share one rule (`apps/fam-ui/cmake/FamDeployQt.cmake`), because two copies of a
packaging rule drift and the drift shows up on someone else's machine rather than in this repo.
`fam-ui` itself is a static library and has no install rule - its objects are already inside the
executable - which is why the tree holds three CMake projects and still ships two programs. The two
console targets (`abi-smoke`, `model-smoke`) are deliberately **not** installed: they are gates that
read this source tree, not products. Sizes from a Debug build, recorded in the notes: 125 MB and 23
files for the shell, of which `famcore.dll` is 51.2 MB and `opengl32sw.dll` 19.7 MB - the last one is
the only cut known to be available and deliberately not taken, because the condition it protects
against cannot be produced on this machine.

`--self-check` is the headless gate: it renders the window, writes a screenshot and a `.log` beside
it, and exits. `--self-check-mutate` additionally **writes two files into the watched tree and
removes them again**: one runs the "new file -> reconciled re-read -> windowed diff" and
"deletion -> cut" channels, the other runs the three drag-and-drop cases (open the split, point the
two panes at two folders inside that tree, then drop with no modifier = move, with Ctrl = copy, and
back onto the folder already holding it = no call at all). The log says which path exists on disk
after each one, because whether the source is still there is the only observation that tells a move
apart from a copy. The same run also does a **tag round trip** (read back, add, read back, try to
remove nothing, remove) and grabs the dialog on its own as `<shot>.tags.png` - every assertion about
the wire came back green while the first picture of that dialog showed it had not picked up the theme.
Tag chips on rows (`<shot>.rowtags.png`), the vocabulary strip (`<shot>.filter.png` - a chip is
clicked until the search settles and clicked again to clear, then a merge through the strip's
signal), the note dialog (`<shot>.note.png`) and the drop hover mid-drag (`<shot>.drophover.png`)
each have their own frame, because a new surface wears the theme or the picture says so. The
strip step drives a chip **until the search settles with the tagged file in it and a second click
clears**, the row's own chip is clicked with a **real mouse press and release** at the point the
delegate's own hit test names, notes run none -> write -> read back -> blank refused -> cleared ->
"nothing to clear" told honestly, the note marker lights and goes out (`marker: shown=1 gone=1`),
the undo step sends two **real Ctrl+Z keys** (`copy-undone`/`move-undone`, asserted on the disk,
after the transfer steps wait for the queue to drain), and the drop hover is entered and left with
real drag events - all of these read 1 before the run passes. Finally the log carries the icon
extract/destroy counts and the IPC call counts - those numbers are what "no HICON leak" and "a poll
is cheap" mean in practice. **If the process is
killed mid-run those two files stay in the tree you pointed it at**; both are named
`fam-self-check-*.txt`.

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
- `docs/sqlDesign.md` — old-stack design intent (the PostgreSQL shape was never implemented); the
  banner at the top says so, and the live schema lives in `crates/fam-svc-store`
- `docs/代码辅助器.md` — still partly applicable
- `docs/history/` — obsolete designs kept for intent only, **do not implement from them**

Old-stack source is recoverable from the `archive/pre-rust-migration` tag; the pre-migration
workspace snapshot is `84342b5`, which is the line-number baseline for `docs/legacy-specs.md`.

#### Installation

Build the core and the shell (see **Build** above), then install into a self-contained folder:

```bash
cmake --install apps/fam-shell/build --prefix dist/fam-shell
cmake --install apps/fam-design/build --prefix dist/fam-design
```

The installed folder runs with nothing but the Windows system directories on PATH - that is the
whole point, and the acceptance: point the config at a tree, strip PATH, run the self-check gate,
and expect exit 0 with every assertion at 1.

#### Instructions

1.  Hand `fam-shell.exe` a configuration file (`--config <config.toml>`) naming the database, the
    search roots, the audit directory and the token file - a minimal example lives in the CLI tests;
    `--watch <dir>` opens a tree at start-up, `--theme light|dark|system` picks the scheme.
2.  Browse with the Miller columns; type terms in the box (or click a tag chip) and press Search;
    `tag:name` searches the library; select a row for `Tags…` and `Note…`.
3.  Keyboard: Delete removes (twice - the first press names what the second will remove), F2
    renames, double-click or Enter hands a file to the platform, Ctrl+Z undoes the last transfer,
    Esc cancels running transfers or a delete confirmation.
4.  Drag a row onto another column to move it, with Ctrl to copy; the destination is the folder
    that column shows.
5.  For agents: issue a credential with `fam mcp token add` and run `fam-mcp.exe` over stdio - see
    **Build** above.

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
