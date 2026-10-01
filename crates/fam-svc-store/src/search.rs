//! The tag store as a search backend.
//!
//! A name search asks the filesystem what exists; this asks the library what somebody labelled, and
//! then goes to find out what those files look like. The two answers arrive in one shape -
//! [`SearchHit`] through the same [`SearchProvider`] contract - which is the whole reason a tag query
//! is a backend rather than a separate call the UI has to render specially: a hit carries a size, a
//! modified time, a kind and the folder-or-not bit, so the shell paints it as an ordinary row.
//!
//! The store cannot supply those four. Its `files` table is `(path_norm, path, updated_at)` - an
//! identity and a spelling, with no metadata column anywhere - so every hit is hydrated with one
//! `metadata` call on the path the store recorded. That is a filesystem read per hit, bounded by the
//! request's own limit, on the search thread rather than the UI's: the same trade the enumeration
//! backend already makes, and the reason this runs as a search rather than as a table lookup.

use std::sync::{Arc, Mutex, MutexGuard};

use fam_core::{
    classify, raw_path_units, Cancellation, PathKey, SearchError, SearchHit, SearchOutcome,
    SearchProvider, SearchRequest,
};

use crate::model::SearchRequest as TagRequest;
use crate::store::TagStore;

/// The ceiling this backend asks the store for when the caller named no limit of their own.
///
/// A number rather than "no limit" because the rows come back into this process: the store's own
/// default is what an agent's `find` call already gets, and a search backend that reads more than the
/// front end asked for is a memory story waiting to become a bug report.
const CANDIDATE_CEILING: usize = 5000;

/// The tag library answering a search.
pub struct TagSearch {
    store: Arc<Mutex<TagStore>>,
}

impl TagSearch {
    pub fn new(store: Arc<Mutex<TagStore>>) -> Self {
        Self { store }
    }

    /// The store, for the one query it is asked for.
    ///
    /// Locked and released before any row is hydrated: a search of a thousand tagged files holds this
    /// mutex for its SQL, not for its filesystem calls, because the GUI thread tags files through the
    /// same store and a lock held across `metadata` is a window that freezes the interface.
    fn locked(&self) -> MutexGuard<'_, TagStore> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SearchProvider for TagSearch {
    fn id(&self) -> &'static str {
        "tags"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn handles_tags(&self) -> bool {
        true
    }

    /// Exactly the requests that name a tag - and no others.
    ///
    /// This is not a scope claim but a subject claim: the store knows about every file it ever
    /// labelled, on any volume, and nothing at all about a file nobody tagged. Asked to answer a plain
    /// name search it would return the labelled subset and call it complete, which is the shorter list
    /// that looks exhaustive - the one failure mode the routing rules were built to refuse.
    fn covers(&self, request: &SearchRequest) -> bool {
        request.names_tags()
    }

    fn search(
        &self,
        request: &SearchRequest,
        cancel: &Cancellation,
        sink: &mut dyn FnMut(SearchHit) -> bool,
    ) -> Result<SearchOutcome, SearchError> {
        if !request.names_tags() {
            return Err(SearchError::Empty);
        }
        let asked = request.limit.max(1);
        let candidates = {
            let store = self.locked();
            let mut query = TagRequest::new(request.tags.clone());
            query.groups = request.tag_groups.clone();
            query.tags_not = request.tags_not.clone();
            if let Some(scope) = &request.scope {
                query = query.within(scope);
            }
            store
                .search(&query.limit(CANDIDATE_CEILING))
                .map_err(|error| SearchError::Unavailable {
                    provider: self.id(),
                    reason: error.to_string(),
                })?
        };

        let mut outcome = SearchOutcome::default();
        // "Complete" here means this backend looked at everything it could have shown the caller. The
        // three ways it does not: the store's own ceiling cut the candidate list short, somebody
        // cancelled, or the sink stopped taking rows. Reaching the caller's stated limit is *not* one of
        // them - that is the agreement the caller made, and the shell flags a search that filled its
        // ceiling on its own terms.
        let mut exhaustive = candidates.len() < CANDIDATE_CEILING;
        for found in candidates {
            outcome.visited += 1;
            if cancel.is_stopped() {
                exhaustive = false;
                break;
            }
            // A path the store still lists and the filesystem no longer has is not a file to show.
            // Deleting a file leaves its tag rows behind - measured, not assumed, in the increment 16
            // account - and a row drawn from that would be a name with no size to print, which is the
            // shape of the bug the row painter exists to avoid.
            let Ok(meta) = std::fs::metadata(&found.path) else {
                continue;
            };
            let directory = meta.is_dir();
            let kind = classify(&found.path, directory);
            if request.kind.is_some_and(|wanted| wanted != kind) {
                continue;
            }
            // The store matches tags and nothing else, so a request that also named words of a name is
            // narrowed here rather than in the query.
            let Some(name) = found.path.file_name() else {
                continue;
            };
            if !request.matches_name(&name.to_string_lossy().to_lowercase()) {
                continue;
            }
            outcome.matched += 1;
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as i64)
                .unwrap_or(i64::MIN);
            // A directory's size is the size of its entry, which is the same untruth `0 B` on a folder
            // was: the row painter drops the number for directories, so it is not printed either way,
            // and the search backends zero it for exactly that reason.
            let hit = SearchHit {
                key: PathKey::of(&found.path),
                path: raw_path_units(&found.path),
                kind,
                size: if directory { 0 } else { meta.len() },
                mtime_ms,
            };
            if !sink(hit) {
                exhaustive = false;
                break;
            }
            if outcome.matched >= asked as u64 {
                break;
            }
        }
        outcome.complete = exhaustive;
        Ok(outcome)
    }
}
