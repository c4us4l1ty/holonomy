//! Bounded, metadata-only folder navigation. Never reads document bodies or writes an index.
use std::path::{Path, PathBuf};

const MAX_SCANNED: usize = 1024;
const MAX_VISIBLE: usize = 128;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub directory: bool,
}

pub struct Library {
    pub folder: PathBuf,
    pub entries: Vec<Entry>,
    pub notice: String,
}

impl Library {
    pub fn new(folder: PathBuf) -> Self {
        let mut library = Self {
            folder,
            entries: Vec::new(),
            notice: String::new(),
        };
        library.refresh();
        library
    }

    pub fn navigate(&mut self, folder: &Path) {
        self.folder = if folder.is_absolute() {
            folder.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| ".".into())
                .join(folder)
        };
        self.refresh();
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        self.notice.clear();
        let listing = match std::fs::read_dir(&self.folder) {
            Ok(listing) => listing,
            Err(error) => {
                self.notice = format!("Cannot list folder: {error}");
                return;
            }
        };
        for (index, item) in listing.enumerate() {
            if index >= MAX_SCANNED {
                self.notice =
                    "Large folder: showing a bounded subset. Navigate into a smaller folder."
                        .into();
                break;
            }
            let Ok(item) = item else {
                self.notice = "Some folder entries could not be read. Refresh to retry.".into();
                continue;
            };
            let Ok(kind) = item.file_type() else { continue };
            // Do not follow links into another tree, devices, or pipes.
            if kind.is_dir()
                || (kind.is_file()
                    && item
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "wavefunction"))
            {
                self.entries.push(Entry {
                    path: item.path(),
                    directory: kind.is_dir(),
                });
            }
        }
        self.entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.path.cmp(&b.path))
        });
        if self.entries.len() > MAX_VISIBLE {
            self.entries.truncate(MAX_VISIBLE);
            self.notice =
                "Showing the first 128 folders/documents. Use Open for other paths.".into();
        }
    }
}

/// The text input path must be lossless and bounded before navigation changes any session.
pub fn input_path(path: &Path) -> Option<&str> {
    path.to_str().filter(|path| path.len() <= 4096)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrepresentable_library_paths_are_refused_before_navigation() {
        assert_eq!(input_path(Path::new("notes.wavefunction")), Some("notes.wavefunction"));
        assert!(input_path(Path::new(&"x".repeat(4096))).is_some());
        assert!(input_path(Path::new(&"x".repeat(4097))).is_none());
        #[cfg(unix)] {
            use std::os::unix::ffi::OsStringExt;
            let invalid = std::ffi::OsString::from_vec(vec![b'n', 0xff]);
            assert!(input_path(Path::new(&invalid)).is_none());
        }
    }

    #[test]
    fn metadata_browser_filters_sorts_refreshes_and_handles_missing_folders() {
        let root = std::env::temp_dir().join(format!("holonomy-library-{}", std::process::id()));
        std::fs::create_dir_all(root.join("folder")).unwrap();
        std::fs::write(root.join("b.wavefunction"), b"not read by browser").unwrap();
        std::fs::write(root.join("a.txt"), b"not a document").unwrap();
        let mut library = Library::new(root.clone());
        assert_eq!(library.entries.len(), 2);
        assert!(library.entries[0].directory);
        assert!(library.entries[1].path.ends_with("b.wavefunction"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                root.join("b.wavefunction"),
                root.join("linked.wavefunction"),
            )
            .unwrap();
            library.refresh();
            assert_eq!(library.entries.len(), 2);
        }
        for n in 0..140 {
            std::fs::write(root.join(format!("{n}.wavefunction")), b"").unwrap();
        }
        library.refresh();
        assert_eq!(library.entries.len(), MAX_VISIBLE);
        assert!(!library.notice.is_empty());
        library.navigate(&root.join("folder"));
        assert!(library.entries.is_empty());
        library.navigate(&root.join("missing"));
        assert!(!library.notice.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
