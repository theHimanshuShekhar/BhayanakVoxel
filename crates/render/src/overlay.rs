//! The debug overlay: an egui panel with the GPU time of every pass, the render settings, and a
//! switch for the debug views. This only lays the panel out; the window integration (input,
//! painting) is in the client and in [`Renderer::render`](crate::Renderer::render).

use egui::{Align2, Context, Grid, RichText, Slider, Window};

use crate::debug_view::DebugView;
use crate::timing::{Series, Timings};
use crate::window::Mode;

/// The render settings the overlay edits. Whoever owns the real state fills this in before
/// [`show`] and applies what changed after it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
    pub mode: Mode,
    pub debug_view: DebugView,
    /// The sun's azimuth and elevation in radians (see [`Sun::from_angles`](crate::Sun)).
    pub sun: (f32, f32),
}

/// What the overlay reports and does not edit.
pub struct Info<'a> {
    pub timings: &'a Timings,
    /// Whether the device can time passes (see [`Profiler::supported`](crate::Profiler)).
    pub timers_supported: bool,
    /// Samples in the reference image so far.
    pub samples: u32,
}

/// A time for the panel: milliseconds, or "n/a" where there is none.
fn ms(time: Option<f64>) -> String {
    time.map_or_else(|| "n/a".to_string(), |t| format!("{t:.3}"))
}

/// Frames per second for a series of frame times in milliseconds.
fn fps(frame_times: &Series) -> String {
    frame_times
        .average()
        .filter(|&t| t > 0.0)
        .map_or_else(|| "n/a".to_string(), |t| format!("{:.0}", 1e3 / t))
}

fn timer_row(ui: &mut egui::Ui, name: &str, series: &Series, active: bool) {
    let label = RichText::new(name).monospace();
    ui.label(if active { label } else { label.weak() });
    ui.monospace(ms(series.average()));
    ui.monospace(ms(series.max()));
    ui.end_row();
}

/// Lays out the panel on `ctx`, and writes the user's changes to the settings in `controls`.
pub fn show(ctx: &Context, info: &Info, controls: &mut Controls) {
    Window::new("Debug")
        .anchor(Align2::LEFT_TOP, [8.0, 8.0])
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            if !info.timers_supported {
                ui.label("GPU timers: not supported by this device");
            }
            Grid::new("pass timers").num_columns(3).show(ui, |ui| {
                ui.label("");
                ui.label("avg ms");
                ui.label("max ms");
                ui.end_row();
                for pass in info.timings.passes() {
                    timer_row(ui, &pass.name, &pass.times, pass.active);
                }
                timer_row(ui, "GPU total", info.timings.total(), true);
                timer_row(ui, "frame (CPU)", info.timings.wall(), true);
                ui.end_row();
            });
            ui.label(format!("fps: {}", fps(info.timings.wall())));
            ui.separator();

            let mut reference = controls.mode == Mode::Reference;
            if ui.checkbox(&mut reference, "Reference mode (R)").changed() {
                controls.mode = if reference {
                    Mode::Reference
                } else {
                    Mode::Primary
                };
            }
            if controls.mode == Mode::Reference {
                ui.label(format!("samples: {}", info.samples));
            }

            egui::ComboBox::from_label("View (V)")
                .selected_text(controls.debug_view.name())
                .show_ui(ui, |ui| {
                    for view in DebugView::ALL {
                        ui.selectable_value(&mut controls.debug_view, view, view.name());
                    }
                });

            // Edited in degrees; the stored radians are only touched when the user drags, so
            // that converting back and forth cannot nudge the sun (which restarts the
            // reference image) every frame.
            let (azimuth, elevation) = controls.sun;
            let mut degrees = [azimuth.to_degrees(), elevation.to_degrees()];
            if ui
                .add(Slider::new(&mut degrees[0], -180.0..=180.0).text("sun azimuth (deg)"))
                .changed()
            {
                controls.sun.0 = degrees[0].to_radians();
            }
            if ui
                .add(Slider::new(&mut degrees[1], -90.0..=90.0).text("sun elevation (deg)"))
                .changed()
            {
                controls.sun.1 = degrees[1].to_radians();
            }
            ui.weak("arrows: sun, V: view, F1: hide, Esc: free the mouse");
        });
}

