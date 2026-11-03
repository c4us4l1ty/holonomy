//! Reading an `.Xauthority` file, which is the only credential this crate uses.
//!
//! # The file format is not a text format
//!
//! An Xauthority file is a sequence of big-endian, length-prefixed binary records:
//!
//! ```text
//!   uint16 family
//!   uint16 len, bytes     address   (a hostname for FamilyWild, else unused)
//!   uint16 len, bytes     number    (a display number, as ASCII)
//!   uint16 len, bytes     name      ("MIT-MAGIC-COOKIE-1")
//!   uint16 len, bytes     data      (16 bytes of cookie)
//! ```
//!
//! Both of the ways to get this wrong are quiet. Parsing it as text finds no records at all; parsing
//! the lengths as little-endian finds a record with a plausible name and a 21-byte cookie, and the
//! server then refuses the connection with a reason of `Invalid` -- which is exactly what this crate's
//! first attempt produced.
//!
//! # Which record to use
//!
//! `FamilyLocal` (256) with an empty number matches a local display; `FamilyWild` (65535) matches any
//! display. This crate takes the first `MIT-MAGIC-COOKIE-1` record whose display number is empty or
//! equal to the display being opened, preferring `FamilyLocal`. If the file has neither, the
//! connection is reported as unauthenticated rather than being attempted without a cookie, because on
//! this host that produces a confusing refusal instead of a clear one.

use std::fmt;
use std::path::Path;

/// Address family for a local connection: the record's address and number are both empty.
pub const FAMILY_LOCAL: u16 = 256;
/// Address family matching any host and any display.
pub const FAMILY_WILD: u16 = 65535;
/// The one authorisation scheme this crate speaks.
pub const MIT_MAGIC_COOKIE_1: &str = "MIT-MAGIC-COOKIE-1";
/// A cookie is 16 bytes on every implementation that uses `MIT-MAGIC-COOKIE-1`.
pub const COOKIE_BYTES: usize = 16;

/// Why no cookie could be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The file could not be read.
    Unreadable {
        /// The path that was tried.
        path: String,
        /// The `errno` it failed with.
        errno: i32,
    },
    /// The file was read but is not a sequence of records this crate can parse.
    Malformed {
        /// Where in the file the parse stopped.
        at: usize,
    },
    /// The file parsed but holds no cookie for this display.
    NoCookie {
        /// The display that was wanted.
        display: u32,
        /// How many records were there, and what they were called.
        records: Vec<String>,
    },
    /// A cookie was found but it is not 16 bytes, which no `MIT-MAGIC-COOKIE-1` ever is.
    WrongLength {
        /// How long the record's data was.
        got: usize,
    },
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, errno } => write!(
                f,
                "no Xauthority at {path} (errno {errno}) -- is DISPLAY set to a display this user may open?"
            ),
            Self::Malformed { at } => {
                write!(f, "the Xauthority file is malformed at byte {at}")
            }
            Self::NoCookie { display, records } => write!(
                f,
                "the Xauthority file has {} record(s) {records:?} and none of them is a cookie for display {display}",
                records.len()
            ),
            Self::WrongLength { got } => write!(
                f,
                "the cookie is {got} bytes; MIT-MAGIC-COOKIE-1 is {COOKIE_BYTES}"
            ),
        }
    }
}

impl std::error::Error for AuthError {}

/// One authorisation record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    /// The address family.
    pub family: u16,
    /// The display number as text, empty for a family-local record.
    pub number: String,
    /// The scheme name; always `MIT-MAGIC-COOKIE-1` in practice.
    pub name: String,
    /// The cookie bytes.
    pub data: Vec<u8>,
}

/// Every record in an Xauthority file, in order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Cookie>, AuthError> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let family = be_u16(bytes, &mut at)?;
        let _address = be_bytes(bytes, &mut at)?;
        let number = be_bytes(bytes, &mut at)?;
        let name = be_bytes(bytes, &mut at)?;
        let data = be_bytes(bytes, &mut at)?;
        out.push(Cookie {
            family,
            number: String::from_utf8_lossy(&number).into_owned(),
            name: String::from_utf8_lossy(&name).into_owned(),
            data,
        });
    }
    Ok(out)
}

