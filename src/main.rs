mod stamp_controls;
mod text;
mod ui;
mod ui_worker;
mod window_latency;

use std::error::Error;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use implicit_forge::render::Camera;
use implicit_forge::scene::{
    DEMO_COUNT, MAX_INSTRUCTIONS, Scene, Stamp, StampAxis, StampOp, StampShape, demo,
};
use softbuffer::{Context, Surface};
use ui::{Editor, Hit, Layout, Panel};
use ui_worker::{Backend, Command, Event, Image, Job, Schedule, Ticket, Worker};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{
    ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy, OwnedDisplayHandle,
};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

enum Adapter {
    Disabled,
    Untried,
    Probing,
    Ready { backend: Backend, name: String },
    Failed(String),
}

impl Adapter {
    fn note(&self) -> &str {
        match self {
            Self::Disabled | Self::Untried | Self::Probing => "",
            Self::Ready { name, .. } => name,
            Self::Failed(reason) => reason,
        }
    }
}

#[derive(Clone, Copy)]
enum Drag {
    Orbit,
    Parameter,
}

struct Design {
    scene: Arc<Scene>,
    base_len: usize,
    values: [f32; 3],
    stamp: Stamp,
    enabled: bool,
    applied: Vec<Stamp>,
    dirty: bool,
}

impl Design {
    fn new(index: usize) -> Self {
        let values = [0.5; 3];
        let scene = Arc::new(demo(index, values));
        Self {
            base_len: scene.code.len(),
            scene,
            values,
            stamp: Stamp::default(),
            enabled: false,
            applied: Vec::new(),
            dirty: false,
        }
    }

    fn has_room(&self) -> bool {
        self.base_len + 2 * (self.applied.len() + 1) <= MAX_INSTRUCTIONS
    }

    fn refresh(&mut self, index: usize) {
        if self.dirty {
            let mut scene = demo(index, self.values).with_stamps(&self.applied, true);
            if self.enabled {
                scene = scene.with_stamp(self.stamp);
            }
            self.scene = Arc::new(scene);
            self.dirty = false;
        }
    }
}

struct Desktop {
    surface: Surface<OwnedDisplayHandle, Rc<Window>>,
    window: Rc<Window>,
    layout: Layout,
    own_frame: bool,
}

struct App {
    context: Context<OwnedDisplayHandle>,
    font: text::TextRenderer,
    desktop: Option<Desktop>,
    proxy: EventLoopProxy<Event>,
    worker: Worker,
    schedule: Schedule,
    designs: [Design; DEMO_COUNT],
    selected: usize,
    editor: Editor,
    active_slider: usize,
    camera: Camera,
    adapter: Adapter,
    want_adapter: bool,
    pruned: bool,
    heatmap: bool,
    image: Option<Image>,
    image_ticket: Option<Ticket>,
    latency: Option<window_latency::Recorder>,
    cursor: Option<(f64, f64)>,
    drag: Option<Drag>,
    trace: bool,
    error: Option<String>,
}

impl App {
    fn new(events: &EventLoop<Event>, cpu_only: bool) -> Result<Self, Box<dyn Error>> {
        let proxy = events.create_proxy();
        Ok(Self {
            context: Context::new(events.owned_display_handle())?,
            font: text::TextRenderer::new()?,
            worker: Worker::new(proxy.clone())?,
            proxy,
            desktop: None,
            schedule: Schedule::new(Instant::now()),
            designs: std::array::from_fn(Design::new),
            editor: Editor::Scene,
            active_slider: 0,
            selected: 0,
            camera: Camera::default(),
            adapter: if cpu_only {
                Adapter::Disabled
            } else {
                Adapter::Untried
            },
            want_adapter: false,
            pruned: true,
            heatmap: false,
            image: None,
            image_ticket: None,
            latency: window_latency::Recorder::from_env(),
            cursor: None,
            drag: None,
            trace: std::env::var_os("IMPLICITFORGE_TRACE").is_some_and(|v| v == "1"),
            error: None,
        })
    }

    fn open(&mut self, events: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        // WSLg remotes client decorations as separate host windows. Keep its
        // title bar, borders and content on a single opaque Wayland surface.
        #[cfg(target_os = "linux")]
        let own_frame = winit::platform::wayland::ActiveEventLoopExtWayland::is_wayland(events)
            && std::env::var_os("WSL_DISTRO_NAME").is_some();
        #[cfg(not(target_os = "linux"))]
        let own_frame = false;
        let attributes = Window::default_attributes()
            .with_title("ImplicitForge")
            .with_decorations(!own_frame)
            .with_inner_size(LogicalSize::new(1008., 600.))
            .with_min_inner_size(LogicalSize::new(760., 492.));
        let window = Rc::new(events.create_window(attributes)?);
        let surface = Surface::new(&self.context, window.clone())?;
        let size = window.inner_size();
        let layout = Layout::new(size.width, size.height, window.scale_factor());
        self.desktop = Some(Desktop {
            surface,
            window,
            layout,
            own_frame,
        });
        self.change(true);
        Ok(())
    }

