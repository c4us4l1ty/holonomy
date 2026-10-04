//! Command-line parsing, as a pure function.
//!
//! # Why every file open is a flag, not an action
//!
//! The jail's allowlist has no `open` -- see
//! [`holonomy_jail::seccomp::table::ALLOWLIST`]. So a path that reaches `open` *after* the boot
//! chain is a `SIGSYS` and an exit 137, which reads like a crash rather than like a design rule.
//!
//! [`Args::parse`] therefore returns *paths*, and `main` opens every one of them **before** the boot
//! chain runs. The session consumes descriptors and never sees a path. Parsing and opening are
//! separate steps on purpose: it is the difference between "the user asked for an HTML export" and
//! "the exporter opened a file", and only the second one can fail after sealing.
//!
//! # Unknown flags are errors
//!
//! Not warnings. A typo in `--export-pdf` would otherwise produce a file the user did not ask for --
//! or silently no file at all -- and both are worse than being told. Same reason
//! [`holonomy_export::Format::parse`] returns `None` rather than defaulting.

use std::path::PathBuf;

use holonomy_export::Format;

/// A file the session will export to, opened before the jail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportTarget {
    /// The format to write.
    pub format: Format,
    /// Where. Opened during boot, never after.
    pub path: PathBuf,
}

/// The parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Container to open. `None` creates one.
    pub container: Option<PathBuf>,
    /// Where to write the frame as a PPM, for the visual baseline.
    pub screenshot: Option<PathBuf>,
    /// Exports to perform on exit.
    pub exports: Vec<ExportTarget>,
    /// Panel width.
    pub width: u32,
    /// Panel height.
    pub height: u32,
    /// Zoom, as a percentage.
    pub zoom: u32,
    /// Run headless: no DRM, no evdev, no jail. The integration gate's mode.
    pub headless: bool,
    /// Open a window on a desktop display, instead of the jail's panel. Requires the `desktop` feature.
    pub window: bool,
    /// Which display, for `--window`. `$DISPLAY` when absent.
    pub display: Option<String>,
    /// Print what would happen without doing it.
    pub dry_run: bool,
    /// Type a scripted event stream from this file instead of reading a keyboard.
    pub script: Option<PathBuf>,
    /// The passphrase. Read from `HOLONOMY_PASSPHRASE` rather than the command line, because an
    /// argument is visible in `/proc/*/cmdline` to every process on the machine.
    pub passphrase_env: &'static str,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            container: None,
            screenshot: None,
            exports: Vec::new(),
            width: 1280,
            height: 800,
            zoom: 100,
            headless: false,
            window: false,
            display: None,
            dry_run: false,
            script: None,
            passphrase_env: "HOLONOMY_PASSPHRASE",
        }
    }
}

