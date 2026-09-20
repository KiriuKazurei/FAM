/* C ABI exported by the fam-ffi crate (famcore.dll).
 *
 * Strings in are UTF-8 C strings; strings out are heap allocations the caller must release with
 * fam_free_string. Payloads are JSON so no Rust type layout crosses this boundary.
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

uint32_t fam_abi_version(void);

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
