/* C ABI exported by the fam-ffi crate (famcore.dll).
 *
 * v1 sends JSON everywhere; v2 sends rows as fixed-length binary into a buffer the caller owns.
 * Both are exported by the same library, and the version below is what this header was written
 * against - a shell built for one ABI must refuse to run against the other rather than misread the
 * bytes (see fam_abi_version).
 *
 * Strings in are UTF-8 C strings; strings out are heap allocations the caller must release with
 * fam_free_string. Control payloads (stats, tags, errors, config) stay JSON in both ABIs.
 */
#ifndef FAMCORE_H
#define FAMCORE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FamCore FamCore;

/* Returns 0 on success and -1 with *err set; 1 means the shell must refuse to run. */
#define FAM_OK 0
#define FAM_ERR (-1)

/* What fam_abi_version() must report. A mismatch is fatal, not a warning. */
#define FAM_ABI_VERSION 2u

/* --- v2 row buffers -----------------------------------------------------------------------
 *
 * A v2 call that carries data fills a caller-owned buffer and returns the number of bytes written,
 * or -1 with *err set. The buffer always begins with the header below; what follows depends on the
 * call, and nothing is ever written to a buffer whose caller is not ready to read.
 *
 *   fam_dir_state  header, alone
 *   fam_dir_fetch  header, `row_count` records of `record_size` bytes (a FamRecord), then the
 *                  UTF-16 name blob: a name starts at (40 + row_count * 32 + name_off * 2)
 *   fam_dir_poll   header, then `row_count` FamCut records, no blob
 *   fam_search_page  fam_dir_fetch's shape exactly, except that a record's name is the entry's whole
 *                  path - a result set spans directories, so the name has to say where as well as
 *                  what
 *
 * `record_size` is the version mechanism for the records: fields are appended and never reordered,
 * so a shell that knows only the first N bytes of a FamRecord steps `record_size` per row and reads
 * what it knows. A buffer too small for the whole window is not an error: the header comes back with
 * row_count 0 and FAM_FLAG_TRUNCATED, because half a record at the far end would be a corrupt row.
 */
#define FAM_FLAG_TRUNCATED 0x01u /* the window had more rows than this buffer got */
#define FAM_FLAG_LOSS 0x02u      /* events were discarded; what is on screen may be missing rows */
#define FAM_FLAG_RESYNC 0x04u    /* rows changed in a way positions cannot express: rebuild */
#define FAM_FLAG_INCOMPLETE 0x08u /* these rows are a prefix: the scope was not fully covered */

enum FamDirState {
    FAM_READY = 0,   /* rows cached and current */
    FAM_LOADING = 1, /* a read is running and there are no previous rows to show */
    FAM_STALE = 2,   /* rows cached but no longer guaranteed to match the filesystem */
    FAM_FAILED = 3,  /* the last read failed and the backoff has not elapsed */
    FAM_EMPTY = 4    /* nothing cached and nothing in flight */
};

/* 40 bytes, exactly as the Rust side lays it out. */
typedef struct FamHead {
    uint32_t magic;
    uint16_t abi;
    uint16_t record_size;
    uint32_t row_count;
    uint8_t state;
    uint8_t flags;
    uint16_t pad;
    uint32_t total_rows;
    uint32_t reserved;
    uint64_t generation;
    uint64_t seq;
} FamHead;

/* 32 bytes: one directory row, and the same bytes the cache stores. `name_off`/`name_units` address
 * the name blob of the buffer this record arrived in. mtime_ms is i64_min when unknown. */
typedef struct FamRecord {
    uint32_t name_off;
    uint16_t name_units;
    uint8_t kind;   /* index into the core's display buckets */
    uint8_t flags;  /* bit 0 directory, bit 1 reparse point */
    uint64_t size;
    int64_t modified_ms;
    uint32_t attrs;
    uint32_t dir;
} FamRecord;

/* One row position for the model to cut. Positions must be applied in the order returned, each
 * against the rows the earlier cuts left behind - which is what lets a run coalesce into one
 * beginRemoveRows. */
typedef struct FamCut {
    uint32_t position;
    uint32_t reserved;
} FamCut;

#ifdef __cplusplus
#define FAM_STATIC_ASSERT(name, condition) static_assert(condition, #name)
/* The layout is the contract; if these fire, the Rust and C++ definitions have drifted. */
FAM_STATIC_ASSERT(fam_head_is_forty_bytes, sizeof(FamHead) == 40);
FAM_STATIC_ASSERT(fam_record_is_thirty_two_bytes, sizeof(FamRecord) == 32);
FAM_STATIC_ASSERT(fam_cut_is_eight_bytes, sizeof(FamCut) == 8);
#endif

uint32_t fam_abi_version(void);

