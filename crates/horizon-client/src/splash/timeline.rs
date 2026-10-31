//! The startup splash's motion (fork `NeuHorizon`): what is where at a
//! moment, from the loading's progress. Drawing is `super::gpu`'s.
//!
//! Every mod that loads (or, without mods, every loading stage) is a drop:
//! it flies in from a random point of a screen edge, curving and speeding
//! up, and merges into a pool in the middle. The pool starts empty; its
//! radius rides a damped spring toward a size that grows fast with the first
//! drops and tapers off, and each landing kicks it outward. A fox runs in and
//! laps the pool. When loading ends the fox crouches with a hip wiggle,
//! leaps, and at the top of its leap the pool springs into the game window's
//! rectangle; the fox dives head-first behind it and the game window takes
//! over there.
//!
//! Lengths are in the splash surface's logical pixels; `k` scales the
//! design's units (a 640-pixel square) to the surface.

/// The most balls the liquid draws (the shader's array).
pub const MAX_BALLS: usize = 24;
/// A drop's flight takes this long, plus up to [`FLIGHT_SPREAD`].
const FLIGHT: f32 = 1.0;
const FLIGHT_SPREAD: f32 = 0.35;
/// Drops leave at least this far apart, so a fast start still shows the pool
/// grow drop by drop.
const LAUNCH_GAP: f32 = 0.12;
/// Once loading is done, the drops still to come hurry: the game is waiting.
const HURRY_GAP: f32 = 0.05;
const HURRY_FLIGHT: f32 = 0.55;
/// The finale never starts before this many seconds of splash.
const MIN_BEFORE_FINALE: f32 = 3.0;
/// The finale: after the last landing, the crouch, then the leap to its apex.
const SETTLE: f32 = 0.25;
const CROUCH: f32 = 0.7;
const LEAP: f32 = 0.5;
/// After the apex: the pool turns into the window, and the window takes over.
const MORPH: f32 = 0.38;
pub const HANDOVER: f32 = 0.55;
/// The fox runs in over these seconds, then laps.
const FOX_IN: (f32, f32) = (0.5, 2.0);

/// The chosen look (chino's settings, 2026-10-28).
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub fox_size: f32,
    pub lap_speed: f32,
    /// Smooth-min radius, pixels.
    pub gooeyness: f32,
    pub window_corners: f32,
    /// No motion (the setting `still`): the pool at its size, the stage line
    /// and the bar; no drops in flight, no fox, no morph.
    pub still: bool,
    /// The liquid's edge and core colours (sRGB, 0..1).
    pub edge: [f32; 3],
    pub deep: [f32; 3],
}
impl Default for Look {
    fn default() -> Self {
        // "ember": a pale gold edge to a deep red core.
        Look {
            fox_size: 1.35,
            lap_speed: 1.35,
            gooeyness: 39.0,
            window_corners: 0.0,
            still: false,
            edge: [1.0, 0.78, 0.55],
            deep: [0.62, 0.16, 0.12],
        }
    }
}

struct Drop {
    launched: f32,
    flight: f32,
    edge: u8,
    along: f32,
    radius: f32,
    bend: f32,
}

/// Where the fox is and how it moves, for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fox {
    /// Feet, in pixels.
    pub x: f32,
    pub y: f32,
    /// Pixels per block.
    pub scale: f32,
    /// Facing on the ground plane: 0 to the right, π/2 toward the viewer.
    pub heading: f32,
    /// Nose up (positive) or down, radians.
    pub pitch: f32,
    /// The run's leg phase (`WalkAnimationState.position`) and speed.
    pub run: f32,
    pub speed: f32,
    /// `Fox.crouchAmount` while crouching, and the wiggle's age.
    pub crouch: f32,
    pub age: f32,
    /// Behind the pool (drawn before the liquid).
    pub behind: bool,
}

/// One frame of the splash.
#[derive(Clone, Debug)]
pub struct Frame {
    /// (x, y, radius) in pixels.
    pub balls: Vec<[f32; 3]>,
    /// 0: metaballs, 1: the window's rounded box.
    pub morph: f32,
    /// The window's rectangle: centre and size, pixels.
    pub rect: [f32; 4],
    /// The window's own colour over the liquid, 0..1.
    pub window: f32,
    pub fox: Option<Fox>,
    pub text: String,
    pub text_alpha: f32,
    /// Below the pool: the line's y.
    pub text_y: f32,
    pub progress: f32,
    /// The pool's radius, pixels.
    pub pool: f32,
}