    fn redraw(&self) {
        if let Some(d) = &self.desktop {
            d.window.request_redraw();
        }
    }

    fn change(&mut self, hard: bool) {
        if self.schedule.change(Instant::now(), hard) {
            self.worker.cancel();
        }
        if hard {
            self.image = None;
            self.image_ticket = None;
        }
        self.redraw();
    }

    fn backend(&self) -> Backend {
        if let (true, Adapter::Ready { backend, .. }) = (self.want_adapter, &self.adapter) {
            *backend
        } else if self.pruned {
            Backend::CpuPruned
        } else {
            Backend::CpuBaseline
        }
    }

    fn pump(&mut self, events: &ActiveEventLoop) {
        let Some(d) = &self.desktop else {
            return;
        };
        if d.layout.width == 0 || d.layout.height == 0 {
            events.set_control_flow(ControlFlow::Wait);
            return;
        }
        if let Some(ticket) = self.schedule.start(Instant::now()) {
            self.designs[self.selected].refresh(self.selected);
            self.worker.submit(Command::Render(Job {
                ticket,
                scene: self.designs[self.selected].scene.clone(),
                camera: self.camera,
                target: d.layout.target(),
                backend: self.backend(),
                heatmap: self.heatmap,
            }));
            self.redraw();
        }
        events.set_control_flow(
            self.schedule
                .deadline()
                .map_or(ControlFlow::Wait, ControlFlow::WaitUntil),
        );
    }

    fn draw(&mut self) -> Result<(), Box<dyn Error>> {
        let backend = self.backend();
        let Some(d) = &mut self.desktop else {
            return Ok(());
        };
        let l = d.layout;
        let (Some(w), Some(h)) = (
            NonZeroU32::new(l.width as u32),
            NonZeroU32::new(l.height as u32),
        ) else {
            return Ok(());
        };
        d.surface.resize(w, h)?;
        let mut buffer = d.surface.buffer_mut()?;
        let design = &self.designs[self.selected];
        ui::paint(
            &mut buffer,
            l,
            &mut self.font,
            Panel {
                names: self.designs.each_ref().map(|d| d.scene.name.as_str()),
                selected: self.selected,
                parameter_names: design.scene.parameter_names.each_ref().map(String::as_str),
                values: design.values,
                editor: self.editor,
                active_slider: self.active_slider,
                stamp: design.stamp,
                stamp_enabled: design.enabled,
                applied: design.applied.len(),
                can_apply: design.enabled && design.has_room(),
                backend,
                heatmap: self.heatmap,
                adapter: self.adapter.note(),
                gpu_requested: self.want_adapter,
                gpu_allowed: !matches!(self.adapter, Adapter::Disabled),
                own_frame: d.own_frame,
                image: self.image.as_ref(),
            },
        );
        d.window.pre_present_notify();
        buffer.present()?;
        if let (Some(recorder), Some(ticket), Some(image)) =
            (&mut self.latency, self.image_ticket, &self.image)
        {
            recorder.presented(
                Instant::now(),
                ticket,
                self.schedule.revision(),
                image,
                &design.scene.name,
                &d.window,
                l.target(),
            );
        }
        Ok(())
    }

    fn select(&mut self, index: usize) {
        if self.selected != index {
            self.selected = index;
            self.active_slider = 0;
            self.drag = None;
            self.camera = Camera::default();
            self.change(true);
        }
    }

    fn geometry_changed(&mut self) {
        self.designs[self.selected].dirty = true;
        self.change(false);
    }

    fn slider_value(&self) -> f32 {
        let d = &self.designs[self.selected];
        match self.editor {
            Editor::Scene => d.values[self.active_slider],
            editor => editor.field(d.stamp, self.active_slider).fraction(d.stamp),
        }
    }

