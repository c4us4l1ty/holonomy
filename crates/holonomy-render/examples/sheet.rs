//! Render every icon as a labelled sheet. A development aid for reviewing the artwork.
use holonomy_render::icons::IconId;

fn main() {
    const CELL_W: usize = 22;
    const CELL_H: usize = 24;
    const COLS: usize = 9;
    let n = IconId::ALL.len();
    let rows = (n + COLS - 1) / COLS;
    let (w, h) = (COLS * CELL_W, rows * CELL_H);
    let mut px = vec![0x1E1E22u32; w * h];
    for (i, id) in IconId::ALL.iter().enumerate() {
        let cx = (i % COLS) * CELL_W;
        let cy = (i / COLS) * CELL_H;
        let ic = id.at((cx + 3) as i32, (cy + 3) as i32, 0xD8D8E0);
        for y in 0..ic.height {
            for x in 0..ic.width {
                if ic.coverage(x, y) {
                    let (a, b) = (cx + x as usize + 3, cy + y as usize + 3);
                    px[b * w + a] = 0xD8D8E0;
                }
            }
        }
        for x in 0..CELL_W - 2 {
            px[(cy + CELL_H - 2) * w + cx + 1 + x] = 0x3E3E48;
        }
    }
    let mut ppm = Vec::with_capacity(w * h * 3 + 32);
    ppm.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for p in px {
        ppm.extend_from_slice(&[((p >> 16) & 0xFF) as u8, ((p >> 8) & 0xFF) as u8, (p & 0xFF) as u8]);
    }
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/sheet.ppm".into());
    std::fs::write(&out, &ppm).expect("write");
    println!("{out}: {w}x{h}  ({n} icons)");
}