/// Read `path` and take the cookie for `display`.
pub fn cookie_for(path: &Path, display: u32) -> Result<Vec<u8>, AuthError> {
    let bytes = std::fs::read(path).map_err(|e| AuthError::Unreadable {
        path: path.display().to_string(),
        errno: e.raw_os_error().unwrap_or(libc::EIO),
    })?;
    let records = parse(&bytes)?;
    let wanted = display.to_string();
    let mut names = Vec::with_capacity(records.len());
    for r in &records {
        names.push(format!("{}/{}", r.name, r.number));
    }
    // `FamilyLocal` first, then anything whose display number matches, then a wildcard. The order is
    // the file's for equal-family matches, because two records for one display is a broken file and
    // the first is as good a guess as any.
    let pick = records
        .iter()
        .filter(|r| r.name == MIT_MAGIC_COOKIE_1)
        .find(|r| r.family == FAMILY_LOCAL && r.number.is_empty())
        .or_else(|| {
            records
                .iter()
                .find(|r| r.name == MIT_MAGIC_COOKIE_1 && r.number == wanted)
        })
        .or_else(|| {
            records
                .iter()
                .find(|r| r.name == MIT_MAGIC_COOKIE_1 && r.family == FAMILY_WILD)
        });
    let Some(pick) = pick else {
        return Err(AuthError::NoCookie {
            display,
            records: names,
        });
    };
    if pick.data.len() != COOKIE_BYTES {
        return Err(AuthError::WrongLength {
            got: pick.data.len(),
        });
    }
    Ok(pick.data.clone())
}

fn be_u16(bytes: &[u8], at: &mut usize) -> Result<u16, AuthError> {
    let end = at
        .checked_add(2)
        .filter(|e| *e <= bytes.len())
        .ok_or(AuthError::Malformed { at: *at })?;
    let v = u16::from_be_bytes([bytes[*at], bytes[end - 1]]);
    *at = end;
    Ok(v)
}

fn be_bytes(bytes: &[u8], at: &mut usize) -> Result<Vec<u8>, AuthError> {
    let len = be_u16(bytes, at)? as usize;
    let end = at
        .checked_add(len)
        .filter(|e| *e <= bytes.len())
        .ok_or(AuthError::Malformed { at: *at })?;
    let v = bytes[*at..end].to_vec();
    *at = end;
    Ok(v)
}