    fn parameter(&mut self, knob: f32) {
        let v = knob.clamp(0., 1.);
        if v == self.slider_value() {
            self.redraw();
            return;
        }
        let d = &mut self.designs[self.selected];
        match self.editor {
            Editor::Scene => d.values[self.active_slider] = v,
            editor => editor
                .field(d.stamp, self.active_slider)
                .set(&mut d.stamp, v),
        }
        if self.editor == Editor::Scene || d.enabled {
            self.geometry_changed();
        } else {
            self.redraw();
        }
    }

    fn editor(&mut self, editor: Editor) {
        self.editor = editor;
        self.active_slider = 0;
        self.drag = None;
        self.redraw();
    }

    fn tool_action(&mut self, action: Hit) {
        let d = &mut self.designs[self.selected];
        match action {
            Hit::EnableStamp if d.enabled || d.has_room() => d.enabled = !d.enabled,
            Hit::Shape => {
                d.stamp.shape = match d.stamp.shape {
                    StampShape::Sphere => StampShape::Box,
                    StampShape::Box => StampShape::Torus,
                    StampShape::Torus => StampShape::Cylinder,
                    StampShape::Cylinder => StampShape::Sphere,
                }
            }
            Hit::Operation => {
                d.stamp.op = match d.stamp.op {
                    StampOp::Union => StampOp::Subtract,
                    StampOp::Subtract => StampOp::Intersect,
                    StampOp::Intersect => StampOp::Smooth,
                    StampOp::Smooth => StampOp::SmoothSubtract,
                    StampOp::SmoothSubtract => StampOp::Union,
                }
            }
            Hit::Axis if d.stamp.shape != StampShape::Sphere => {
                d.stamp.axis = match d.stamp.axis {
                    StampAxis::Y => StampAxis::X,
                    StampAxis::X => StampAxis::Z,
                    StampAxis::Z => StampAxis::Y,
                };
            }
            Hit::Apply if d.enabled && d.has_room() => {
                d.applied.push(d.stamp);
                d.enabled = false;
            }
            Hit::Undo if !d.applied.is_empty() => {
                d.applied.pop();
                d.enabled = false;
            }
            _ => return,
        }
        self.active_slider = self.active_slider.min(self.editor.sliders(d.stamp) - 1);
        self.geometry_changed();
    }

    fn fullscreen(&mut self) {
        if let Some(d) = &self.desktop {
            let next = d
                .window
                .fullscreen()
                .is_none()
                .then(|| Fullscreen::Borderless(d.window.current_monitor()));
            d.window.set_fullscreen(next);
            self.drag = None;
            self.change(true);
        }
    }

    fn resized(&mut self, size: PhysicalSize<u32>) {
        if let Some(d) = &mut self.desktop {
            d.layout = Layout::new(size.width, size.height, d.window.scale_factor());
            self.drag = None;
            // Previous-size previews cannot publish into the resized framebuffer.
            self.change(true);
        }
    }

    fn orbit(&mut self, dx: f32, dy: f32) {
        self.camera.yaw = (self.camera.yaw + dx).rem_euclid(std::f32::consts::TAU);
        self.camera.pitch = (self.camera.pitch + dy).clamp(-1.35, 1.35);
        self.change(false);
    }

    fn zoom(&mut self, delta: f32) {
        let radius = (self.camera.radius * (-delta * 0.12).exp()).clamp(2.0, 16.0);
        if radius != self.camera.radius {
            self.camera.radius = radius;
            self.change(false);
        }
    }

    fn adapter_error(&mut self, error: String) {
        let reason = format!("Adapter unavailable: {error}");
        eprintln!("{reason}");
        self.adapter = Adapter::Failed(reason);
        self.want_adapter = false;
        self.change(true);
    }

    fn set_gpu(&mut self, requested: bool) {
        if matches!(self.adapter, Adapter::Disabled) || requested == self.want_adapter {
            return;
        }
        if requested && self.heatmap {
            self.heatmap = false;
            self.change(true);
        }
        self.want_adapter = requested;
        if requested && matches!(self.adapter, Adapter::Untried | Adapter::Failed(_)) {
            self.adapter = Adapter::Probing;
            if let Err(error) = ui_worker::probe(self.proxy.clone()) {
                self.adapter_error(error.to_string());
                return;
            }
        }
        if matches!(self.adapter, Adapter::Ready { .. }) {
            self.change(true);
        } else {
            self.redraw();
        }
    }

    fn toggle_heatmap(&mut self) {
        self.heatmap = !self.heatmap;
        self.want_adapter = false;
        self.change(true);
    }

    fn toggle_cpu_mode(&mut self) {
        self.want_adapter = false;
        self.pruned = !self.pruned;
        self.change(true);
    }