pub struct Timeline {
    width: f32,
    height: f32,
    k: f32,
    look: Look,
    /// The game window's size in pixels (the rectangle the pool becomes).
    window: (f32, f32),
    names: Vec<String>,
    mods: bool,
    drops: Vec<Drop>,
    seed: u32,
    t: f32,
    last_launch: f32,
    pool: (f32, f32),
    pool_landed: usize,
    loaded: bool,
    finale: Option<f32>,
    /// Where the lap ended, for the pounce.
    fox_at: (f32, f32),
    lap_phase: f32,
    /// The pool's height on the surface, as a share of it.
    centre_y: f32,
    /// When the fox started running in (infinite: not yet, its model is
    /// still being prepared).
    fox_from: f32,
    /// What the setup is doing and how far it is, over the drops' own line
    /// and bar (the setup's splash, `game/setup_splash.rs`).
    status: Option<(String, f32)>,
    /// The pool never settles smaller: the size the setup's splash left it
    /// at, when the game's splash takes over from it.
    pool_floor: f32,
}

/// Where the setup's splash was, for the game's splash to carry on from it
/// (the fox mid-lap, the pool at its size): seconds since it opened, when
/// its fox came, its pool's radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    pub t: f32,
    pub fox_from: f32,
    pub pool: f32,
}

impl Timeline {
    /// `names`: the mods that load, one drop each (`mods`), or the loading
    /// stages when there are none.
    pub fn new(
        width: f32,
        height: f32,
        window: (f32, f32),
        names: Vec<String>,
        mods: bool,
        look: Look,
    ) -> Self {
        Timeline {
            width,
            height,
            k: width.min(height) / 640.0,
            look,
            window,
            seed: 7 + names.len() as u32,
            names,
            mods,
            drops: Vec::new(),
            t: 0.0,
            last_launch: f32::NEG_INFINITY,
            pool: (0.0, 0.0),
            pool_landed: 0,
            loaded: false,
            finale: None,
            fox_at: (0.0, 0.0),
            lap_phase: 0.0,
            centre_y: 0.47,
            fox_from: 0.0,
            status: None,
            pool_floor: 0.0,
        }
    }
    /// No fox until `fox_arrives`.
    pub fn without_fox(mut self) -> Self {
        self.fox_from = f32::INFINITY;
        self
    }
    /// The fox runs in now (when it hasn't yet).
    pub fn fox_arrives(&mut self) {
        if self.fox_from.is_infinite() {
            self.fox_from = self.t;
        }
    }
    /// The setup's line and bar, in place of the drops' own.
    pub fn set_status(&mut self, text: String, progress: f32) {
        self.status = Some((text, progress.clamp(0.0, 1.0)));
    }
    /// Where this splash is, for `resume`.
    pub fn moment(&self) -> Moment {
        Moment {
            t: self.t,
            fox_from: self.fox_from,
            pool: self.pool.0,
        }
    }
    /// Carries on from another splash's moment: its clock (the pool's wobble
    /// and the fox's lap go on where they were), its fox, and its pool's size.
    pub fn resume(mut self, m: Moment) -> Self {
        self.t = m.t.max(0.0);
        // Its fox, or (when it never came) one running in now.
        self.fox_from = if m.fox_from.is_finite() {
            m.fox_from
        } else {
            self.t
        };
        self.pool = (m.pool.max(0.0), 0.0);
        self.pool_floor = m.pool.max(0.0);
        self
    }
    /// The pool in the exact middle: when the splash fills the game
    /// window's own rectangle, the window it turns into matches its edges.
    pub fn centred(mut self) -> Self {
        self.centre_y = 0.5;
        self
    }
    fn random(&mut self) -> f32 {
        // Park-Miller, as the prototype.
        self.seed = ((u64::from(self.seed) * 16807) % 2_147_483_647) as u32;
        (self.seed - 1) as f32 / 2_147_483_646.0
    }
    pub fn centre(&self) -> (f32, f32) {
        (self.width / 2.0, self.height * self.centre_y)
    }
    fn total(&self) -> usize {
        self.names.len().max(1)
    }
    fn landed(&self) -> usize {
        self.drops
            .iter()
            .filter(|d| self.t >= d.launched + d.flight)
            .count()
    }
    /// When the finale started (the last landing settled), if it has.
    pub fn finale(&self) -> Option<f32> {
        self.finale
    }
    /// Seconds since the splash opened.
    pub fn time(&self) -> f32 {
        self.t
    }
    /// The apex of the leap, when the pool turns into the window.
    pub fn apex(&self) -> Option<f32> {
        self.finale.map(|f| f + CROUCH + LEAP)
    }
    /// Whether the game window should take over now.
    pub fn handover(&self) -> bool {
        if self.look.still {
            return self.finale.is_some();
        }
        self.apex().is_some_and(|a| self.t >= a + HANDOVER)
    }

