//! Native caption actions; icons are vector strokes, independent of font glyph coverage.
use super::{button, NativeHome};
use gpui::{canvas, div, point, prelude::*, px, rgb, Context, PathBuilder};

pub fn controls(ink: u32, cx: &mut Context<NativeHome>) -> gpui::Div {
    let mut controls = div().flex().items_center().gap_1().flex_shrink_0();
    for (id, kind) in [("window-minimize", 0), ("window-maximize", 1), ("window-close", 2)] {
        controls = controls.child(button(id, "").w(px(32.)).child(canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let mut path = PathBuilder::stroke(px(1.));
                let mut line = |a: (f32, f32), b: (f32, f32)| {
                    path.move_to(point(bounds.origin.x + px(a.0), bounds.origin.y + px(a.1)));
                    path.line_to(point(bounds.origin.x + px(b.0), bounds.origin.y + px(b.1)));
                };
                match kind {
                    0 => line((2., 8.), (14., 8.)),
                    1 => {
                        line((3., 3.), (13., 3.)); line((13., 3.), (13., 13.));
                        line((13., 13.), (3., 13.)); line((3., 13.), (3., 3.));
                    }
                    _ => { line((3., 3.), (13., 13.)); line((13., 3.), (3., 13.)); }
                }
                if let Ok(path) = path.build() { window.paint_path(path, rgb(ink)); }
            },
        ).size(px(16.))).on_click(cx.listener(move |owner, _, window, cx| {
            match kind {
                0 => window.minimize_window(),
                1 => window.zoom_window(),
                _ => if owner.save() { window.remove_window(); },
            }
            cx.stop_propagation();
            cx.notify();
        })));
    }
    controls
}
