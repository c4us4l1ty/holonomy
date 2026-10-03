//! Emit the geometry calibration as JSON, for the browser harness.
//!
//! # Why this exists
//!
//! The height model's constants were hand-copied into the frontend, with a test
//! asserting the two copies matched. That test worked, and the arrangement was
//! still wrong: two hand-maintained numbers that must agree, where the failure
//! mode is a silently stale comparison rather than a visible error.
//!
//! This binary makes the frontend's copy a *generated artifact* instead. Run it
//! after changing the calibration:
//!
//! ```sh
//! cargo run --release --bin emit-calibration > app/public/calibration.json
//! ```
//!
//! Staleness cannot pass silently. If this file drifts from the Rust source, the
//! frontend's height estimates drift with it, and the scroll suite's estimate
//! assertions — which bound the mean error against real rendered sections — fail.
//! That is an outcome-based check on the *consequence*, which is strictly better
//! than a check that two string literals are equal.
//!
//! In the packaged app this file is not used at all: the boot payload carries the
//! same numbers from the Rust process that owns the store.

use holonomy_core::geometry::GeometryCalibration;

fn main() {
    let cal = GeometryCalibration::default();
    // Serialized with serde_json rather than hand-rolled. The hand-rolled version
    // emitted trailing commas, which JSON does not permit, so every test that
    // fetched the file failed with `Expected double-quoted property name at
    // position 89` — 18 failures across the scroll suite from a comma.
    //
    // `GeometryCalibration` already derives Serialize, so this is the only honest
    // option: hand-writing the format for a type that has a serializer available
    // means the two can drift, and the drift shows up as a parse error at runtime
    // rather than at build time.
    let json = serde_json::to_string_pretty(&CalibrationWire::from(cal)).expect("serialisable");
    println!("{json}");
}

/// The wire shape, mirroring `GeometryCalibration` field-for-field.
///
/// A separate type rather than serializing `GeometryCalibration` directly, because
/// the Rust field names are `snake_case` with `px_per_paragraph` already — but
/// making the wire contract explicit means adding a field to the struct forces a
/// decision here rather than silently shipping it to the frontend.
#[derive(serde::Serialize)]
struct CalibrationWire {
    px_per_100_chars: f64,
    px_per_paragraph: f64,
    section_chrome_px: f64,
}

impl From<GeometryCalibration> for CalibrationWire {
    fn from(c: GeometryCalibration) -> Self {
        Self {
            px_per_100_chars: c.px_per_100_chars,
            px_per_paragraph: c.px_per_paragraph,
            section_chrome_px: c.section_chrome_px,
        }
    }
}