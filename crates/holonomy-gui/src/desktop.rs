//! Canonical desktop launcher. Never silently falls back to the legacy pixel renderer.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

fn companion(executable: &Path) -> std::io::Result<PathBuf> {
    let parent = executable.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "launcher has no parent directory",
        )
    })?;
    Ok(parent.join(if cfg!(windows) {
        "holonomy-native.exe"
    } else {
        "holonomy-native"
    }))
}

/// Runs the GPUI companion installed alongside this executable. Paths are passed as arguments,
/// never interpreted by a shell; the child owns the document and its shutdown lifecycle.
pub fn launch(doc: Option<&Path>, deadman: Option<Duration>) -> std::io::Result<ExitStatus> {
    let binary = companion(&std::env::current_exe()?)?;
    let mut command = Command::new(&binary);
    if let Some(doc) = doc {
        command.arg(doc);
    }
    if let Some(timeout) = deadman {
        command.env("HOLONOMY_DEADMAN_MS", timeout.as_millis().to_string());
    }
    // Replace the wrapper on Unix: signals/termination must reach the process that owns
    // the unlocked session, rather than leaving an orphaned GPUI child behind.
    #[cfg(unix)]
    let result = {
        use std::os::unix::process::CommandExt;
        Err(command.exec())
    };
    #[cfg(not(unix))]
    let result = command.status();
    result.map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "cannot start GPUI desktop {}: {error}. Build/install holonomy-native alongside the launcher: cargo build --locked -p holonomy-native --features native-ui. The legacy pixel UI is available only with HOLONOMY_LEGACY_GUI=1",
                binary.display()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_is_an_absolute_sibling_not_a_path_search() {
        let root = std::env::temp_dir().join("holonomy launcher test");
        let result = companion(&root.join("holonomy-gui")).unwrap();
        assert_eq!(result.parent(), Some(root.as_path()));
        assert_eq!(
            result.file_stem().unwrap().to_str(),
            Some("holonomy-native")
        );
    }
}