/* --- v2: directory views ------------------------------------------------------------------
 *
 * fam_open_dir never waits for the filesystem: it returns a token and the rows arrive when the
 * core's loader thread gets there. Nothing in this group touches the filesystem on the calling
 * thread, so every one of them is safe to call from a paint or a scroll.
 */
uint64_t fam_open_dir(FamCore *core, const char *path, char **err);
int fam_dir_state(FamCore *core, uint64_t token, unsigned char *buffer, uint32_t buffer_len,
                  char **err);
int fam_dir_fetch(FamCore *core, uint64_t token, uint32_t first, uint32_t count,
                  unsigned char *buffer, uint32_t buffer_len, char **err);
int fam_dir_poll(FamCore *core, uint64_t token, unsigned char *buffer, uint32_t buffer_len,
                 char **err);
int fam_dir_set_sort(FamCore *core, uint64_t token, const char *sort, const char *order,
                     char **err);
/* terms_json is a JSON array of lower-cased substrings, or null/empty for no filter. */
int fam_dir_set_filter(FamCore *core, uint64_t token, const char *terms_json, const char *kind,
                       char **err);
void fam_dir_close(FamCore *core, uint64_t token);
/* Which search backends exist and which are usable now, as JSON. */
int fam_backends_json(FamCore *core, char **out, char **err);

/* --- v2: paged name search ------------------------------------------------------------------
 *
 * fam_search_begin queues a search and returns its token without waiting for a single row. The rows
 * are then read the same way a directory's are: fam_search_page hands out a window with a header that
 * says whether the set is still growing (FAM_FLAG_INCOMPLETE, state FAM_LOADING) or final.
 * fam_search_cancel asks the core's search thread to stop - cooperatively, between directories and
 * between entries, so the walk really ends instead of the caller merely giving up on it.
 * `limit` 0 asks for the core's default (500 hits); larger values are clamped to 20000, because a
 * result set is held in the core until fam_search_close.
 */
uint64_t fam_search_begin(FamCore *core, const char *query, const char *within, const char *kind,
                          uint32_t limit, char **err);
int fam_search_page(FamCore *core, uint64_t token, uint32_t first, uint32_t count,
                    unsigned char *buffer, uint32_t buffer_len, char **err);
int fam_search_cancel(FamCore *core, uint64_t token, char **err);
void fam_search_close(FamCore *core, uint64_t token);

/* --- the half of the ABI that stayed JSON --------------------------------------------------------
 *
 * Lifecycle, stats, tags and `find`. These were JSON in v1 and stayed JSON in v2, because they are
 * control payloads rather than rows: one call, one small answer, no window to page through.
 *
 * The v1 view trio (`fam_view_new` / `fam_view_search` / `fam_view_drain`) and `fam_watch_start`
 * used to sit here. Increment 9 deleted them: they were a second implementation of "a listing that
 * follows a directory", and the one in v2 above is the only thing that can keep a view honest about
 * what it does not know. */
FamCore *fam_core_new(const char *config_path, char **err);
void fam_core_free(FamCore *core);
void fam_free_string(char *value);

int fam_stats(FamCore *core, char **out, char **err);

int fam_tag_add(FamCore *core, const char *path, const char *tags_json, const char *source,
                int32_t priority, char **out, char **err);
int fam_tag_list(FamCore *core, const char *path, char **out, char **err);
int fam_tag_remove(FamCore *core, const char *path, const char *tags_json, const char *source,
                   char **out, char **err);

/* The asynchronous transfer: begin answers `{"job":N}` before a byte has moved (flags are
 * `fam_path_op`'s - `FAM_PATH_COPY`, no other bit); poll answers
 * `{"state":"running"|"done"|"cancelled"|"failed","bytes":N,"total":N,...}` and the terminal state
 * exactly once, retiring the job; cancel asks the job to stop at its next chunk. Two audit lines
 * per transfer - begin at this call, end at the poll that sees it finish. The same never-overwrite
 * and parent-must-exist rules run inside the job; a refusal arrives at poll time as a `failed`
 * state with the sentence it always had. */
int fam_transfer_begin(FamCore *core, const char *from, const char *to, uint32_t flags,
                       char **out, char **err);
int fam_transfer_poll(FamCore *core, uint64_t job, char **out, char **err);
int fam_transfer_cancel(FamCore *core, uint64_t job, char **out, char **err);

/* Removes one path - a file, an empty directory, or a symlink as the link itself. A directory that
 * still has entries is refused by name; the recursive delete is the one irreversible call there is,
 * and this surface does not make it quietly. This is the inverse half of a copied transfer: the
 * shell's undo runs this entry, audited as `path.delete` like any other act. The answer is
 * `{"removed":"...","kind":"file"|"dir"}`. */
