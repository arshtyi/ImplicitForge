use implicit_forge::resample;
use implicit_forge::scene::{DEMO_COUNT, Stamp, StampOp, StampShape};

use crate::text::{TextRenderer, TextSurface};
use crate::ui_worker::{Backend, Image};
use winit::window::ResizeDirection;

const INK: u32 = 0x172123;
const PAPER: u32 = 0xf2efe7;
const PANEL: u32 = 0xe3e3d8;
const TEAL: u32 = 0x14695e;
const MINT: u32 = 0x91d5b9;
const MUTED: u32 = 0x536263;
const RULE: u32 = 0xc0c8be;
const PANEL_W: usize = 256;
const INSET: usize = 16;
const TRACK_W: usize = PANEL_W - 2 * INSET;
const HALF_W: usize = (TRACK_W - 4) / 2;
const CAPTION: usize = 15;
const BODY: usize = 18;

#[derive(Clone, Copy)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    scale: f64,
    pub view_width: usize,
    top: usize,
    bottom: usize,
}

impl Layout {
    pub fn new(w: u32, h: u32, dpi: f64) -> Self {
        let width = w as usize;
        let height = h as usize;
        let scale = dpi
            .clamp(0.5, 8.)
            .min(width.max(1) as f64 / 760.)
            .min(height.max(1) as f64 / 492.);
        let view_width = width.saturating_sub((PANEL_W as f64 * scale).round() as usize);
        Self {
            width,
            height,
            scale,
            view_width,
            top: ((40. * scale).round() as usize).min(height),
            bottom: height.saturating_sub((32. * scale).round() as usize),
        }
    }

    fn px(self, logical: usize) -> usize {
        (logical as f64 * self.scale).round() as usize
    }

    pub fn target(self) -> [u32; 2] {
        let w = self.view_width.max(1) as f64;
        let h = self.bottom.saturating_sub(self.top).max(1) as f64;
        let shrink = (1440. / w).min(1080. / h).min(1.);
        [w, h].map(|v| (v * shrink).round().max(1.) as u32)
    }

    pub fn hit(self, x: f64, y: f64, editor: Editor, own_frame: bool) -> Option<Hit> {
        if !(0.0..self.width as f64).contains(&x) || !(0.0..self.height as f64).contains(&y) {
            return None;
        }
        if own_frame {
            let edge = self.px(4) as f64;
            let west = x < edge;
            let east = x >= self.width as f64 - edge;
            let north = y < edge;
            let south = y >= self.height as f64 - edge;
            let direction = match (west, east, north, south) {
                (true, _, true, _) => Some(ResizeDirection::NorthWest),
                (_, true, true, _) => Some(ResizeDirection::NorthEast),
                (true, _, _, true) => Some(ResizeDirection::SouthWest),
                (_, true, _, true) => Some(ResizeDirection::SouthEast),
                (true, _, _, _) => Some(ResizeDirection::West),
                (_, true, _, _) => Some(ResizeDirection::East),
                (_, _, true, _) => Some(ResizeDirection::North),
                (_, _, _, true) => Some(ResizeDirection::South),
                _ => None,
            };
            if let Some(direction) = direction {
                return Some(Hit::Resize(direction));
            }
        }
        if x < self.view_width as f64 {
            if own_frame && y < self.top as f64 {
                let from_right = self.view_width - x as usize;
                return Some(if from_right <= self.px(36) {
                    Hit::Close
                } else if from_right <= self.px(72) {
                    Hit::Maximize
                } else if from_right <= self.px(108) {
                    Hit::Minimize
                } else {
                    Hit::Title
                });
            }
            return (y >= self.top as f64 && y < self.bottom as f64).then_some(Hit::View);
        }
        let x = x as usize - self.view_width;
        let y = y as usize;
        let contains = |control: Hit| {
            let [cx, cy, w, h] = control.bounds(editor);
            (self.px(cx)..self.px(cx + w)).contains(&x)
                && (self.px(cy)..self.px(cy + h)).contains(&y)
        };
        let fixed = [
            Hit::Cpu,
            Hit::Gpu,
            Hit::Mode,
            Hit::Editor(Editor::Scene),
            Hit::Editor(Editor::Tool),
        ];
        fixed
            .into_iter()
            .chain(
                match editor {
                    Editor::Scene => &[][..],
                    Editor::Tool => &[
                        Hit::EnableStamp,
                        Hit::Shape,
                        Hit::Operation,
                        Hit::Apply,
                        Hit::Undo,
                    ][..],
                }
                .iter()
                .copied(),
            )
            .chain(
                (0..if editor == Editor::Scene {
                    DEMO_COUNT
                } else {
                    0
                })
                    .map(Hit::Scene),
            )
            .chain((0..editor.sliders()).map(Hit::Slider))
            .find(|&hit| contains(hit))
    }