    fn key(&mut self, code: KeyCode, repeat: bool, events: &ActiveEventLoop) {
        match code {
            KeyCode::Digit1 => self.select(0),
            KeyCode::Digit2 => self.select(1),
            KeyCode::Digit3 => self.select(2),
            KeyCode::Digit4 => self.select(3),
            KeyCode::Digit5 => self.select(4),
            KeyCode::Digit6 => self.select(5),
            KeyCode::Digit7 => self.select(6),
            KeyCode::Digit8 => self.select(7),
            KeyCode::Digit9 => self.select(8),
            KeyCode::ArrowLeft => self.orbit(-0.09, 0.),
            KeyCode::ArrowRight => self.orbit(0.09, 0.),
            KeyCode::ArrowUp => self.orbit(0., 0.07),
            KeyCode::ArrowDown => self.orbit(0., -0.07),
            KeyCode::Equal | KeyCode::NumpadAdd => self.zoom(1.),
            KeyCode::Minus | KeyCode::NumpadSubtract => self.zoom(-1.),
            KeyCode::BracketLeft => self.parameter(self.slider_value() - 0.05),
            KeyCode::BracketRight => self.parameter(self.slider_value() + 0.05),
            KeyCode::KeyH if !repeat => self.toggle_heatmap(),
            KeyCode::Tab if !repeat => {
                let count = self.editor.sliders(self.designs[self.selected].stamp);
                self.active_slider = (self.active_slider + 1) % count;
                self.redraw();
            }
            KeyCode::KeyT if !repeat => self.editor(if self.editor != Editor::Scene {
                Editor::Scene
            } else {
                Editor::Tool
            }),
            KeyCode::Space if !repeat && self.editor != Editor::Scene => {
                self.tool_action(Hit::EnableStamp)
            }
            KeyCode::Enter if !repeat && self.editor != Editor::Scene => {
                self.tool_action(Hit::Apply)
            }
            KeyCode::Backspace if !repeat && self.editor != Editor::Scene => {
                self.tool_action(Hit::Undo)
            }
            KeyCode::F11 if !repeat => self.fullscreen(),
            KeyCode::KeyG if !repeat => self.set_gpu(!self.want_adapter),
            KeyCode::KeyB if !repeat => self.toggle_cpu_mode(),
            KeyCode::KeyR if !repeat => {
                self.camera = Camera::default();
                self.change(false);
            }
            KeyCode::Escape
                if self
                    .desktop
                    .as_ref()
                    .is_some_and(|d| d.window.fullscreen().is_some()) =>
            {
                self.fullscreen()
            }
            KeyCode::Escape => events.exit(),
            _ => {}
        }
    }