    /// Advances by `dt` seconds with the loading at `progress` (0..1) and
    /// whether it finished.
    pub fn step(&mut self, dt: f32, progress: f32, loaded: bool) {
        self.t += dt.clamp(0.0, 0.1);
        self.loaded |= loaded;
        let due = if self.loaded {
            self.total()
        } else {
            ((progress.clamp(0.0, 1.0) * self.total() as f32).floor() as usize).min(self.total())
        };
        let gap = match (self.look.still, self.loaded) {
            (true, _) => 0.0,
            (false, true) => HURRY_GAP,
            (false, false) => LAUNCH_GAP,
        };
        while self.drops.len() < due && self.t - self.last_launch >= gap {
            let flight = match (self.look.still, self.loaded) {
                (true, _) => 0.0,
                (false, true) => HURRY_FLIGHT + self.random() * 0.15,
                (false, false) => FLIGHT + self.random() * FLIGHT_SPREAD,
            };
            let edge = (self.random() * 4.0) as u8 % 4;
            let along = self.random();
            let radius = 8.0 + self.random() * 7.0;
            let bend = (self.random() - 0.5) * 0.9;
            self.drops.push(Drop {
                launched: self.t,
                flight,
                edge,
                along,
                radius,
                bend,
            });
            self.last_launch = self.t;
            if !self.look.still {
                break;
            }
        }
        // The pool's spring: each landing kicks it out; it settles toward
        // a size that grows fast with the first drops.
        let landed = self.landed();
        if landed > self.pool_landed {
            let kick = (landed - self.pool_landed) as f32 * 95.0 * self.k
                / (1.0 + self.pool_landed as f32 * 0.12);
            self.pool.1 += kick;
            self.pool_landed = landed;
        }
        let p = landed as f32 / self.total() as f32;
        let target = if landed > 0 {
            self.k * (8.0 + 60.0 * (1.0 - (-3.4 * p).exp()) / (1.0 - (-3.4f32).exp()))
        } else {
            0.0
        }
        .max(self.pool_floor);
        let d = dt.clamp(0.0, 0.05);
        if self.look.still {
            self.pool = (target, 0.0);
        } else {
            self.pool.1 += (110.0 * (target - self.pool.0) - 10.0 * self.pool.1) * d;
            self.pool.0 = (self.pool.0 + self.pool.1 * d).max(0.0);
        }
        if self.finale.is_none()
            && self.loaded
            && self.drops.len() == self.total()
            && landed == self.total()
        {
            let last = self
                .drops
                .iter()
                .map(|d| d.launched + d.flight)
                .fold(0.0, f32::max);
            let start = if self.look.still {
                last
            } else {
                (last + SETTLE).max(MIN_BEFORE_FINALE)
            };
            if self.t >= start {
                self.finale = Some(self.t);
            }
        }
    }

    fn edge_point(&self, d: &Drop) -> (f32, f32) {
        let m = 24.0 * self.k;
        let (w, h) = (self.width, self.height);
        match d.edge {
            0 => (d.along * w, -m),
            1 => (w + m, d.along * h),
            2 => (d.along * w, h + m),
            _ => (-m, d.along * h),
        }
    }

    /// The window rectangle's size: the game window, within the screen.
    fn window_size(&self) -> (f32, f32) {
        (
            self.window.0.min(self.width),
            self.window.1.min(self.height),
        )
    }