#[cfg(test)]
mod tests {
    use egui::{RawInput, Shape};

    use super::*;
    use crate::timing::FrameTimes;

    /// All the text the context drew, in one string.
    fn text_of(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn collect(shape: &Shape, out: &mut String) {
            match shape {
                Shape::Text(text) => {
                    out.push_str(text.galley.text());
                    out.push('\n');
                }
                Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        let mut out = String::new();
        for clipped in shapes {
            collect(&clipped.shape, &mut out);
        }
        out
    }

    fn controls() -> Controls {
        Controls {
            mode: Mode::Primary,
            debug_view: DebugView::Lit,
            sun: (0.5, 0.8),
        }
    }

    fn draw(info: &Info, controls: &mut Controls) -> String {
        let ctx = Context::default();
        // A window is laid out invisibly on its first frame, to learn its size.
        for _ in 0..2 {
            ctx.run_pass(RawInput::default(), |ctx| show(ctx, info, controls))
                .drop_without_applying_deltas();
        }
        let output = ctx.run_pass(RawInput::default(), |ctx| show(ctx, info, controls));
        let text = text_of(&output.shapes);
        output.drop_without_applying_deltas();
        text
    }

    #[test]
    fn the_overlay_has_a_font_to_draw_with() {
        // Without egui's `default_fonts` feature every glyph renders as a box.
        assert!(!egui::FontDefinitions::default().font_data.is_empty());
    }

    #[test]
    fn times_without_data_read_na() {
        assert_eq!(ms(None), "n/a");
        assert_eq!(ms(Some(1.23456)), "1.235");
        assert_eq!(fps(Timings::default().wall()), "n/a");
    }

    #[test]
    fn the_panel_lists_every_pass_with_its_time_and_the_totals() {
        let mut timings = Timings::default();
        let primary = timings.register("primary");
        let blit = timings.register("blit");
        timings.register("never ran");
        timings.record_frame(&FrameTimes {
            passes: vec![(primary, 2.5), (blit, 0.25)],
            total: Some(3.0),
        });
        let info = Info {
            timings: &timings,
            timers_supported: true,
            samples: 7,
        };
        let text = draw(&info, &mut controls());
        for expected in [
            "primary",
            "2.500",
            "blit",
            "0.250",
            "never ran",
            "n/a",
            "GPU total",
            "3.000",
            "frame (CPU)",
        ] {
            assert!(text.contains(expected), "no {expected:?} in:\n{text}");
        }
        assert!(!text.contains("not supported"));
        // Not in reference mode, so no sample count yet.
        assert!(!text.contains("samples: 7"));
    }

    #[test]
    fn unsupported_timers_say_so_and_every_time_is_na() {
        let mut timings = Timings::default();
        timings.register("primary");
        let info = Info {
            timings: &timings,
            timers_supported: false,
            samples: 0,
        };
        let text = draw(&info, &mut controls());
        assert!(text.contains("GPU timers: not supported"), "{text}");
        assert!(text.contains("primary") && text.contains("n/a"), "{text}");
    }

    #[test]
    fn reference_mode_shows_the_sample_count_and_the_settings_stay_as_they_were() {
        let timings = Timings::default();
        let info = Info {
            timings: &timings,
            timers_supported: true,
            samples: 42,
        };
        let mut controls = Controls {
            mode: Mode::Reference,
            debug_view: DebugView::Depth,
            sun: (0.5, 0.8),
        };
        let before = controls;
        let text = draw(&info, &mut controls);
        assert!(text.contains("samples: 42"), "{text}");
        assert!(text.contains("Depth"), "{text}");
        // Showing the panel is not an edit.
        assert_eq!(controls, before);
    }
}
