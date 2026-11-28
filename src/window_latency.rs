//! Opt-in observer for the live window experiment in `report/window_latency.py`.
//! Only the application takes timestamps. The driver waits for a settled Target
//! before each drag. Record in memory until the terminal present has returned.

use crate::ui_worker::{Image, QUIET, Stage, Ticket};
use serde::Serialize;
use serde_json::json;
use std::time::Instant;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

#[derive(Clone, Copy, Serialize)]
struct Motion {
    ns: u64,
    revision: u64,
    xy: (f64, f64),
}

#[derive(Serialize)]
struct Present {
    ns: u64,
    revision: u64,
    current_revision: u64,
    stage: &'static str,
}

#[derive(Serialize)]
struct Trial {
    id: u64,
    press_ns: u64,
    press_revision: u64,
    press_xy: (f64, f64),
    motions: Vec<Motion>,
    // First presentation of each distinct painted ticket during this trial,
    // including a repaint of a pre-drag ticket if one occurred.
    presents: Vec<Present>,
    release_ns: Option<u64>,
    release_revision: Option<u64>,
    preview_ns: Option<u64>,
    preview_revision: Option<u64>,
    target_ns: Option<u64>,
    target_revision: Option<u64>,
    invalid: Option<&'static str>,
}

impl Trial {
    fn new(id: u64, ns: u64, revision: u64, xy: (f64, f64)) -> Self {
        Self {
            id,
            press_ns: ns,
            press_revision: revision,
            press_xy: xy,
            motions: Vec::with_capacity(16),
            presents: Vec::with_capacity(16),
            release_ns: None,
            release_revision: None,
            preview_ns: None,
            preview_revision: None,
            target_ns: None,
            target_revision: None,
            invalid: None,
        }
    }

    fn last_revision(&self) -> u64 {
        self.motions
            .last()
            .map_or(self.press_revision, |m| m.revision)
    }

    fn motion(&mut self, ns: u64, revision: u64, xy: (f64, f64)) {
        if revision != self.last_revision() + 1 || self.release_ns.is_some() {
            self.invalid = Some("non-orbit edit or motion outside held drag");
        }
        self.motions.push(Motion { ns, revision, xy });
    }

    fn release(&mut self, ns: u64, revision: u64) {
        if self.motions.is_empty() || revision != self.last_revision() || self.release_ns.is_some()
        {
            self.invalid = Some("release did not follow a matching orbit motion");
        }
        self.release_ns = Some(ns);
        self.release_revision = Some(revision);
    }

    // Called only after buffer.present() succeeds, with the ticket belonging
    // to the pixels just painted. Worker completion alone cannot trigger this.
    fn present(&mut self, ns: u64, stage: Stage, revision: u64, current: u64) -> bool {
        let duplicate = self
            .presents
            .last()
            .is_some_and(|p| p.revision == revision && p.stage == stage.label());
        if !duplicate {
            self.presents.push(Present {
                ns,
                revision,
                current_revision: current,
                stage: stage.label(),
            });
        }
        if stage == Stage::Target && revision == current && self.release_ns.is_some() {
            if self.release_revision != Some(revision) || revision != self.last_revision() {
                self.invalid = Some("generation changed after release");
            } else if duplicate || self.motions.is_empty() {
                self.invalid = Some("target already visible or no motion");
            } else {
                self.target_ns = Some(ns);
                self.target_revision = Some(revision);
            }
            return true;
        }
        if stage == Stage::Preview
            && self.preview_ns.is_none()
            && !self.motions.is_empty()
            && revision == self.last_revision()
            && revision == current
        {
            self.preview_ns = Some(ns);
            self.preview_revision = Some(revision);
        }
        false
    }
}

pub(crate) struct Recorder {
    origin: Instant,
    serial: u64,
    ready: bool,
    trial: Option<Trial>,
}

impl Recorder {
    pub(crate) fn from_env() -> Option<Self> {
        (std::env::var_os("IMPLICITFORGE_WINDOW_LATENCY").as_deref()
            == Some(std::ffi::OsStr::new("1")))
        .then(|| Self {
            origin: Instant::now(),
            serial: 0,
            ready: false,
            trial: None,
        })
    }

    fn ns(&self, now: Instant) -> u64 {
        now.duration_since(self.origin).as_nanos() as u64
    }

