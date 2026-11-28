//! Native caption/actions; vector strokes avoid platform font glyph substitutions.
use super::{button, NativeHome};
use gpui::{canvas, div, point, prelude::*, px, rgb, Context, PathBuilder};

pub fn icon(kind: u8, ink: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let mut path = PathBuilder::stroke(px(1.));
            let mut line = |a: (f32, f32), b: (f32, f32)| {
                path.move_to(point(bounds.origin.x + px(a.0), bounds.origin.y + px(a.1)));
                path.line_to(point(bounds.origin.x + px(b.0), bounds.origin.y + px(b.1)));
            };
            match kind {
                0 => line((2., 8.), (14., 8.)),
                1 | 5 => {
                    line((3., 3.), (13., 3.));
                    line((13., 3.), (13., 13.));
                    line((13., 13.), (3., 13.));
                    line((3., 13.), (3., 3.));
                    if kind == 5 {
                        line((6., 3.), (6., 13.));
                    }
                }
                2 => {
                    line((3., 3.), (13., 13.));
                    line((13., 3.), (3., 13.));
                }
                3 => {
                    let points = [
                        (11., 6.),
                        (9.5, 2.5),
                        (6., 1.),
                        (2.5, 2.5),
                        (1., 6.),
                        (2.5, 9.5),
                        (6., 11.),
                        (9.5, 9.5),
                        (11., 6.),
                    ];
                    for pair in points.windows(2) {
                        line(pair[0], pair[1]);
                    }
                    line((10., 10.), (15., 15.));
                }
                4 => {
                    for (x, y) in [(2., 2.), (10., 2.), (2., 10.), (10., 10.)] {
                        line((x, y), (x + 4., y));
                        line((x + 4., y), (x + 4., y + 4.));
                        line((x + 4., y + 4.), (x, y + 4.));
                        line((x, y + 4.), (x, y));
                    }
                }
                _ => {
                    for x in [3., 8., 13.] {
                        line((x, 7.), (x, 9.));
                    }
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(ink));
            }
        },
    )
    .size(px(16.))
}

pub fn controls(ink: u32, cx: &mut Context<NativeHome>) -> gpui::Div {
    let mut controls = div().flex().items_center().gap_1().flex_shrink_0();
    for (id, kind) in [
        ("window-minimize", 0),
        ("window-maximize", 1),
        ("window-close", 2),
    ] {
        controls = controls.child(button(id, "").w(px(32.)).child(icon(kind, ink)).on_click(
            cx.listener(move |owner, _, window, cx| {
                match kind {
                    0 => window.minimize_window(),
                    1 => window.zoom_window(),
                    _ => {
                        if owner.save() {
                            window.remove_window();
                        }
                    }
                }
                cx.stop_propagation();
                cx.notify();
            }),
        ));
    }
    controls
}
