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
 *
 * `record_size` is the version mechanism for the records: fields are appended and never reordered,
 * so a shell that knows only the first N bytes of a FamRecord steps `record_size` per row and reads
 * what it knows. A buffer too small for the whole window is not an error: the header comes back with
 * row_count 0 and FAM_FLAG_TRUNCATED, because half a record at the far end would be a corrupt row.
 */
#define FAM_FLAG_TRUNCATED 0x01u /* the window had more rows than this buffer got */
#define FAM_FLAG_LOSS 0x02u      /* events were discarded; what is on screen may be missing rows */
#define FAM_FLAG_RESYNC 0x04u    /* rows changed in a way positions cannot express: rebuild */

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

/* --- v1: JSON views, kept so an older shell still runs against this library ---------------- */
FamCore *fam_core_new(const char *config_path, char **err);
void fam_core_free(FamCore *core);
void fam_free_string(char *value);

int fam_stats(FamCore *core, char **out, char **err);

int fam_tag_add(FamCore *core, const char *path, const char *tags_json, const char *source,
                int32_t priority, char **out, char **err);
int fam_tag_list(FamCore *core, const char *path, char **out, char **err);
int fam_tag_remove(FamCore *core, const char *path, const char *tags_json, const char *source,
                   char **out, char **err);
int fam_find(FamCore *core, const char *tags_json, int match_all, const char *within,
             const char *source, uint32_t limit, char **out, char **err);

/* Tokens identify live view models. 0 signals failure. */
uint64_t fam_view_new(FamCore *core, const char *sort, const char *order, char **err);
int fam_view_search(FamCore *core, uint64_t view, const char *query, const char *within,
                    const char *kind, uint32_t limit, char **out, char **err);
int fam_view_drain(FamCore *core, uint64_t view, char **out, char **err);
void fam_view_free(FamCore *core, uint64_t view);

uint64_t fam_watch_start(FamCore *core, const char *dir, char **err);
void fam_watch_stop(FamCore *core, uint64_t watch);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* FAMCORE_H */