    pub(crate) fn press(&mut self, now: Instant, revision: u64, xy: (f64, f64)) {
        self.serial += 1;
        self.trial = Some(Trial::new(self.serial, self.ns(now), revision, xy));
    }

    pub(crate) fn motion(&mut self, now: Instant, revision: u64, xy: (f64, f64)) {
        let ns = self.ns(now);
        if let Some(trial) = &mut self.trial {
            trial.motion(ns, revision, xy);
        }
    }

    pub(crate) fn release(&mut self, now: Instant, revision: u64) {
        let ns = self.ns(now);
        if let Some(trial) = &mut self.trial {
            trial.release(ns, revision);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn presented(
        &mut self,
        now: Instant,
        ticket: Ticket,
        current: u64,
        image: &Image,
        scene: &str,
        window: &Window,
        target: [u32; 2],
    ) {
        let ns = self.ns(now);
        let revision = ticket.revision();
        let finished = self
            .trial
            .as_mut()
            .is_some_and(|t| t.present(ns, ticket.stage, revision, current));
        if !finished && (self.ready || ticket.stage != Stage::Target || revision != current) {
            return;
        }
        let first = !self.ready;
        self.ready = true;
        let xid = window.window_handle().ok().and_then(|h| match h.as_raw() {
            RawWindowHandle::Xlib(h) => Some(json!(h.window)),
            RawWindowHandle::Xcb(h) => Some(json!(h.window.get())),
            _ => None,
        });
        let size = window.inner_size();
        let event = json!({
            "kind": if first { "ready" } else { "trial" }, "xid": xid,
            "revision": current, "scene": scene, "backend": image.backend.label(),
            "rayon_workers": rayon::current_num_threads(),
            "window": [size.width, size.height], "scale": window.scale_factor(),
            "preview": Stage::Preview.size(target), "target": image.size,
            "quiet_ms": QUIET.as_millis(), "trial": self.trial.take(),
        });
        println!("WINDOW_LATENCY {event}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_and_in_flight_previews_are_not_fresh_feedback() {
        let mut t = Trial::new(1, 10, 6, (0., 0.));
        t.motion(20, 7, (1., 0.));
        t.motion(30, 8, (2., 0.));
        for rev in [6, 7] {
            assert!(!t.present(35, Stage::Preview, rev, 8));
        }
        assert!(t.preview_ns.is_none());
        assert!(!t.present(40, Stage::Preview, 8, 8));
        assert!(!t.present(45, Stage::Preview, 8, 8));
        assert_eq!(t.preview_ns, Some(40));
        assert_eq!(t.preview_revision, Some(8));
        t.release(50, 8);
        assert!(!t.present(55, Stage::Target, 6, 8));
        assert!(t.target_ns.is_none());
        assert!(t.present(80, Stage::Target, 8, 8));
        assert_eq!(t.target_ns, Some(80));
        assert!(t.invalid.is_none());
    }

    #[test]
    fn pre_release_target_repaint_and_post_release_changes_invalidate_trial() {
        let mut t = Trial::new(1, 10, 5, (0., 0.));
        t.motion(20, 6, (1., 0.));
        assert!(!t.present(30, Stage::Target, 6, 6));
        t.release(40, 6);
        assert!(t.present(50, Stage::Target, 6, 6));
        assert!(t.target_ns.is_none() && t.invalid.is_some());

        let mut next = Trial::new(2, 60, 6, (1., 0.));
        next.motion(70, 7, (2., 0.));
        next.release(80, 7);
        assert!(!next.present(90, Stage::Target, 6, 8));
        assert!(next.present(100, Stage::Target, 8, 8));
        assert!(next.target_ns.is_none() && next.invalid.is_some());
    }

    #[test]
    fn preview_can_arrive_after_release_and_may_be_missing_entirely() {
        for preview in [false, true] {
            let mut t = Trial::new(1, 0, 0, (0., 0.));
            t.motion(1, 1, (1., 0.));
            t.release(2, 1);
            if preview {
                assert!(!t.present(3, Stage::Preview, 1, 1));
            }
            assert!(t.present(4, Stage::Target, 1, 1));
            assert_eq!(t.preview_ns, preview.then_some(3));
            assert_eq!(t.target_ns, Some(4));
        }
    }
}
