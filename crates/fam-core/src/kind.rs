use std::path::Path;

use serde::{Deserialize, Serialize};

/// Coarse display bucket for a file.
///
/// The legacy stack filtered enumeration through a media-extension whitelist that contradicted
/// itself between config and code, and could not show non-media files at all
/// (`docs/legacy-specs.md` §2). A resource manager shows everything; the bucket only decides which
/// section a hit renders under, so this table groups and never filters.
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

    /// Declaration index, used as the `u8` carried by [`crate::Item`].
    pub const fn index(self) -> u8 {
        match self {
            Kind::Directory => 0,
            Kind::Image => 1,
            Kind::Video => 2,
            Kind::Audio => 3,
            Kind::Document => 4,
            Kind::Archive => 5,
            Kind::Code => 6,
            Kind::Executable => 7,
            Kind::Other => 8,
        }
    }

    pub fn from_index(index: u8) -> Option<Self> {
        KIND_ORDER.get(index as usize).copied()
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lowercases an extension before lookup, so `.PNG` and `.png` land in the same bucket — the
/// mismatch recorded in `docs/legacy-specs.md` §2.2-2.
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

        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg"
        | "m2ts" | "3gp" => Kind::Video,

        "mp3" | "flac" | "wav" | "ogg" | "oga" | "m4a" | "aac" | "wma" | "opus" | "aiff"
        | "mid" | "ape" => Kind::Audio,

        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "iso" | "cab" | "apk" => {
            Kind::Archive
        }

        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "markdown"
        | "rtf" | "odt" | "ods" | "odp" | "epub" | "mobi" | "azw3" | "csv" | "tsv" | "lnk" => {
            Kind::Document
        }

        "rs" | "go" | "java" | "kt" | "ts" | "tsx" | "js" | "jsx" | "py" | "rb" | "php"
        | "swift" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "sh" | "ps1" | "bat" | "cmd"
        | "sql" | "html" | "css" | "scss" | "vue" | "json" | "xml" | "yaml" | "yml" | "toml"
        | "ini" | "conf" | "log" => Kind::Code,

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

const MAX_EXTENSION_UNITS: usize = 16;

/// [`classify`] for a name the caller already holds as UTF-16 units, which is the shape the Win32
/// enumeration hands over. Allocation-free on purpose: this runs once per directory entry, and an
/// extension longer than [`MAX_EXTENSION_UNITS`] or containing anything outside ASCII cannot be in
/// the table anyway, so neither case needs a `String`.
pub fn classify_units(name: &[u16], is_directory: bool) -> Kind {
    if is_directory {
        return Kind::Directory;
    }
    let Some(dot) = name.iter().rposition(|unit| *unit == b'.' as u16) else {
        return Kind::Other;
    };
    let extension = &name[dot + 1..];
    if extension.is_empty() || extension.len() > MAX_EXTENSION_UNITS {
        return Kind::Other;
    }
    let mut buffer = [0u8; MAX_EXTENSION_UNITS];
    for (slot, unit) in buffer.iter_mut().zip(extension) {
        if *unit >= 0x80 {
            // The table is ASCII-only, so a non-ASCII extension can never match.
            return Kind::Other;
        }
        *slot = (*unit as u8).to_ascii_lowercase();
    }
    let text = std::str::from_utf8(&buffer[..extension.len()]).unwrap_or("");
    kind_of_extension(text)
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
        assert_eq!(
            classify(Path::new("D:/archive.tar.gz"), false),
            Kind::Archive
        );
    }

    #[test]
    fn every_kind_round_trips_through_its_name() {
        for kind in KIND_ORDER {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(Kind::parse("nonsense"), None);
    }

    #[test]
    fn units_and_path_forms_always_agree() {
        let names = [
            "photo.PNG",
            "movie.mkv",
            "song.flac",
            "book.epub",
            "tool.rs",
            "run.exe",
            "archive.tar.gz",
            "Makefile",
            ".gitignore",
            "note.超",
            "a.abcdefghijklmnop",
            "a.abcdefghijklmnopq",
            "trailing.",
            "weird..TXT",
        ];
        for name in names {
            let path = Path::new("D:/x").join(name);
            let units = crate::raw_name_units(&path);
            assert_eq!(
                classify_units(&units, false),
                classify(&path, false),
                "{name}"
            );
        }
        assert_eq!(classify_units(&[], true), Kind::Directory);
    }

    #[test]
    fn the_wire_index_is_the_declaration_order() {
        for (index, kind) in KIND_ORDER.into_iter().enumerate() {
            assert_eq!(kind.index(), index as u8);
            assert_eq!(Kind::from_index(kind.index()), Some(kind));
        }
        assert_eq!(Kind::from_index(KIND_ORDER.len() as u8), None);
    }
}
