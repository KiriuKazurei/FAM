use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::StoreError;

pub const MAX_TAG_CHARS: usize = 64;

/// Extra characters beyond alphanumerics that a tag may contain. Path separators are deliberately
/// absent: a tag must never be able to look like a path.
const TAG_PUNCTUATION: &[char] = &[
    ' ', '-', '_', '.', '#', ',', ';', '(', ')', '（', '）', '、', '，', '。', '！', '？',
];

/// Where a tag came from. Ordering is part of the contract: manual tags outrank automatic ones
/// (`docs/legacy-specs.md` §1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Manual,
    Auto,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Manual, Source::Auto];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Manual => "manual",
            Source::Auto => "auto",
        }
    }

    /// Accepts the legacy spelling `AI`, which §1.1 records as the old `source` value.
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" => Ok(Source::Manual),
            "auto" | "ai" => Ok(Source::Auto),
            other => Err(StoreError::BadSource(other.to_string())),
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagRecord {
    pub tag: String,
    pub source: Source,
    pub priority: i32,
    pub created_at: String,
}

/// Ordering key shared by every read path: manual first, then priority, then the tag itself.
pub fn display_order(a: &TagRecord, b: &TagRecord) -> std::cmp::Ordering {
    match a.source {
        Source::Manual => match b.source {
            Source::Manual => std::cmp::Ordering::Equal,
            Source::Auto => std::cmp::Ordering::Less,
        },
        Source::Auto => match b.source {
            Source::Manual => std::cmp::Ordering::Greater,
            Source::Auto => std::cmp::Ordering::Equal,
        },
    }
    .then_with(|| b.priority.cmp(&a.priority))
    .then_with(|| a.tag.cmp(&b.tag))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTag {
    pub tag: String,
    pub source: Source,
    pub priority: i32,
}

impl NewTag {
    pub fn manual(tag: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            source: Source::Manual,
            priority: 0,
        }
    }

    pub fn auto(tag: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            source: Source::Auto,
            priority: 0,
        }
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        validate_tag(&self.tag)?;
        Ok(())
    }
}