/// Why a command line was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A flag with no value.
    MissingValue {
        /// The flag.
        flag: String,
    },
    /// A flag nobody recognises.
    Unknown {
        /// The flag, without its dashes.
        flag: String,
    },
    /// A value that is not a number.
    NotANumber {
        /// The flag.
        flag: String,
        /// What was given.
        value: String,
    },
    /// A value outside its range.
    OutOfRange {
        /// The flag.
        flag: String,
        /// What was given.
        value: u32,
        /// The smallest legal value.
        min: u32,
        /// The largest legal value.
        max: u32,
    },
    /// The same format exported twice.
    DuplicateFormat {
        /// The format.
        format: Format,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingValue { flag } => write!(f, "{flag} needs a value"),
            Self::Unknown { flag } => {
                if flag.starts_with('/') || flag.contains('/') {
                    // A positional. `trim_start_matches('-')` does not touch it, so it arrives whole,
                    // and the message has to read differently -- "unknown flag --/tmp/x" would be a
                    // confusing way to say "there is no positional argument".
                    write!(f, "unknown argument {flag:?}; every input is a --flag")
                } else {
                    write!(f, "unknown flag --{flag}")
                }
            }
            Self::NotANumber { flag, value } => write!(f, "--{flag} wants a number, got {value:?}"),
            Self::OutOfRange {
                flag,
                value,
                min,
                max,
            } => {
                write!(f, "--{flag} wants {min}..={max}, got {value}")
            }
            Self::DuplicateFormat { format } => {
                write!(
                    f,
                    "--export-{} given twice; it would truncate the first",
                    format.extension()
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

impl Args {
    /// The flags this build accepts, for `--help` and for the error messages above.
    pub const USAGE: &'static str = "\
holonomy --container <path> [--headless] [--screenshot <path.ppm>]
         [--export-html <path>] [--export-pdf <path>]
         [--script <path>] [--width N] [--height N] [--zoom N] [--dry-run]

  --container <path>   Open or create this .wavefunction container.
  --screenshot <path>  Dump the final frame as a PPM.
  --export-html <path> Write the document as HTML on exit.
  --export-pdf <path>  Write the document as a PDF on exit.
  --script <path>      Replay a scripted input_event stream instead of a keyboard.
  --width N            Panel width in pixels (default 1280).
  --height N           Panel height in pixels (default 800).
  --zoom N             Zoom percentage, 25..=400 (default 100).
  --headless           No DRM, no evdev, no jail.
  --dry-run            Print the boot plan and exit without opening anything.

The passphrase is read from $HOLONOMY_PASSPHRASE, never from the command line:
an argument is visible in /proc/*/cmdline to every process on the machine.";

    /// Parse an argument list, **excluding** argv[0].
    pub fn parse<I, S>(args: I) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut out = Args::default();
        let mut it = args.into_iter().map(Into::into).peekable();
        while let Some(arg) = it.next() {
            // `--flag=value` as well as `--flag value`, because a path with a space in it is much
            // easier to write the first way.
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) => (f.to_string(), Some(v.to_string())),
                None => (arg.clone(), None),
            };
            let mut value = || -> Result<String, ParseError> {
                match inline.clone() {
                    Some(v) => Ok(v),
                    None => it
                        .next()
                        .ok_or(ParseError::MissingValue { flag: flag.clone() }),
                }
            };
            match flag.as_str() {
                "--container" => out.container = Some(PathBuf::from(value()?)),
                "--screenshot" => out.screenshot = Some(PathBuf::from(value()?)),
                "--script" => out.script = Some(PathBuf::from(value()?)),
                "--export-html" => out.push_export(Format::Html, PathBuf::from(value()?))?,
                "--export-pdf" => out.push_export(Format::Pdf, PathBuf::from(value()?))?,
                "--width" => out.width = parse_u32(&flag, &value()?)?,
                "--height" => out.height = parse_u32(&flag, &value()?)?,
                "--zoom" => out.zoom = parse_u32(&flag, &value()?)?,
                "--headless" => out.headless = true,
                "--window" => out.window = true,
                "--display" => out.display = Some(value()?),
                "--dry-run" => out.dry_run = true,
                "--help" | "-h" => {
                    return Err(ParseError::Unknown {
                        flag: "help".into(),
                    })
                }
                other => {
                    // A bare path is not accepted: `--container` is required, and a silent
                    // positional would be ambiguous with it.
                    return Err(ParseError::Unknown {
                        flag: other.trim_start_matches('-').to_string(),
                    });
                }
            }
        }
        if out.width == 0 || out.height == 0 {
            return Err(ParseError::OutOfRange {
                flag: if out.width == 0 { "width" } else { "height" }.into(),
                value: if out.width == 0 {
                    out.width
                } else {
                    out.height
                },
                min: 1,
                max: 7680,
            });
        }
        if !(25..=400).contains(&out.zoom) {
            return Err(ParseError::OutOfRange {
                flag: "zoom".into(),
                value: out.zoom,
                min: 25,
                max: 400,
            });
        }
        Ok(out)
    }

    /// Add an export, refusing a second one of the same format.
    ///
    /// Two `--export-pdf` flags would open the same path twice and the second would truncate the
    /// first, which is a silent data loss with a plausible-looking command line.
    fn push_export(&mut self, format: Format, path: PathBuf) -> Result<(), ParseError> {
        if self.exports.iter().any(|e| e.format == format) {
            return Err(ParseError::DuplicateFormat { format });
        }
        self.exports.push(ExportTarget { format, path });
        Ok(())
    }
}

/// Parse a `u32`, with the flag named in both error variants.
fn parse_u32(flag: &str, value: &str) -> Result<u32, ParseError> {
    value.parse().map_err(|_| ParseError::NotANumber {
        flag: flag.trim_start_matches('-').to_string(),
        value: value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, ParseError> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn an_empty_command_line_is_the_defaults() {
        let a = parse(&[]).expect("empty is fine");
        assert_eq!(a, Args::default());
        assert!(!a.headless);
        assert!(a.exports.is_empty());
    }

    #[test]
    fn a_container_is_named() {
        let a = parse(&["--container", "/tmp/x.wavefunction"]).expect("parse");
        assert_eq!(a.container, Some(PathBuf::from("/tmp/x.wavefunction")));
    }

    #[test]
    fn exports_are_collected_and_deduplicated() {
        let a = parse(&["--export-html=/tmp/a.html", "--export-pdf", "/tmp/a.pdf"]).expect("parse");
        assert_eq!(a.exports.len(), 2);
        assert_eq!(a.exports[0].format, Format::Html);
        assert_eq!(a.exports[0].path, PathBuf::from("/tmp/a.html"));
        assert_eq!(a.exports[1].format, Format::Pdf);

        // The same format twice would truncate the first.
        assert_eq!(
            parse(&["--export-html", "/tmp/a", "--export-html", "/tmp/b"]),
            Err(ParseError::DuplicateFormat {
                format: Format::Html
            })
        );
    }

    #[test]
    fn an_unknown_flag_is_an_error_not_a_warning() {
        assert_eq!(
            parse(&["--export-epub", "/tmp/a"]),
            Err(ParseError::Unknown {
                flag: "export-epub".into()
            })
        );
        // Including a bare positional, which arrives whole: trimming leading dashes leaves
        // `/tmp/x` untouched, and the `Display` arm says "argument" rather than "flag" for it.
        assert_eq!(
            parse(&["/tmp/x"]),
            Err(ParseError::Unknown {
                flag: "/tmp/x".into()
            })
        );
    }

    #[test]
    fn a_flag_without_its_value_is_an_error() {
        assert_eq!(
            parse(&["--container"]),
            Err(ParseError::MissingValue {
                flag: "--container".into()
            })
        );
        // And `--flag=` is a *present* empty value, not a missing one.
        assert_eq!(
            parse(&["--container="]),
            Ok(Args {
                container: Some(PathBuf::new()),
                ..Args::default()
            })
        );
    }

    #[test]
    fn numbers_are_parsed_and_bounded() {
        assert_eq!(parse(&["--width", "640"]).expect("ok").width, 640);
        assert_eq!(
            parse(&["--width", "wide"]),
            Err(ParseError::NotANumber {
                flag: "width".into(),
                value: "wide".into()
            })
        );
        assert_eq!(
            parse(&["--zoom", "1000"]),
            Err(ParseError::OutOfRange {
                flag: "zoom".into(),
                value: 1000,
                min: 25,
                max: 400
            })
        );
        assert_eq!(
            parse(&["--width", "0"]),
            Err(ParseError::OutOfRange {
                flag: "width".into(),
                value: 0,
                min: 1,
                max: 7680
            })
        );
    }

    #[test]
    fn flags_compose_and_repeat() {
        let a = parse(&[
            "--headless",
            "--dry-run",
            "--container=/tmp/c",
            "--width",
            "800",
            "--headless",
        ])
        .expect("parse");
        assert!(a.headless && a.dry_run);
        assert_eq!(a.width, 800);
    }
}