    pub fn frame(&mut self) -> Frame {
        let t = self.t;
        let k = self.k;
        let (cx, cy) = self.centre();
        let mut balls = Vec::new();
        let pool = self.pool.0;
        if pool > 0.5 && self.look.still {
            balls.push([cx, cy, pool]);
        } else if pool > 0.5 {
            for i in 0..5 {
                let fi = i as f32;
                let a = t * (0.7 + fi * 0.13) + fi * 1.3;
                let wob = pool * 0.32;
                balls.push([
                    cx + a.cos() * wob,
                    cy + (a * 1.2).sin() * wob * 0.6,
                    pool * (0.78 - fi * 0.06),
                ]);
            }
        }
        let landed = self.landed();
        for d in &self.drops {
            let u = (t - d.launched) / d.flight;
            if !(0.0..1.0).contains(&u) || balls.len() >= MAX_BALLS {
                continue;
            }
            let start = self.edge_point(d);
            let e = u.clamp(0.0, 1.0).powf(2.2);
            let (px, py) = (start.0 + (cx - start.0) * e, start.1 + (cy - start.1) * e);
            let (nx, ny) = (-(cy - start.1), cx - start.0);
            let nl = nx.hypot(ny).max(1.0);
            let bend = (std::f32::consts::PI * e).sin() * d.bend * 90.0 * k;
            balls.push([
                px + nx / nl * bend,
                py + ny / nl * bend,
                d.radius * k * (1.0 - 0.35 * e),
            ]);
        }

        // The window morph, from the apex of the leap.
        let finale = self.finale.unwrap_or(f32::INFINITY);
        let apex = finale + CROUCH + LEAP;
        let morph = ease((t - apex) / MORPH);
        let (rw, rh) = self.window_size();
        let since = (t - apex).max(0.0);
        let s0 = ((pool * 2.2) / rw).min(1.0);
        let pop = if t > apex {
            1.0 - (1.0 - s0) * (-6.5 * since).exp() * (12.0 * since).cos()
        } else {
            s0
        };
        let rect = [cx, cy, rw * pop, rh * pop];
        let window = ease((t - apex - 0.15) / 0.3);

        let n = self.total();
        let shown = landed.min(n);
        let progress = shown as f32 / n as f32;
        let name = self
            .names
            .get(shown.saturating_sub(1))
            .cloned()
            .unwrap_or_default();
        let (text, progress) = if let Some((text, progress)) = &self.status {
            (text.clone(), *progress)
        } else if shown >= n {
            ("Loaded · opening the game".to_owned(), progress)
        } else if self.mods {
            (format!("Loading mods {shown} / {n} · {name}"), progress)
        } else {
            // The stages say it themselves ("Loading assets").
            (name, progress)
        };
        let text_alpha = if t > finale {
            (1.0 - (t - finale) * 3.0).max(0.0)
        } else {
            1.0
        };
        let text_y = cy + pool.max(40.0 * k) + 64.0 * k;

        let fox = if self.look.still {
            None
        } else {
            self.fox(t, pool, finale, rh)
        };
        Frame {
            balls,
            morph,
            rect,
            window,
            fox,
            text,
            text_alpha,
            text_y,
            progress,
            pool,
        }
    }