/// Where the cookie lives when `$XAUTHORITY` is not set, per the Xlib convention.
pub fn default_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("XAUTHORITY") {
        if !p.is_empty() {
            return Some(p.into());
        }
    }
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".Xauthority"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a record the way Xlib writes one. Used by every test below, because the point of those
    /// tests is the *parse*, not the fixture.
    fn record(family: u16, address: &[u8], number: &[u8], name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&family.to_be_bytes());
        for field in [address, number, name, data] {
            out.extend_from_slice(&(field.len() as u16).to_be_bytes());
            out.extend_from_slice(field);
        }
        out
    }

    const COOKIE: [u8; 16] = [
        0x7e, 0xaf, 0x7f, 0xd5, 0xa7, 0xc5, 0xe7, 0xaa, 0x33, 0x59, 0x28, 0x57, 0xdd, 0x52, 0xfa,
        0x28,
    ];

    /// The real file on this host, hexdumped, parses. Recorded here so a future change to the parser
    /// cannot quietly stop reading the actual format:
    ///
    /// ```text
    /// 01 00 | 00 01 63 | 00 00 | 00 12 "MIT-MAGIC-COOKIE-1" | 00 10 7eaf7f...
    /// ^fam  ^"c" (localhost, short form)          ^empty number
    /// 01 00 -> 0x0100 = 256 = FamilyLocal
    /// ```
    #[test]
    fn the_format_this_host_writes_parses() {
        let mut bytes = record(FAMILY_LOCAL, b"c", b"", b"MIT-MAGIC-COOKIE-1", &COOKIE);
        bytes.extend_from_slice(&record(
            FAMILY_WILD,
            b"c",
            b"",
            b"MIT-MAGIC-COOKIE-1",
            &COOKIE,
        ));
        let got = parse(&bytes).expect("parse");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].family, FAMILY_LOCAL);
        assert_eq!(got[0].number, "");
        assert_eq!(got[0].name, MIT_MAGIC_COOKIE_1);
        assert_eq!(got[0].data, COOKIE.to_vec());
        assert_eq!(got[1].family, FAMILY_WILD);
    }

    /// Every integer in the file is big-endian. Reading them little-endian gives a name that decodes
    /// to nothing and a cookie that the server rejects with reason `Invalid` -- which is what this
    /// crate's first probe did.
    #[test]
    fn lengths_are_read_big_endian() {
        let bytes = record(FAMILY_LOCAL, b"c", b"", b"MIT-MAGIC-COOKIE-1", &COOKIE);
        // family[0..2], address-length[2..4], "c"[4..5], number-length[5..7] = 0,
        // name-length[7..9] = 18, name[9..27], data-length[27..29] = 16, data[29..45].
        assert_eq!(&bytes[..2], [0x01u8, 0x00], "family 256 big-endian");
        assert_eq!(&bytes[2..4], [0x00u8, 0x01], "address length 1 big-endian");
        assert_eq!(&bytes[4..5], b"c", "the address, as Xlib writes localhost");
        assert_eq!(
            &bytes[5..7],
            [0x00u8, 0x00],
            "the empty number is a zero length"
        );
        assert_eq!(&bytes[7..9], [0x00u8, 0x12], "name length 18 big-endian");
        assert_eq!(&bytes[9..27], b"MIT-MAGIC-COOKIE-1");
        assert_eq!(&bytes[27..29], [0x00u8, 0x10], "data length 16 big-endian");
        assert_eq!(bytes.len(), 45);
    }

    /// A record whose length runs off the end of the file is refused at the byte where it ran out,
    /// rather than being read as whatever follows.
    #[test]
    fn a_truncated_record_is_refused_with_the_offset() {
        let mut bytes = record(FAMILY_LOCAL, b"c", b"", b"MIT-MAGIC-COOKIE-1", &COOKIE);
        bytes.truncate(bytes.len() - 4);
        // The cookie starts at 29 and needs 16 bytes; only 12 are left, so the parse stops there.
        assert_eq!(parse(&bytes), Err(AuthError::Malformed { at: 29 }));
    }

    /// A cookie for display 1 is not a cookie for display 0. Getting this wrong fails only when a
    /// user has a second display, which is the worst time to find it.
    #[test]
    fn a_cookie_is_matched_against_the_display_being_opened() {
        let mut bytes = record(FAMILY_LOCAL, b"c", b"1", b"MIT-MAGIC-COOKIE-1", &[7u8; 16]);
        bytes.extend_from_slice(&record(
            FAMILY_LOCAL,
            b"c",
            b"0",
            b"MIT-MAGIC-COOKIE-1",
            &COOKIE,
        ));
        let path = write_temp("match", &bytes);
        assert_eq!(cookie_for(&path, 0).expect("display 0"), COOKIE.to_vec());
        assert_eq!(cookie_for(&path, 1).expect("display 1"), vec![7u8; 16]);
        std::fs::remove_file(&path).ok();
    }

    /// A record naming a scheme this crate cannot speak is not silently used as a cookie.
    #[test]
    fn a_file_without_a_usable_cookie_says_so() {
        let bytes = record(FAMILY_LOCAL, b"c", b"", b"XDM-AUTHORIZATION-1", &[0u8; 16]);
        let path = write_temp("noscheme", &bytes);
        let err = cookie_for(&path, 0).expect_err("no cookie");
        match err {
            AuthError::NoCookie { display, records } => {
                assert_eq!(display, 0);
                assert_eq!(records, vec!["XDM-AUTHORIZATION-1/".to_string()]);
            }
            other => panic!("expected NoCookie, got {other:?}"),
        }
        std::fs::remove_file(&path).ok();
    }

    /// A missing file names the path and the errno, because "no cookie" with no path is the least
    /// useful error message available for a user who typed the command wrong.
    #[test]
    fn a_missing_file_names_its_path() {
        let err = cookie_for(Path::new("/nonexistent/Xauthority"), 0).expect_err("missing");
        match err {
            AuthError::Unreadable { ref path, errno } => {
                assert_eq!(path, "/nonexistent/Xauthority");
                assert_eq!(errno, libc::ENOENT);
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
        assert!(format!("{err}").contains("/nonexistent/Xauthority"));
    }

    /// One file per test *and* per call: libtest runs the tests in this module on the same thread in
    /// one process, so a pid-only name had two tests overwriting each other's fixture -- which is how
    /// `a_cookie_is_matched_against_the_display_being_opened` first failed with an empty file.
    fn write_temp(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "holonomy-xauthority-{}-{}-{n}.auth",
            std::process::id(),
            tag
        ));
        std::fs::write(&path, bytes).expect("write the fixture");
        path
    }
}
