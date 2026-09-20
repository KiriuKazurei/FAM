use std::path::Path;

use serde::{Deserialize, Serialize};

/// Coarse display bucket for a file.
///
/// The legacy stack filtered enumeration through a media-extension whitelist that contradicted
/// itself between config and code, and could not show non-media files at all
/// (`docs/legacy-specs.md` §2). A resource manager shows everything; the bucket only decides which
/// section a hit renders under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Directory,
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Code,
    Executable,
    Other,
}

pub const KIND_ORDER: [Kind; 9] = [
    Kind::Directory,
    Kind::Image,
    Kind::Video,
    Kind::Audio,
    Kind::Document,
    Kind::Archive,
    Kind::Code,
    Kind::Executable,
    Kind::Other,
];

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Directory => "directory",
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Document => "document",
            Kind::Archive => "archive",
            Kind::Code => "code",
            Kind::Executable => "executable",
            Kind::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        KIND_ORDER.into_iter().find(|kind| kind.as_str() == value)
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lowercases an extension before lookup, so `.PNG` and `.png` land in the same bucket — the
/// mismatch recorded in §2.2-2.
pub fn extension_of(path: &Path) -> Option<String> {
    let name = path.file_name()?;
    let text = name.to_string_lossy();
    let (_, extension) = text.rsplit_once('.')?;
    if extension.is_empty() || extension.len() > 16 {
        return None;
    }
    Some(extension.to_ascii_lowercase())
}

pub fn kind_of_extension(extension: &str) -> Kind {
    match extension {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "heif"
        | "svg" | "ico" | "avif" | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" => Kind::Image,

        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg" | "m2ts"
        | "3gp" => Kind::Video,

        "mp3" | "flac" | "wav" | "ogg" | "oga" | "m4a" | "aac" | "wma" | "opus" | "aiff" | "mid"
        | "ape" => Kind::Audio,

        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "iso" | "cab" | "apk" => {
            Kind::Archive
        }

        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "markdown" | "rtf" | "odt"
        | "ods" | "odp" | "epub" | "mobi" | "azw3" | "csv" | "tsv" | "lnk" => Kind::Document,

        "rs" | "go" | "java" | "kt" | "ts" | "tsx" | "js" | "jsx" | "py" | "rb" | "php" | "swift"
        | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "sh" | "ps1" | "bat" | "cmd" | "sql"
        | "html" | "css" | "scss" | "vue" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini"
        | "conf" | "log" => Kind::Code,

        "exe" | "dll" | "ocx" | "sys" | "msi" | "so" | "dylib" | "jar" | "pyd" | "lib" | "a"
        | "obj" => Kind::Executable,

        _ => Kind::Other,
    }
}

pub fn classify(path: &Path, is_directory: bool) -> Kind {
    if is_directory {
        return Kind::Directory;
    }
    match extension_of(path) {
        Some(extension) => kind_of_extension(&extension),
        None => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercase_extensions_classify_identically() {
        assert_eq!(classify(Path::new("D:/a/photo.PNG"), false), Kind::Image);
        assert_eq!(classify(Path::new("D:/a/photo.png"), false), Kind::Image);
        assert_eq!(classify(Path::new("D:/a/MOVIE.MP4"), false), Kind::Video);
    }

    #[test]
    fn directories_win_regardless_of_extension() {
        assert_eq!(classify(Path::new("D:/photos.png"), true), Kind::Directory);
    }

    #[test]
    fn extensionless_and_dotfile_names_fall_back_to_other() {
        assert_eq!(classify(Path::new("D:/Makefile"), false), Kind::Other);
        assert_eq!(classify(Path::new("D:/.gitignore"), false), Kind::Other);
        assert_eq!(classify(Path::new("D:/archive.tar.gz"), false), Kind::Archive);
    }

    #[test]
    fn every_kind_round_trips_through_its_name() {
        for kind in KIND_ORDER {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(Kind::parse("nonsense"), None);
    }

    #[test]
    fn buckets_cover_the_media_types_the_legacy_whitelist_claimed() {
        for extension in ["jpg", "png", "mp4", "avi", "pdf", "docx", "mp3"] {
            assert_ne!(kind_of_extension(extension), Kind::Other, "{extension}");
        }
    }
}