/// Validates tag text.
///
/// The legacy check was `len(tag) <= 64` on UTF-8 *bytes*, which capped Chinese tags at about 21
/// characters (§1.3-3). This measures characters and says so; a Rust `chars().count()` switch was
/// the documented open question and this is the answer: characters, limit 64.
pub fn validate_tag(tag: &str) -> Result<(), StoreError> {
    if tag.is_empty() {
        return Err(StoreError::InvalidTag {
            tag: tag.into(),
            reason: "tag is empty",
        });
    }
    if tag.trim() != tag {
        return Err(StoreError::InvalidTag {
            tag: tag.into(),
            reason: "tag has leading or trailing whitespace",
        });
    }
    let count = tag.chars().count();
    if count > MAX_TAG_CHARS {
        return Err(StoreError::InvalidTag {
            tag: tag.into(),
            reason: "tag longer than 64 characters",
        });
    }
    for c in tag.chars() {
        let allowed = c.is_alphanumeric() || TAG_PUNCTUATION.contains(&c);
        if !allowed {
            return Err(StoreError::InvalidTag {
                tag: tag.into(),
                reason: "tag contains a disallowed character",
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTags {
    pub path: PathBuf,
    pub tags: Vec<TagRecord>,
}

impl FileTags {
    pub fn tag_names(&self) -> Vec<&str> {
        self.tags.iter().map(|t| t.tag.as_str()).collect()
    }
}

/// One tag name and how many files carry it - the shape a filter bar is drawn from.
///
/// Deliberately no `source` in here: the bar's job is "which labels exist and how much is under
/// each", and splitting a name's count by source would show `contract (2)` and `contract (1)` as two
/// things to click when removing one removes both (`remove_tags` deletes by name across sources,
/// which is the same fact the shell's tag dialog had to draw around).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFacet {
    pub tag: String,
    pub files: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchRequest {
    /// Tag names the file must carry, **all of them** (AND). Populated by the parser for plain
    /// `tag:a` tokens and for comma groups (`tag:a,b`); separate tokens AND across.
    pub tags: Vec<String>,
    /// OR groups: the file must carry **at least one** name of each group. Populated by
    /// `tag:a||b` and by `tag:a,b||c` (the comma binds tighter: `(a AND b) OR c`).
    pub groups: Vec<Vec<String>>,
    /// Tag names the file must **not** carry - exclusion wins over every inclusion, and files
    /// without the wanted tags simply stay out of the answer (nothing is invented for them).
    pub tags_not: Vec<String>,
    pub source: Option<Source>,
    pub within: Option<PathBuf>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

impl SearchRequest {
    pub fn new(tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            tags: tags.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    }

    pub fn within(mut self, dir: impl AsRef<Path>) -> Self {
        self.within = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn source(mut self, source: Source) -> Self {
        self.source = Some(source);
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        if self.tags.is_empty() && self.groups.is_empty() && self.tags_not.is_empty() {
            /* An entirely empty request asks for nothing and is refused; an exclusion-only one is
             * not empty - "every tagged file except these names" is a real question. */
            return Err(StoreError::InvalidSearch(
                "at least one tag is required".into(),
            ));
        }
        for tag in &self.tags {
            validate_tag(tag)?;
        }
        for group in &self.groups {
            if group.is_empty() {
                return Err(StoreError::InvalidSearch(
                    "a tag group cannot be empty".into(),
                ));
            }
            for tag in group {
                validate_tag(tag)?;
            }
        }
        for tag in &self.tags_not {
            validate_tag(tag)?;
        }
        Ok(())
    }
}

/// Half-open range `[lower, upper)` covering everything nested under `parent_key`, or `None` when
/// the parent is a volume/root boundary where the range would be wrong.
pub(crate) fn descendant_range(parent_key: &str) -> Option<(String, String)> {
    if parent_key.is_empty() || parent_key == "/" {
        return None;
    }
    let base = parent_key.trim_end_matches('/');
    Some((format!("{base}/"), format!("{base}0")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_cjk_tags_pass() {
        validate_tag("invoice-2026").unwrap();
        validate_tag("合同 审阅").unwrap();
        validate_tag("v2").unwrap();
        validate_tag("（重要）").unwrap();
    }

    #[test]
    fn path_separators_are_rejected() {
        for bad in ["a/b", r"a\b", "C:/x"] {
            let error = validate_tag(bad).unwrap_err();
            assert!(
                matches!(error, StoreError::InvalidTag { .. }),
                "{bad:?} should not be a valid tag"
            );
        }
    }

    #[test]
    fn limit_counts_characters_not_bytes() {
        let cjk = "标".repeat(MAX_TAG_CHARS);
        assert_eq!(cjk.len(), MAX_TAG_CHARS * 3, "fixture must be multi-byte");
        validate_tag(&cjk).unwrap();
        let too_long = "标".repeat(MAX_TAG_CHARS + 1);
        assert!(validate_tag(&too_long).is_err());
        let ascii = "a".repeat(MAX_TAG_CHARS);
        validate_tag(&ascii).unwrap();
    }

    #[test]
    fn blank_and_padded_tags_are_rejected() {
        assert!(validate_tag("").is_err());
        assert!(validate_tag(" x").is_err());
        assert!(validate_tag("x ").is_err());
        assert!(validate_tag("a\tb").is_err());
    }

    #[test]
    fn legacy_source_spellings_map_onto_auto() {
        assert_eq!(Source::parse("AI").unwrap(), Source::Auto);
        assert_eq!(Source::parse(" auto ").unwrap(), Source::Auto);
        assert_eq!(Source::parse("MANUAL").unwrap(), Source::Manual);
        assert!(Source::parse("whatever").is_err());
    }

    #[test]
    fn manual_outranks_auto_then_priority_then_name() {
        let record = |tag: &str, source: Source, priority: i32| TagRecord {
            tag: tag.into(),
            source,
            priority,
            created_at: String::new(),
        };
        let mut tags = vec![
            record("auto-high", Source::Auto, 9),
            record("manual-low", Source::Manual, 0),
            record("manual-high", Source::Manual, 5),
            record("b", Source::Auto, 1),
            record("a", Source::Auto, 1),
        ];
        tags.sort_by(display_order);
        let order: Vec<&str> = tags.iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(
            order,
            vec!["manual-high", "manual-low", "auto-high", "a", "b"]
        );
    }

    #[test]
    fn descendant_range_covers_only_nested_paths() {
        let (lower, upper) = descendant_range("D:/FAM").unwrap();
        assert_eq!(lower, "D:/FAM/");
        assert_eq!(upper, "D:/FAM0");
        let in_range = |key: &str| key >= lower.as_str() && key < upper.as_str();

        assert!(in_range("D:/FAM/DOC.TXT"));
        assert!(in_range("D:/FAM/A/B/C.PDF"));
        assert!(in_range("D:/FAM/!URGENT"));
        assert!(
            !in_range("D:/FAM"),
            "the directory itself is matched by equality"
        );
        assert!(
            !in_range("D:/FAM.PARENT"),
            "shared-prefix sibling must fall out"
        );
        assert!(
            !in_range("D:/FAMFOO"),
            "shared-prefix sibling must fall out"
        );
        assert_eq!(descendant_range("/"), None);
        assert_eq!(descendant_range(""), None);
    }
}
