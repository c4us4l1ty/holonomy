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

    pub fn visible_entries(&self, filter: &str, hide_hidden: bool, descending: bool) -> Vec<Entry> {
        let needle = filter.to_lowercase();
        let mut entries: Vec<_> = self.entries.iter().filter(|entry| {
            let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
            (!hide_hidden || !name.starts_with('.')) && name.to_lowercase().contains(&needle)
        }).cloned().collect();
        entries.sort_by(|a, b| b.directory.cmp(&a.directory).then_with(|| {
            let order = a.path.cmp(&b.path);
            if descending { order.reverse() } else { order }
        }));
        entries
    }

    pub fn create_folder(&mut self, name: &str) -> std::io::Result<()> {
        if !valid_folder_name(name) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Use one folder name (1–255 bytes), not a path, dot name or control characters."));
        }
        std::fs::create_dir(self.folder.join(name))?;
        self.refresh();
        Ok(())
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

pub fn valid_folder_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && name != "." && name != ".."
        && !name.chars().any(|c| c == '/' || c == '\\' || c.is_control())
}

/// The text input path must be lossless and bounded before navigation changes any session.
pub fn input_path(path: &Path) -> Option<&str> {
    path.to_str().filter(|path| path.len() <= 4096)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_names_cannot_escape_or_overwrite_and_views_filter_metadata() {
        for name in ["", ".", "..", "../outside", "a/b", "a\\b", "a\nb"] { assert!(!valid_folder_name(name)); }
        assert!(!valid_folder_name(&"x".repeat(256)));
        assert!(valid_folder_name("Research notes"));
        let library = Library { folder: ".".into(), notice: String::new(), entries: vec![
            Entry { path: ".hidden".into(), directory: true },
            Entry { path: "Beta.wavefunction".into(), directory: false },
            Entry { path: "Alpha.wavefunction".into(), directory: false },
        ] };
        assert_eq!(library.visible_entries("alpha", true, false).len(), 1);
        assert_eq!(library.visible_entries("", true, true)[0].path, PathBuf::from("Beta.wavefunction"));
        assert_eq!(library.visible_entries("", false, false).len(), 3);
    }

    #[test]
    fn unrepresentable_library_paths_are_refused_before_navigation() {
        assert_eq!(
            input_path(Path::new("notes.wavefunction")),
            Some("notes.wavefunction")
        );
        assert!(input_path(Path::new(&"x".repeat(4096))).is_some());
        assert!(input_path(Path::new(&"x".repeat(4097))).is_none());
        #[cfg(unix)]
        {
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
