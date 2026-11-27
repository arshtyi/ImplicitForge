use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use implicit_forge::gpu::Gpu;
use implicit_forge::render::{Camera, Params, cpu};
use implicit_forge::scene::Scene;
use winit::event_loop::EventLoopProxy;

const QUIET: Duration = Duration::from_millis(180);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Preview,
    Refine,
    Target,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preview => "Preview",
            Self::Refine => "Refining",
            Self::Target => "Target",
        }
    }

    pub fn size(self, target: [u32; 2]) -> [u32; 2] {
        let divisor = match self {
            Self::Preview => 4,
            Self::Refine => 2,
            Self::Target => return target,
        };
        target.map(|v| v.div_ceil(divisor).max(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    epoch: u64,
    revision: u64,
    pub stage: Stage,
}

// One running job and one implicit pending snapshot (the current UI state).
// During a drag, finish the running preview and then sample the newest pose.
// Fine passes and previews from a different scene/backend can never publish.
pub struct Schedule {
    epoch: u64,
    revision: u64,
    pub running: Option<Ticket>,
    next: Option<Stage>,
    quiet_at: Instant,
}

impl Schedule {
    pub fn new(now: Instant) -> Self {
        Self {
            epoch: 0,
            revision: 0,
            running: None,
            next: Some(Stage::Preview),
            quiet_at: now,
        }
    }

    pub fn change(&mut self, now: Instant, hard: bool) -> bool {
        self.revision += 1;
        self.epoch += u64::from(hard);
        self.quiet_at = now + QUIET;
        self.next = Some(Stage::Preview);
        self.running
            .is_some_and(|t| hard || t.stage != Stage::Preview)
    }

    pub fn deadline(&self) -> Option<Instant> {
        (self.running.is_none() && self.next.is_some_and(|s| s != Stage::Preview))
            .then_some(self.quiet_at)
    }

    pub fn start(&mut self, now: Instant) -> Option<Ticket> {
        if self.running.is_some() || self.deadline().is_some_and(|at| now < at) {
            return None;
        }
        let ticket = Ticket {
            epoch: self.epoch,
            revision: self.revision,
            stage: self.next.take()?,
        };
        self.running = Some(ticket);
        Some(ticket)
    }

    pub fn finish(&mut self, ticket: Ticket) -> bool {
        if self.running != Some(ticket) {
            return false;
        }
        self.running = None;
        if ticket.revision != self.revision {
            return ticket.epoch == self.epoch && ticket.stage == Stage::Preview;
        }
        self.next = match ticket.stage {
            Stage::Preview => Some(Stage::Refine),
            Stage::Refine => Some(Stage::Target),
            Stage::Target => None,
        };
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    CpuPruned,
    CpuBaseline,
    Gpu,
    Software,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::CpuPruned => "Rayon CPU / pruned",
            Self::CpuBaseline => "Rayon CPU / baseline",
            Self::Gpu => "GPU / WGSL",
            Self::Software => "Software / WGSL",
        }
    }
}

pub struct Image {
    pub pixels: Vec<u32>,
    pub size: [u32; 2],
    pub stage: Stage,
    pub backend: Backend,
    pub elapsed: Duration,
}

pub enum Event {
    Probed(Result<Box<Gpu>, String>),
    Finished(Ticket, Result<Option<Image>, String>),
}

pub struct Job {
    pub ticket: Ticket,
    pub scene: Arc<Scene>,
    pub camera: Camera,
    pub target: [u32; 2],
    pub backend: Backend,
}

pub enum Command {
    Render(Job),
    Install(Box<Gpu>),
}

pub struct Worker {
    tx: Sender<Command>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    pub fn new(proxy: EventLoopProxy<Event>) -> std::io::Result<Self> {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        thread::Builder::new()
            .name("render".into())
            .spawn(move || run(rx, flag, proxy))?;
        Ok(Self { tx, stop })
    }

    pub fn submit(&self, cmd: Command) {
        if matches!(&cmd, Command::Render(_)) {
            self.stop.store(false, Ordering::Relaxed);
        }
        self.tx.send(cmd).expect("render worker exited");
    }

    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn probe(proxy: EventLoopProxy<Event>) -> std::io::Result<()> {
    thread::Builder::new()
        .name("adapter-probe".into())
        .spawn(move || {
            let result = driver_call(Gpu::new).map(Box::new);
            let _ = proxy.send_event(Event::Probed(result));
        })?;
    Ok(())
}

// Some platform backends panic on driver initialization or device loss.
// Keep these failures local to the optional adapter, including their cause.
fn driver_call<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|cause| {
        let message = cause
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| cause.downcast_ref::<&str>().copied())
            .unwrap_or("no driver diagnostic");
        Err(format!("Compute backend panicked: {message}"))
    })
}