int fam_path_delete(FamCore *core, const char *path, char **out, char **err);

/* The free-text note beside one path - `{"body":"...","updated_at":"..."}`, or `{"body":null}`
 * when there is none. A note is deliberately not a tag and never enters the directory index, so
 * this call on the exact path is the only way to read one back. `fam_note_set` replaces whatever
 * was there and refuses a blank body (the store will not read "empty" as "delete it" -
 * `fam_note_clear` exists); the limit is 8 KiB of UTF-8, and an over-long body is refused whole. */
int fam_note_get(FamCore *core, const char *path, char **out, char **err);
int fam_note_set(FamCore *core, const char *path, const char *body, char **out, char **err);
int fam_note_clear(FamCore *core, const char *path, char **out, char **err);

/* Rename a label across the whole library: every row named `from` takes the name `into`, and where
 * a file already carries `into` from the same source the two rows collapse into the one that was
 * already there. With nothing yet named `into` this is simply a rename; with rows already there it
 * is the merge that turns `pdf` and `PDF` back into one name. The answer reports what happened as
 * counts - `{"moved":N,"collapsed":N,"files":N}` - and an unknown `from` is an error, so a typo
 * merging onto a real name cannot read as success. */
int fam_tag_merge(FamCore *core, const char *from, const char *into, char **out, char **err);

/* Rename one label on one file; `to` and `source` are each optional and neither may be absent. */
int fam_tag_rename(FamCore *core, const char *path, const char *from, const char *to,
                   const char *source, char **out, char **err);

/* The tags of many files at once: `paths_json` is a JSON array of paths, the answer a JSON object
 * keyed by those same strings, holding only the files that carry a tag - an absent key means "no
 * tags", so a caller never has to tell "untagged" apart from "not asked about".
 *
 * This is here instead of a `fam_tag_list` per row because a screen is a hundred and twenty-eight
 * rows, and that was measured: 7.5ms of store time per window against 1.2ms for this one call. The
 * keys come back as the caller spelled them, deliberately - `path_norm` is this library's folding of
 * a path into an identity, and a caller that had to reproduce it to join the answer would be doing
 * identity work on the wrong side of the wire.
 *
 * It is a heartbeat, not an act: `tag.fetch` never reaches the audit trail, because it runs on the
 * shell's poll beat for as long as a folder is on screen. */
int fam_tags_for_paths(FamCore *core, const char *paths_json, char **out, char **err);

/* Which paths of a window carry a note: `{"<path>":true}` for the ones that do, keys spelled as the
 * caller spelled them, absent keys meaning "no note" - the twin of `fam_tags_for_paths`, and a
 * heartbeat like it (`note.fetch` never reaches the audit trail). An empty window is an empty
 * answer rather than an error. */
int fam_notes_for_paths(FamCore *core, const char *paths_json, char **out, char **err);

/* The notes of many files at once: `paths_json` is a JSON array of paths, the answer a JSON object
 * keyed by those same strings, holding `true` for each path that carries a note. An absent key means
 * "no note", so "has note" and "not asked about" stay the same answer here as they are on the wire.
 *
 * It is a heartbeat, not an act: `note.fetch` never reaches the audit trail, because it runs on the
 * shell's poll beat for as long as a folder is on screen. */
int fam_notes_for_paths(FamCore *core, const char *paths_json, char **out, char **err);

/* Every tag name in the store with how many files carry it, most used first:
 * `[{"tag":"contract","files":2}, ...]`. A name on one file under both sources counts as one file,
 * because the number is a promise about what selecting that tag will show.
 *
 * Not scoped to a directory, which makes it the largest read on this surface - so ask it once and
 * again when something you did changed the answer, rather than on every beat. */
int fam_tag_vocabulary(FamCore *core, char **out, char **err);

/* --- moving a file -------------------------------------------------------------------------------
 *
 * The first entry point on this surface that changes what is on disk. Everything above it reads.
 *
 * Both arguments are full paths: `to` is the destination *name*, not the folder being dropped into,
 * because only the caller knows whether the person meant "into here" or "rename it to this".
 * The answer is JSON naming what happened and how: `"by"` is `renamed` for a same-volume move,
 * `copied-then-removed` for a file that had to cross a volume, `copied` when `FAM_PATH_COPY` was
 * set or when the source could not be deleted after a successful copy.
 *
 * The rules are the same ones the agent's `fs_move` tool is held to, because both call one
 * implementation: a target that exists is refused rather than overwritten, a missing source is
 * refused, a directory is never copied, and a directory that would have to cross a volume is refused
 * by name. `err` carries the sentence that says which rule refused it. */
#define FAM_PATH_COPY 1u

int fam_path_op(FamCore *core, const char *from, const char *to, uint32_t flags,
                char **out, char **err);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* FAMCORE_H */
