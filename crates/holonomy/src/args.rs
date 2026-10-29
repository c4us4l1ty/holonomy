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
    /// **Whether `--width` and `--height` were given, part 36.**
    ///
    /// **Recorded because a default is not a request.** The window driver sizes itself to the screen
    /// when the user has not asked for a size, and the only way to tell "did not ask" from "asked for
    /// the default" is to remember whether the flag appeared.
    pub sized: bool,
    /// Zoom, as a percentage.
    pub zoom: u32,
    /// **Pixels per em the faces are rasterised at.** This is the type size, and it is the one that
    /// actually makes the application bigger.
    pub ppem: u16,
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
            width: 1600,
            height: 1000,
            sized: false,
            zoom: 100,
            // **CORRECTION, part 36: 20, not 24.**
            //
            // Part 32 raised this from 16 to 24 because the type was too small, and it measured the
            // *atlas* ceiling (26, where `build_atlas` returns `AtlasFull`) rather than the **viewport**.
            // **Measured, at the default 1,600x1,000 window, with `Session::new`'s own setter order:**
            //
            // ```text
            //   ppem   bands   text height   rows   columns
            //    16     139         762        30      100
            //    20     166         735        24       98
            //    24     205         696        18       80
            // ```
            //
            // **Measured at the default 1,600x1,000 window**, with `Session::new`'s own setter order
            // (`with_type_scale` then `with_line_pitch`; the other order double-scales the bands and
            // reports 270 where the application uses 205).
            //
            // **24 ppem bought bigger letters with six fewer lines of document, and the measure it
            // implied was 80 columns where 100 fit** — the user reported it as *"the view port is way
            // too small and I cannot properly use the app"*. `with_type_scale` multiplies **every band
            // that holds type**, so the chrome grew from 139 px to 205 px of a 1,000 px window, and a
            // 37 px line pitch against a 25 px one costs a third of the rows whatever the height.
            //
            // **20 keeps part 32's complaint addressed** — a pitch of 30 against 25, and a cell of 12
            // against 10 — **without the viewport shrinking**, and it costs 20 px of chrome against
            // 16 ppem rather than 66 px against 24. `--ppem 24` remains, and is what part 32 measured
            // everything against.
            //
            // # # Part 46 moved this back to 16, and the reason is the atlas rather than the viewport
            //
            // **The table three lines above already answered the viewport question and 16 won it**:
            // 16 ppem shows **30 rows and 100 columns** against 20 ppem's **24 rows and 98 columns**,
            // so 16 is better on *both* axes of "I cannot properly use the app". Choosing 20 over 16
            // bought larger letters, not more document — and part 32 never said larger was the goal,
            // it said the viewport was too small.
            //
            // **What part 46 made true is that 16 is also the only default at which zoom works.**
            // Part 43 gave the atlas a second size so 125 % would have glyphs, and measured it once at
            // `[16, 20]` without asking whether the same expression holds elsewhere. It does not: two
            // sizes fit up to `--ppem 17` and the default was 20, so `cargo run` aborted with
            // `AtlasFull { need: 554236, have: 458752 }` and **the application could not start**.
            //
            // **So the default is now 16**, which is where the pair fits (378,850 of 458,752), where
            // 125 % zoom has glyphs behind it, **and where every test fixture in the suite already
            // built its atlas.** That last one is not incidental: several parts were spent on a gate
            // that failed because a test built `[16]` while the product built `[20]`, and the fix
            // for the next one of those is for the two to be the same number.
            //
            // **`--ppem 20` still works** and still gives larger type. It builds one atlas size and
            // says so once on stderr; see `crate::atlas_plan`.
            ppem: 16,
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
  --width N            Panel width in pixels. Unset, the window is sized
                         to four fifths of the screen; the default 1600 is
                         what the headless paths use.
  --height N           Panel height in pixels (default 1000).
                         Wider than 1280 grows the text column up to 100 columns
                         and then stops; past that the extra width is desk. Part 27.
  --ppem N             Pixels per em for the faces (default 16; 27 and above
                         do not fit the atlas). This is the type size: it
                         enlarges the glyphs and every band that holds them,
                         so it costs rows as well as pixels. Part 32 chose
                         24; part 36 measured the viewport and chose 20.
  --zoom N             Zoom percentage, 25..=400 (default 100).
                         CORRECTION, part 23: this does not zoom. It sets the
                         number the toolbar prints; the page does not change.
                         See PROJECT.md part 23 and crates/holonomy/tests/zoom.rs.
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
                "--width" => {
                    out.width = parse_u32(&flag, &value()?)?;
                    out.sized = true;
                }
                "--height" => {
                    out.height = parse_u32(&flag, &value()?)?;
                    out.sized = true;
                }
                "--ppem" => {
                    let ppem = parse_u32(&flag, &value()?)?;
                    // Range-check before narrowing: `65546 as u16` is 10, which used to
                    // smuggle an out-of-range value past the atlas clamp below.
                    out.ppem = u16::try_from(ppem).map_err(|_| ParseError::OutOfRange {
                        flag: "ppem".into(),
                        value: ppem,
                        min: 10,
                        max: 26,
                    })?;
                }
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
        for (flag, value) in [("width", out.width), ("height", out.height)] {
            if !(1..=7680).contains(&value) {
                return Err(ParseError::OutOfRange {
                    flag: flag.into(),
                    value,
                    min: 1,
                    max: 7680,
                });
            }
        }
        // **The atlas's own ceiling, measured.** At 28 ppem `build_atlas` returns `AtlasFull`:
        // `need: 406,068, have: 458,752`. 26 is the largest size that fits, and the range is clamped
        // here rather than at the build so that `--ppem 9999` says *why* rather than aborting on an
        // allocator error sixteen frames later.
        //
        // # **Every value in this range builds an atlas, and that is a gate**
        //
        // **`atlas_plan::tests`... no: `tests/atlas_plan.rs::every_legal_ppem_builds_an_atlas`
        // rasterises all seventeen.** This clamp is what makes that gate possible, so the two are the
        // same decision read from opposite ends: **the range here is the promise, and there it is
        // kept.**
        //
        // **Note what the clamp does *not* cover.** Two atlas sizes — the pair zoom needs — fit only
        // up to `--ppem 17`, and the default is now 16. **That is not this function's error to
        // refuse**, because the request is legal and the answer is *one size plus a message*, not an
        // error. `--ppem 20` builds and runs; it just has no second size to zoom to, and says so.
        // See `crate::atlas_plan::build`.
        if !(10..=26).contains(&out.ppem) {
            return Err(ParseError::OutOfRange {
                flag: "ppem".into(),
                value: u32::from(out.ppem),
                min: 10,
                max: 26,
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
    #[test]
    fn ppem_is_range_checked_before_narrowing_to_u16() {
        // 65546 as u16 == 10, which used to pass the atlas clamp.
        for value in ["65536", "65546", "4294967295"] {
            let err = Args::parse(["--container", "x", "--ppem", value]).unwrap_err();
            assert!(matches!(err, ParseError::OutOfRange { ref flag, .. } if flag == "ppem"));
        }
        assert_eq!(Args::parse(["--container", "x", "--ppem", "26"]).unwrap().ppem, 26);
    }

    #[test]
    fn width_and_height_reject_values_above_the_documented_ceiling() {
        for flag in ["--width", "--height"] {
            let err = Args::parse(["--container", "x", flag, "7681"]).unwrap_err();
            assert!(matches!(err, ParseError::OutOfRange { min: 1, max: 7680, .. }));
        }
    }

}