    fn fox(&mut self, t: f32, pool: f32, finale: f32, rh: f32) -> Option<Fox> {
        let k = self.k;
        let (cx, cy) = self.centre();
        let lap = self.look.lap_speed;
        let orbit = pool.max(40.0 * k) + 78.0 * k;
        let (rx, ry) = (orbit, orbit * 0.42);
        let oy = cy + pool.max(40.0 * k) * 0.25;
        // Pixels per block: the prototype's fox, about 125 pixels long at
        // size 1.35 on a 1080p screen (vanilla's fox is about 1.1 blocks).
        let scale = 58.0 * k * self.look.fox_size;
        let omega = std::f32::consts::TAU / 2.6 * lap;
        let run = t * 13.0 * (0.6 + 0.4 * lap) / std::f32::consts::PI;
        let base = Fox {
            x: 0.0,
            y: 0.0,
            scale,
            heading: 0.0,
            pitch: 0.0,
            run,
            speed: 1.0,
            crouch: 0.0,
            age: t * 20.0,
            behind: false,
        };
        // Its run in and lap from when it came.
        let since = t - self.fox_from;
        if since < FOX_IN.0 {
            return None;
        }
        if since < FOX_IN.1 {
            let u = ease((since - FOX_IN.0) / (FOX_IN.1 - FOX_IN.0));
            let x0 = -70.0 * k;
            let fox = Fox {
                x: x0 + (cx - x0) * u,
                y: oy + ry,
                heading: 0.0,
                ..base
            };
            self.fox_at = (fox.x, fox.y);
            return Some(fox);
        }
        if t < finale {
            // Anticlockwise on screen: rightward along the near side.
            let th = std::f32::consts::FRAC_PI_2 - (since - FOX_IN.1) * omega;
            self.lap_phase = th;
            let depth = th.sin();
            let fox = Fox {
                x: cx + th.cos() * rx,
                y: oy + th.sin() * ry,
                // The direction of travel on the ground: d/dt of (cos, sin)·(-ω).
                heading: (-th.cos()).atan2(th.sin()),
                scale: scale * (0.82 + 0.18 * (depth * 0.5 + 0.5)),
                behind: depth < 0.0,
                ..base
            };
            self.fox_at = (fox.x, fox.y);
            return Some(fox);
        }
        // The pounce: crouch where the lap ended, leap over the window's top
        // edge nose up, then dive head-first behind it.
        let (fx, fy) = self.fox_at;
        let face: f32 = if cx >= fx { 1.0 } else { -1.0 };
        let heading = if face > 0.0 {
            -0.45
        } else {
            std::f32::consts::PI + 0.45
        };
        // Over the window's top edge; with a window nearly the screen's
        // height the leap may leave the screen for a moment (chino: fine).
        let top = cy - rh / 2.0 - 46.0 * k;
        let c = (t - finale) / CROUCH;
        let u = (t - finale - CROUCH) / LEAP;
        if c < 1.0 {
            return Some(Fox {
                x: fx,
                y: fy,
                heading,
                speed: 0.0,
                crouch: 3.0 * ease(c / 0.6),
                behind: fy < oy,
                ..base
            });
        }
        if u < 1.0 {
            let e = ease(u);
            return Some(Fox {
                x: fx + (cx - fx) * e,
                y: fy + (top - fy) * (e * std::f32::consts::FRAC_PI_2).sin(),
                heading,
                pitch: 0.55 * (1.0 - u) - 0.25 * u,
                speed: 0.0,
                ..base
            });
        }
        let v = ((u - 1.0) / 0.55).min(1.0);
        if v >= 1.0 {
            return None;
        }
        Some(Fox {
            x: cx + face * 18.0 * k * v,
            y: top + (cy - top) * v * v,
            heading,
            pitch: -0.25 - ease(v * 1.6),
            speed: 0.0,
            behind: true,
            ..base
        })
    }
}

fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(t: &mut Timeline, until: f32, progress: impl Fn(f32) -> (f32, bool)) {
        while t.time() < until {
            let (p, done) = progress(t.time());
            t.step(1.0 / 60.0, p, done);
            t.frame();
        }
    }

    #[test]
    fn a_drop_per_mod_lands_in_the_pool_and_the_finale_follows_the_last() {
        let names: Vec<String> = (0..20).map(|i| format!("mod {i}")).collect();
        let mut t = Timeline::new(
            1920.0,
            1080.0,
            (1280.0, 720.0),
            names,
            true,
            Look::default(),
        );
        // Loading takes 10 s, evenly.
        run(&mut t, 6.0, |s| ((s / 10.0).min(1.0), false));
        let f = t.frame();
        assert!(f.text.starts_with("Loading mods "), "{}", f.text);
        assert!(f.progress > 0.3 && f.progress < 0.6, "{}", f.progress);
        assert!(f.balls.len() >= 5, "the pool and drops in flight");
        assert!(t.finale().is_none());
        run(&mut t, 14.0, |s| ((s / 10.0).min(1.0), s >= 10.0));
        let finale = t.finale().expect("the finale started");
        assert!((10.0..12.0).contains(&finale), "{finale}");
        assert!(t.handover(), "the window takes over after the leap");
        let f = t.frame();
        assert_eq!(f.morph, 1.0);
        assert!((f.rect[2] - 1280.0).abs() < 30.0, "{:?}", f.rect);
        assert_eq!(f.text_alpha, 0.0);
    }

    /// A warm start finishes at once: the drops still come one by one, and
    /// the finale waits for the minimum.
    #[test]
    fn a_fast_start_still_grows_the_pool_drop_by_drop() {
        let names: Vec<String> = (0..30).map(|i| format!("mod {i}")).collect();
        let mut t = Timeline::new(
            1920.0,
            1080.0,
            (1280.0, 720.0),
            names,
            true,
            Look::default(),
        );
        let mut sizes = Vec::new();
        while t.finale().is_none() && t.time() < 20.0 {
            t.step(1.0 / 60.0, 1.0, true);
            let f = t.frame();
            sizes.push(f.pool);
        }
        let finale = t.finale().expect("finale");
        // 30 drops at least HURRY_GAP apart, the last one's flight; never
        // before the minimum, and no later: the game is waiting.
        assert!(
            finale >= MIN_BEFORE_FINALE.max(29.0 * HURRY_GAP + HURRY_FLIGHT),
            "{finale}"
        );
        assert!(finale < MIN_BEFORE_FINALE + 0.1, "{finale}");
        // The pool grew over a few seconds, not in one frame.
        let full = sizes.last().copied().unwrap_or(0.0);
        let half = sizes.iter().position(|&s| s > full / 2.0).unwrap();
        assert!(half > 30, "half-size after {half} frames");
    }

    #[test]
    fn still_has_no_motion_and_hands_over_once_loaded() {
        let look = Look {
            still: true,
            ..Look::default()
        };
        let names: Vec<String> = (0..10).map(|i| format!("mod {i}")).collect();
        let mut t = Timeline::new(1920.0, 1080.0, (1280.0, 720.0), names, true, look);
        run(&mut t, 1.0, |_| (0.5, false));
        let f = t.frame();
        assert_eq!(f.balls.len(), 1, "the pool alone");
        assert!(f.fox.is_none());
        assert!(f.text.contains("5 / 10"), "{}", f.text);
        assert!(!t.handover());
        t.step(1.0 / 60.0, 1.0, true);
        t.step(1.0 / 60.0, 1.0, true);
        assert!(t.handover());
    }

    #[test]
    fn the_fox_laps_behind_the_pool_and_pounces() {
        let mut t = Timeline::new(
            1920.0,
            1080.0,
            (1280.0, 720.0),
            vec!["a".into()],
            true,
            Look::default(),
        );
        let (mut front, mut behind) = (0, 0);
        run(&mut t, 8.0, |_| (0.0, false));
        while t.time() < 12.0 {
            t.step(1.0 / 60.0, 0.0, false);
            if let Some(f) = t.frame().fox {
                if f.behind {
                    behind += 1;
                } else {
                    front += 1;
                }
            }
        }
        assert!(
            front > 60 && behind > 60,
            "{front} in front, {behind} behind"
        );
        run(&mut t, 30.0, |_| (1.0, true));
        assert!(t.frame().fox.is_none(), "the fox dived behind the window");
    }

    #[test]
    fn the_setups_splash_waits_for_its_fox_and_says_its_steps() {
        let names: Vec<String> = (0..30).map(|i| format!("step {i}")).collect();
        let look = Look::default();
        let mut t =
            Timeline::new(1920.0, 1080.0, (1280.0, 720.0), names, false, look).without_fox();
        t.set_status("Preparing · Exporting mob data".into(), 0.4);
        run(&mut t, 10.0, |_| (0.4, false));
        let f = t.frame();
        assert!(f.fox.is_none(), "no fox before its model is prepared");
        assert_eq!(f.text, "Preparing · Exporting mob data");
        assert!((f.progress - 0.4).abs() < 1e-6);
        assert!(f.pool > 0.0, "the drops so far made a pool");
        t.fox_arrives();
        run(&mut t, 13.0, |_| (0.5, false));
        assert!(t.frame().fox.is_some(), "the fox ran in");
        assert!(t.finale().is_none(), "the setup never plays the finale");
    }

    #[test]
    fn the_games_splash_carries_on_from_the_setups() {
        let look = Look::default();
        let mut setup = Timeline::new(
            1920.0,
            1080.0,
            (1280.0, 720.0),
            (0..30).map(|i| format!("step {i}")).collect(),
            false,
            look,
        )
        .without_fox();
        run(&mut setup, 6.0, |_| (1.0, false));
        setup.fox_arrives();
        run(&mut setup, 12.0, |_| (1.0, false));
        let m = setup.moment();
        let before = setup.frame();
        let mods: Vec<String> = (0..7).map(|i| format!("mod {i}")).collect();
        let mut game = Timeline::new(1920.0, 1080.0, (1280.0, 720.0), mods, true, look).resume(m);
        let after = game.frame();
        let (a, b) = (before.fox.unwrap(), after.fox.unwrap());
        assert!(
            (a.x - b.x).abs() < 1.0 && (a.y - b.y).abs() < 1.0,
            "the fox goes on where it was"
        );
        assert!(
            (after.pool - before.pool).abs() < 1e-3,
            "the pool keeps its size"
        );
        // Before the game's own drops land, the pool doesn't shrink.
        run(&mut game, m.t + 0.5, |_| (0.0, false));
        assert!(game.frame().pool > before.pool * 0.9);
        // And the game's finale still comes.
        run(&mut game, m.t + 20.0, |_| (1.0, true));
        assert!(game.handover());
    }
}