    fn press(&mut self, l: Layout, events: &ActiveEventLoop) {
        let Some((x, y)) = self.cursor else {
            return;
        };
        let Some(d) = &self.desktop else {
            return;
        };
        match l.hit(
            x,
            y,
            self.editor,
            d.own_frame,
            self.designs[self.selected].stamp,
        ) {
            Some(Hit::View) => self.drag = Some(Drag::Orbit),
            Some(Hit::Scene(index)) => self.select(index),
            Some(Hit::Editor(editor)) => self.editor(editor),
            Some(Hit::Fields) => self.editor(if self.editor == Editor::Place {
                Editor::Tool
            } else {
                Editor::Place
            }),
            Some(Hit::Slider(index)) => {
                self.active_slider = index;
                self.drag = Some(Drag::Parameter);
                self.parameter(l.knob(x));
            }
            Some(Hit::Heatmap) => self.toggle_heatmap(),
            Some(Hit::Mode) => self.toggle_cpu_mode(),
            Some(Hit::Cpu) => self.set_gpu(false),
            Some(Hit::Gpu) => self.set_gpu(true),
            Some(Hit::Title) => {
                let _ = d.window.drag_window();
            }
            Some(Hit::Resize(direction)) => {
                if !d.window.is_maximized() && d.window.fullscreen().is_none() {
                    let _ = d.window.drag_resize_window(direction);
                }
            }
            Some(Hit::Minimize) => d.window.set_minimized(true),
            Some(Hit::Maximize) if d.window.fullscreen().is_some() => self.fullscreen(),
            Some(Hit::Maximize) => d.window.set_maximized(!d.window.is_maximized()),
            Some(Hit::Close) => events.exit(),
            Some(action) => self.tool_action(action),
            None => {}
        }
    }
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.desktop.is_none()
            && let Err(error) = self.open(events)
        {
            self.error = Some(format!("Opening window: {error}"));
            events.exit();
        }
    }

    fn suspended(&mut self, events: &ActiveEventLoop) {
        self.desktop = None;
        self.drag = None;
        self.cursor = None;
        self.change(true);
        events.set_control_flow(ControlFlow::Wait);
    }

    fn user_event(&mut self, _events: &ActiveEventLoop, event: Event) {
        match event {
            Event::Probed(Ok(gpu)) => {
                let backend = if gpu.software {
                    Backend::Software
                } else {
                    Backend::Gpu
                };
                let name = gpu.name.clone();
                self.worker.submit(Command::Install(gpu));
                self.adapter = Adapter::Ready { backend, name };
                if self.want_adapter {
                    self.change(true);
                }
            }
            Event::Probed(Err(error)) => self.adapter_error(error),
            Event::Finished(ticket, result) => {
                let publish = self.schedule.finish(ticket);
                match result {
                    Ok(Some(image)) if publish => {
                        if self.trace {
                            eprintln!(
                                "frame scene=\"{}\" stage={} size={}x{} backend=\"{}\" ms={:.2}",
                                self.designs[self.selected].scene.name,
                                image.stage.label(),
                                image.size[0],
                                image.size[1],
                                image.backend.label(),
                                image.elapsed.as_secs_f64() * 1000.
                            );
                        }
                        self.image = Some(image);
                        self.image_ticket = Some(ticket);
                    }
                    Err(error) => self.adapter_error(error),
                    _ => {}
                }
            }
        }
        self.redraw();
    }

    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        self.pump(events);
    }

    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let dispatch = self.latency.as_ref().map(|_| Instant::now());
        let Some(d) = &mut self.desktop else {
            return;
        };
        if d.window.id() != id {
            return;
        }
        let l = d.layout;
        match event {
            WindowEvent::CloseRequested => events.exit(),
            WindowEvent::Resized(size) => self.resized(size),
            WindowEvent::ScaleFactorChanged { .. } => {
                let size = d.window.inner_size();
                self.resized(size);
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.draw() {
                    self.error = Some(format!("Presenting software framebuffer: {error}"));
                    events.exit();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.key(code, event.repeat, events);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if state == ElementState::Pressed {
                    self.press(l, events);
                    if matches!(self.drag, Some(Drag::Orbit))
                        && let (Some(r), Some(t), Some(p)) =
                            (&mut self.latency, dispatch, self.cursor)
                    {
                        r.press(t, self.schedule.revision(), p);
                    }
                } else {
                    if matches!(self.drag, Some(Drag::Orbit))
                        && let (Some(r), Some(t)) = (&mut self.latency, dispatch)
                    {
                        r.release(t, self.schedule.revision());
                    }
                    self.drag = None;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (position.x, position.y);
                let previous = self.cursor.replace((x, y));
                match (self.drag, previous) {
                    (Some(Drag::Orbit), Some((px, py))) if x != px || y != py => {
                        let sensitivity = 0.008 / d.window.scale_factor() as f32;
                        self.orbit((px - x) as f32 * sensitivity, (y - py) as f32 * sensitivity);
                        if let (Some(r), Some(t)) = (&mut self.latency, dispatch) {
                            r.motion(t, self.schedule.revision(), (x, y));
                        }
                    }
                    (Some(Drag::Parameter), _) => self.parameter(l.knob(x)),
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.cursor.is_some_and(|(x, y)| {
                    matches!(
                        l.hit(
                            x,
                            y,
                            self.editor,
                            d.own_frame,
                            self.designs[self.selected].stamp
                        ),
                        Some(Hit::View)
                    )
                }) =>
            {
                self.zoom(match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 60.,
                });
            }
            WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. } => {
                self.drag = None;
                self.cursor = None;
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut cpu_only = false;
    let mut start_gpu = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--cpu" => cpu_only = true,
            "--gpu" => start_gpu = true,
            "-h" | "--help" => {
                println!(
                    "Usage: implicit-forge [--cpu | --gpu]\n\nDefault: start on CPU. Select CPU or GPU in the window.\n--gpu  Connect to GPU at launch with CPU fallback.\n--cpu  Disable compute probing."
                );
                return Ok(());
            }
            _ => return Err(format!("Unknown argument {arg:?}. Use --help.").into()),
        }
    }
    if cpu_only && start_gpu {
        return Err("Choose either --cpu or --gpu".into());
    }
    let events = EventLoop::<Event>::with_user_event().build()?;
    let mut app = App::new(&events, cpu_only)?;
    if start_gpu {
        app.set_gpu(true);
    }
    events.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}