    pub fn knob(self, x: f64) -> f32 {
        let start = self.view_width + self.px(INSET);
        let width = self.px(INSET + TRACK_W) - self.px(INSET);
        ((x - start as f64) / width.max(1) as f64).clamp(0., 1.) as f32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Editor {
    Scene,
    Tool,
}

impl Editor {
    fn sliders(self) -> usize {
        match self {
            Self::Scene => 3,
            Self::Tool => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    View,
    Scene(usize),
    Title,
    Minimize,
    Maximize,
    Close,
    Resize(ResizeDirection),
    Editor(Editor),
    Slider(usize),
    Cpu,
    Gpu,
    Mode,
    EnableStamp,
    Shape,
    Operation,
    Apply,
    Undo,
}

impl Hit {
    // Panel-local logical pixels. Paint and pointer dispatch use the same edges.
    fn bounds(self, editor: Editor) -> [usize; 4] {
        let left = |y, h| [INSET, y, HALF_W, h];
        let right = |y, h| [INSET + HALF_W + 4, y, HALF_W, h];
        let row = |y, h| [INSET, y, TRACK_W, h];
        match self {
            Self::Cpu => left(12, 28),
            Self::Gpu => right(12, 28),
            Self::Mode => row(44, 24),
            Self::Editor(Editor::Scene) => left(106, 26),
            Self::Editor(Editor::Tool) => right(106, 26),
            Self::Scene(i) => row(139 + i * 22, 21),
            Self::EnableStamp => row(162, 26),
            Self::Shape => left(194, 26),
            Self::Operation => right(194, 26),
            Self::Slider(i) => row(
                match editor {
                    Editor::Scene => 351 + i * 38,
                    Editor::Tool => 249 + i * 38,
                },
                34,
            ),
            Self::Apply => left(410, 26),
            Self::Undo => right(410, 26),
            Self::View
            | Self::Title
            | Self::Minimize
            | Self::Maximize
            | Self::Close
            | Self::Resize(_) => unreachable!("not a panel control"),
        }
    }
}

pub struct Panel<'a> {
    pub names: [&'a str; DEMO_COUNT],
    pub selected: usize,
    pub parameter_names: [&'a str; 3],
    pub values: [f32; 3],
    pub editor: Editor,
    pub active_slider: usize,
    pub stamp: Stamp,
    pub stamp_enabled: bool,
    pub applied: usize,
    pub can_apply: bool,
    pub backend: Backend,
    pub adapter: &'a str,
    pub gpu_requested: bool,
    pub own_frame: bool,
    pub gpu_allowed: bool,
    pub image: Option<&'a Image>,
}

struct Canvas<'a> {
    pixels: &'a mut [u32],
    layout: Layout,
    font: &'a mut TextRenderer,
}

impl Canvas<'_> {
    // Coordinates are physical pixels. Clip spans before indexing, including on
    // compositors that temporarily give us a window smaller than its minimum.
    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let x1 = (x + w).min(self.layout.width);
        if x >= x1 {
            return;
        }
        for row in y..(y + h).min(self.layout.height) {
            let start = row * self.layout.width;
            self.pixels[start + x..start + x1].fill(color);
        }
    }

    fn label(&mut self, origin: [usize; 2], right: usize, size: usize, color: u32, value: &str) {
        let l = self.layout;
        let surface = TextSurface {
            pixels: self.pixels,
            stride: l.width,
            clip: [right.min(l.width), l.height],
        };
        self.font
            .draw(surface, origin, size as f32 * l.scale as f32, color, value);
    }

    fn panel_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let l = self.layout;
        self.rect(
            l.view_width + l.px(x),
            l.px(y),
            l.px(x + w) - l.px(x),
            l.px(y + h) - l.px(y),
            color,
        );
    }

    fn panel_text(&mut self, x: usize, y: usize, size: usize, color: u32, value: &str) {
        let l = self.layout;
        self.label(
            [l.view_width + l.px(x), l.px(y)],
            l.width.saturating_sub(l.px(8)),
            size,
            color,
            value,
        );
    }

    fn button(&mut self, hit: Hit, editor: Editor, text: &str, active: bool, enabled: bool) {
        let [x, y, w, h] = hit.bounds(editor);
        let (bg, fg) = match (enabled, active) {
            (false, _) => (RULE, MUTED),
            (true, true) => (TEAL, PAPER),
            (true, false) => (PAPER, INK),
        };
        self.panel_rect(x, y, w, h, bg);
        let l = self.layout;
        self.label(
            [l.view_width + l.px(x + 7), l.px(y + (h - 17) / 2)],
            l.view_width + l.px(x + w - 3),
            17,
            fg,
            text,
        );
    }

    fn slider(
        &mut self,
        editor: Editor,
        index: usize,
        name: &str,
        knob: f32,
        value: &str,
        active: bool,
    ) {
        let [x, y, w, _] = Hit::Slider(index).bounds(editor);
        let l = self.layout;
        let right = l.view_width + l.px(x + w);
        let value_w = self.font.width(value, BODY as f32 * l.scale as f32).ceil() as usize;
        let value_x = right.saturating_sub(value_w);
        self.label(
            [l.view_width + l.px(x), l.px(y)],
            value_x.saturating_sub(l.px(5)),
            BODY,
            INK,
            name,
        );
        self.label([value_x, l.px(y)], right, BODY, TEAL, value);
        let filled = (w as f32 * knob.clamp(0., 1.)).round() as usize;
        self.panel_rect(x, y + 24, w, 3, RULE);
        self.panel_rect(x, y + 24, filled, 3, if active { TEAL } else { MUTED });
        self.panel_rect(
            x + filled - 3,
            y + 20,
            6,
            11,
            if active { TEAL } else { INK },
        );
    }

    fn scene_image(&mut self, image: &Image) {
        let l = self.layout;
        let height = l.bottom.saturating_sub(l.top);
        if height > 0 && l.view_width > 0 {
            resample::blit(
                &image.pixels,
                image.size.map(|v| v as usize),
                &mut self.pixels[l.top * l.width..],
                [l.view_width, height],
                l.width,
            );
        }
    }
}

pub fn paint(pixels: &mut [u32], l: Layout, font: &mut TextRenderer, panel: Panel<'_>) {
    font.set_dpi(l.scale as f32);
    // Softbuffer may return an older frame after a resize/fullscreen transition.
    // Clear physical pixels, including rounding gaps and empty view margins.
    pixels.fill(INK);
    let mut c = Canvas {
        pixels,
        layout: l,
        font,
    };
    let title = "ImplicitForge";
    c.label([l.px(INSET), l.px(6)], l.view_width, 27, PAPER, title);
    let title_end = l.px(INSET) + c.font.width(title, 27. * l.scale as f32).ceil() as usize;
    let title_right = if panel.own_frame {
        // Match the 36-pixel native-window action targets in Layout::hit.
        let span = l.px(36);
        let start = l.view_width.saturating_sub(3 * span);
        let mut icon = |button: usize, x: usize, y: usize, w: usize, h: usize| {
            c.rect(
                start + button * span + l.px(x),
                l.px(y),
                l.px(w).max(1),
                l.px(h).max(1),
                PAPER,
            );
        };
        icon(0, 13, 24, 10, 1);
        icon(1, 12, 14, 12, 1);
        icon(1, 12, 25, 12, 1);
        icon(1, 12, 15, 1, 10);
        icon(1, 23, 15, 1, 10);
        for i in 0..11 {
            icon(2, 13 + i, 15 + i, 1, 1);
            icon(2, 23 - i, 15 + i, 1, 1);
        }
        start
    } else {
        l.view_width
    };
    c.label(
        [title_end + l.px(20), l.px(12)],
        title_right.saturating_sub(l.px(8)),
        BODY,
        MINT,
        panel.names[panel.selected],
    );

    if let Some(image) = panel.image {
        c.scene_image(image);
        let stats = format!(
            "{}   {} x {}   {:.1} ms",
            image.stage.label(),
            image.size[0],
            image.size[1],
            image.elapsed.as_secs_f64() * 1000.
        );
        c.label(
            [l.px(INSET), l.bottom + l.px(7)],
            l.view_width,
            17,
            PAPER,
            &stats,
        );
    } else {
        c.rect(
            0,
            l.top,
            l.view_width,
            l.bottom.saturating_sub(l.top),
            0x243133,
        );
        c.label(
            [l.px(INSET), l.top + l.px(16)],
            l.view_width,
            BODY,
            MINT,
            "Rendering preview...",
        );
    }

    c.rect(l.view_width, 0, l.width - l.view_width, l.height, PANEL);
    let editor = panel.editor;
    let gpu_active = matches!(panel.backend, Backend::Gpu | Backend::Software);
    let gpu_label = if !panel.gpu_allowed {
        "GPU  disabled"
    } else if gpu_active {
        "GPU  active"
    } else if panel.gpu_requested {
        "GPU  pending"
    } else {
        "GPU"
    };
    c.button(
        Hit::Cpu,
        editor,
        if gpu_active { "CPU" } else { "CPU  active" },
        !gpu_active,
        true,
    );
    c.button(Hit::Gpu, editor, gpu_label, gpu_active, panel.gpu_allowed);
    c.button(Hit::Mode, editor, panel.backend.label(), false, true);
    let mut rest = panel.adapter;
    for y in [73, 91] {
        let (line, tail) = c
            .font
            .wrap_line(rest, 16. * l.scale as f32, l.px(TRACK_W) as f32);
        c.panel_text(INSET, y, 16, MUTED, line);
        rest = tail;
    }
    c.button(
        Hit::Editor(Editor::Scene),
        editor,
        "Scene",
        editor == Editor::Scene,
        true,
    );
    c.button(
        Hit::Editor(Editor::Tool),
        editor,
        "Stamp tool",
        editor == Editor::Tool,
        true,
    );
    match editor {
        Editor::Scene => {
            for (i, name) in panel.names.iter().enumerate() {
                c.button(Hit::Scene(i), editor, name, i == panel.selected, true);
            }
            c.panel_text(INSET, 335, CAPTION, MUTED, "Geometry");
            for (i, &v) in panel.values.iter().enumerate() {
                c.slider(
                    editor,
                    i,
                    panel.parameter_names[i],
                    v,
                    &format!("{:.0}%", v * 100.),
                    i == panel.active_slider,
                );
            }
        }
        Editor::Tool => {
            let stamp = panel.stamp;
            let enabled = panel.stamp_enabled;
            let shape = match stamp.shape {
                StampShape::Sphere => "Sphere",
                StampShape::Box => "Box",
                StampShape::Torus => "Torus",
            };
            let operation = match stamp.op {
                StampOp::Union => "Union",
                StampOp::Subtract => "Subtract",
                StampOp::Intersect => "Intersect",
                StampOp::Smooth => "Smooth",
            };
            c.button(
                Hit::EnableStamp,
                editor,
                if enabled {
                    "Stamp preview  ·  On"
                } else {
                    "Stamp preview  ·  Off"
                },
                enabled,
                true,
            );
            c.button(Hit::Shape, editor, &format!("{shape}   ›"), false, true);
            c.button(
                Hit::Operation,
                editor,
                &format!("{operation}   ›"),
                false,
                true,
            );
            c.panel_text(INSET, 228, CAPTION, MUTED, "Position and size");
            for (i, name) in ["Center X", "Center Y", "Center Z", "Size"]
                .into_iter()
                .enumerate()
            {
                let (units, knob) = if i == 3 {
                    (stamp.size, (stamp.size - 0.15) / 1.35)
                } else {
                    let v = stamp.center[i];
                    (v, (v + 2.) / 4.)
                };
                c.slider(
                    editor,
                    i,
                    name,
                    knob,
                    &format!("{units:.2}"),
                    i == panel.active_slider,
                );
            }
            c.button(Hit::Apply, editor, "Apply", false, panel.can_apply);
            c.button(
                Hit::Undo,
                editor,
                &format!("Undo  ·  {} kept", panel.applied),
                false,
                panel.applied > 0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_is_native_until_capped() {
        assert_eq!(Layout::new(1008, 600, 1.).target(), [752, 528]);
        assert_eq!(Layout::new(973, 541, 1.).target(), [717, 469]);
        assert_eq!(Layout::new(1260, 750, 1.25).target(), [940, 660]);
        assert_eq!(Layout::new(1, 1, 1.).target(), [1, 1]);
        for (w, h, dpi) in [(2016, 1200, 2.), (7680, 4320, 1.), (1800, 3840, 1.5)] {
            let l = Layout::new(w, h, dpi);
            let [rw, rh] = l.target();
            assert!(rw <= 1440 && rh <= 1080 && (rw == 1440 || rh == 1080));
            let source_ratio = l.view_width as f64 / (l.bottom - l.top) as f64;
            assert!((rw as f64 - rh as f64 * source_ratio).abs() <= source_ratio.max(1.) * 0.51);
        }
    }

    #[test]
    fn pointer_matches_drawn_edges_at_fractional_dpi() {
        for dpi in [1., 1.25, 1.5, 2.] {
            let l = Layout::new((760. * dpi) as u32, (492. * dpi) as u32, dpi);
            for editor in [Editor::Scene, Editor::Tool] {
                let mut hits = vec![
                    Hit::Cpu,
                    Hit::Gpu,
                    Hit::Mode,
                    Hit::Editor(Editor::Scene),
                    Hit::Editor(Editor::Tool),
                ];
                hits.extend((0..editor.sliders()).map(Hit::Slider));
                match editor {
                    Editor::Scene => hits.extend((0..DEMO_COUNT).map(Hit::Scene)),
                    Editor::Tool => hits.extend([
                        Hit::EnableStamp,
                        Hit::Shape,
                        Hit::Operation,
                        Hit::Apply,
                        Hit::Undo,
                    ]),
                }
                for hit in hits {
                    let [x, y, w, h] = hit.bounds(editor);
                    let left = l.view_width + l.px(x);
                    let right = l.view_width + l.px(x + w);
                    assert_eq!(l.hit(left as f64, l.px(y) as f64, editor, false), Some(hit));
                    assert_eq!(
                        l.hit((right - 1) as f64, (l.px(y + h) - 1) as f64, editor, false),
                        Some(hit)
                    );
                    assert_eq!(l.hit(right as f64, l.px(y) as f64, editor, false), None);
                }
                assert_eq!(l.hit(1., 1., editor, false), None);
                assert_eq!(
                    l.hit(l.px(24) as f64, l.px(20) as f64, editor, true),
                    Some(Hit::Title)
                );
                assert_eq!(
                    l.hit(
                        (l.view_width - l.px(18)) as f64,
                        l.px(20) as f64,
                        editor,
                        true
                    ),
                    Some(Hit::Close)
                );
                assert_eq!(l.hit(1., l.top as f64, editor, false), Some(Hit::View));
            }
            assert_eq!(l.knob((l.view_width + l.px(INSET)) as f64), 0.);
            assert_eq!(l.knob((l.view_width + l.px(INSET + TRACK_W)) as f64), 1.);
        }
    }

    #[test]
    fn paint_replaces_dirty_pixels_across_resizes_and_pages() {
        let Ok(mut font) = TextRenderer::new() else {
            return;
        };
        let image = Image {
            pixels: vec![MINT],
            size: [1, 1],
            stage: crate::ui_worker::Stage::Preview,
            backend: Backend::CpuPruned,
            elapsed: std::time::Duration::ZERO,
        };
        for (w, h, dpi) in [(1547, 941, 1.25), (760, 492, 1.), (33, 17, 2.), (1, 1, 1.)] {
            let l = Layout::new(w, h, dpi);
            let dirty = 0xdeadbeef;
            let mut pixels = vec![dirty; l.width * l.height];
            for editor in [Editor::Scene, Editor::Tool] {
                pixels.fill(dirty);
                paint(
                    &mut pixels,
                    l,
                    &mut font,
                    Panel {
                        names: ["Scene"; DEMO_COUNT],
                        selected: 0,
                        parameter_names: ["Parameter"; 3],
                        values: [0., 0.5, 1.],
                        editor,
                        active_slider: 0,
                        stamp: Stamp::default(),
                        stamp_enabled: false,
                        applied: 0,
                        can_apply: false,
                        backend: image.backend,
                        gpu_requested: false,
                        gpu_allowed: false,
                        adapter: "Disabled",
                        own_frame: true,
                        image: (editor == Editor::Tool).then_some(&image),
                    },
                );
                assert!(!pixels.contains(&dirty));
            }
        }
    }
}
