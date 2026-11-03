//! MIT-licensed WordCraft UI code, adapted to Holonomy's native surface tree.
//!
//! Upstream: https://github.com/storytold/wordcraft
//! Revision: ae5c65b9dd11faf359ce039a2a487e57e3d824a2
//! Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
//!
//! These modules retain the upstream algorithms and design data, but replace egui's
//! Context/Painter/Color32 with Holonomy's integer geometry and atlas labels. There is
//! deliberately no egui, GPU, windowing or document-engine dependency here. The original
//! source snapshots and full MIT notice are in `third_party/wordcraft`.

pub mod gallery;
pub mod ribbon_fit;
pub mod tokens;