fn run(rx: Receiver<Command>, stop: Arc<AtomicBool>, proxy: EventLoopProxy<Event>) {
    let mut adapter = None;
    for cmd in rx {
        match cmd {
            Command::Install(gpu) => adapter = Some(gpu),
            Command::Render(job) => {
                let size = job.ticket.stage.size(job.target);
                let start = Instant::now();
                let params = Params::new(&job.scene, job.camera, size[0], size[1]);
                let params = match job.ticket.stage {
                    Stage::Preview | Stage::Refine => params.single_sample(),
                    Stage::Target => params,
                };
                let cancel = || stop.load(Ordering::Relaxed);
                let result = match job.backend {
                    Backend::Gpu | Backend::Software => {
                        let gpu = adapter.as_mut().expect("adapter installed before use");
                        driver_call(|| gpu.render(&job.scene, &params)).map(Some)
                    }
                    Backend::CpuPruned => {
                        Ok(cpu::<true, false>(&job.scene, &params, true, &cancel)
                            .map(|frame| frame.pixels))
                    }
                    Backend::CpuBaseline => {
                        Ok(cpu::<false, false>(&job.scene, &params, true, &cancel)
                            .map(|frame| frame.pixels))
                    }
                };
                if result.is_err() {
                    adapter = None;
                }
                let result = result.map(|pixels| {
                    pixels.map(|pixels| Image {
                        pixels,
                        size,
                        stage: job.ticket.stage,
                        backend: job.backend,
                        elapsed: start.elapsed(),
                    })
                });
                if proxy
                    .send_event(Event::Finished(job.ticket, result))
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_and_tiny_stage_sizes_round_up() {
        assert_eq!(Stage::Preview.size([17, 1]), [5, 1]);
        assert_eq!(Stage::Refine.size([17, 1]), [9, 1]);
        assert_eq!(Stage::Target.size([17, 1]), [17, 1]);
    }

    #[test]
    fn preview_makes_progress_during_drag_and_coalesces_to_latest_pose() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        let first = s.start(now).unwrap();
        for _ in 0..100 {
            assert!(!s.change(now, false));
            assert!(s.start(now).is_none());
        }
        assert!(s.finish(first));
        let latest = s.start(now).unwrap();
        assert_eq!((latest.stage, latest.revision), (Stage::Preview, 100));
        assert!(!s.finish(first));
        assert_eq!(s.running, Some(latest));
    }

    #[test]
    fn refinement_waits_for_quiet_and_stale_fine_completion_is_ignored() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        s.change(now, false);
        let preview = s.start(now).unwrap();
        assert!(s.finish(preview));
        assert!(s.start(now).is_none());
        let medium = s.start(now + QUIET).unwrap();
        assert_eq!(medium.stage, Stage::Refine);
        assert!(s.finish(medium));
        let fine = s.start(now + QUIET).unwrap();
        assert_eq!(fine.stage, Stage::Target);
        assert!(s.change(now + QUIET, false));
        assert!(!s.finish(fine));
        assert_eq!(s.start(now + QUIET).unwrap().stage, Stage::Preview);
    }

    #[test]
    fn scene_or_backend_change_even_rejects_pending_previews() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        let preview = s.start(now).unwrap();
        assert!(s.change(now, true));
        s.change(now, false);
        assert!(!s.finish(preview));
        let next = s.start(now).unwrap();
        assert_eq!((next.epoch, next.revision), (1, 2));
    }

    #[test]
    fn a_settled_view_runs_each_size_once_and_goes_idle() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        for (stage, size) in [
            (Stage::Preview, [160, 100]),
            (Stage::Refine, [320, 200]),
            (Stage::Target, [640, 400]),
        ] {
            let ticket = s.start(now).unwrap();
            assert_eq!(ticket.stage, stage);
            assert_eq!(ticket.stage.size([640, 400]), size);
            assert!(s.finish(ticket));
        }
        assert_eq!(s.start(now), None);
        assert!(s.deadline().is_none());
    }
}
