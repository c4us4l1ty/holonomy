//! Opt-in native-scale UI shell. Document editing stays in the existing application for now.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use gpui::{div, prelude::*, px, rgb, size, App, Application, Bounds, Context, Window, WindowBounds, WindowOptions};

#[derive(Default)]
struct NativeHome {
    dark: bool,
    specimen: bool,
    status: String,
}

impl NativeHome {
    fn open_editor(&mut self) {
        // A separate process deliberately keeps plaintext and passphrases out of GPUI until
        // framework text-cache lifetimes have been reviewed against the wipe contract.
        let result = std::env::current_exe().and_then(|path| {
            let name = if cfg!(windows) { "holonomy-gui.exe" } else { "holonomy-gui" };
            std::process::Command::new(path.with_file_name(name)).spawn()
        });
        match result {
            Ok(mut child) => {
                std::thread::spawn(move || { let _ = child.wait(); });
                self.status = "Existing editor launched in a separate window. Its rendering is unchanged.".into();
            }
            Err(error) => {
                self.status = format!("Could not launch the existing editor: {error}. Build holonomy-gui beside this binary.");
            }
        }
    }
}

impl Render for NativeHome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (bg, panel, ink, muted, line) = if self.dark {
            (0x191b1e, 0x222529, 0xeeeeec, 0xa8acae, 0x41464b)
        } else {
            (0xfaf9f6, 0xffffff, 0x242628, 0x666b70, 0xd9dcde)
        };
        let scale = window.scale_factor();
        // This is a physical-pixel rule, not one logical pixel later stretched to the screen.
        let hairline = px(1.0 / scale);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(bg))
            .text_color(rgb(ink))
            .text_size(px(15.))
            .child(
                div().flex().items_center().justify_between().p_4()
                    .border_b_1().border_color(rgb(line))
                    .child("Holonomy")
                    .child(div().flex().gap_4()
                        .child(div().id("specimen").cursor_pointer().child("Rendering specimen")
                            .on_click(cx.listener(|this, _, _, cx| { this.specimen = !this.specimen; cx.notify(); })))
                        .child(div().id("theme").cursor_pointer().child("Toggle theme")
                            .on_click(cx.listener(|this, _, _, cx| { this.dark = !this.dark; cx.notify(); }))))
            )
            .child(
                div().flex_1().flex().flex_col().p_8().gap_4()
                    .child(div().text_size(px(30.)).child(if self.specimen { "Native-resolution specimen" } else { "A quiet place to write" }))
                    .child(div().text_color(rgb(muted)).child(format!("GPUI preview · device scale {scale:.2}× · no whole-window bitmap enlargement")))
                    .child(div().h(hairline).w_full().bg(rgb(line)))
                    .child(
                        div().p_6().bg(rgb(panel)).border_1().border_color(rgb(line)).rounded_lg()
                            .flex().flex_col().gap_4()
                            .child(if self.specimen { "Hamburgefontsiv 0123456789 — Il1 O0 rn m" } else { "Your encrypted documents remain in the existing editor." })
                            .child(div().text_size(px(12.)).child("12 px: The quick brown fox jumps over the lazy dog."))
                            .child(div().text_size(px(16.)).child("16 px: The quick brown fox jumps over the lazy dog."))
                            .child(div().text_size(px(24.)).child("24 px: Independently rasterized text"))
                            .child(div().flex().gap_4().items_center()
                                .child(div().w(px(24.)).h(hairline).bg(rgb(ink)))
                                .child(div().w(hairline).h(px(24.)).bg(rgb(ink)))
                                .child("One-device-pixel horizontal and vertical rules"))
                    )
                    .child(div().id("open-editor").cursor_pointer().px_4().py_2().rounded_md()
                        .bg(rgb(0x365d50)).text_color(rgb(0xffffff))
                        .child("Open existing editor (separate window)")
                        .on_click(cx.listener(|this, _, _, cx| { this.open_editor(); cx.notify(); })))
                    .child(div().text_color(rgb(muted)).child(self.status.clone()))
                    .child(div().text_size(px(12.)).text_color(rgb(muted))
                        .child("Preview only: encrypted unlock, editing and wipe qualification have not been ported. No document content is loaded here."))
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| NativeHome::default()),
        ).expect("could not open the native UI preview");
        cx.activate(true);
    });
}
