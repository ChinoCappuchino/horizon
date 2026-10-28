//! The startup splash's window (fork `afterwinter`; `horizon_client::splash`):
//! transparent, undecorated and click-through over the whole screen, on top
//! until the game window takes over where the splash's pool turned into it.
use horizon_client::splash::{
    gpu::{Assets, Gpu},
    timeline::{Look, Timeline},
};
use std::{path::Path, sync::Arc, time::Instant};
use winit::{
    dpi::PhysicalPosition,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId, WindowLevel},
};

pub(super) struct SplashRun {
    window: Arc<Window>,
    gpu: Gpu,
    assets: Assets,
    timeline: Timeline,
    look: Look,
    last: Instant,
    /// `HORIZON_SPLASH_TRACE=FILE`: each frame's inputs, for replaying the
    /// run offscreen (`examples/splash_frames.rs --replay`).
    trace: Option<std::io::BufWriter<std::fs::File>>,
    /// The splash is the game window's own rectangle (no transparency here).
    fills: bool,
}

impl SplashRun {
    /// Opens the splash, or says why it can't (the game then opens its window
    /// as without one). `game_window`: the game window's size, logical
    /// pixels; `names`: one drop each, mods (`mods`) or loading stages.
    pub(super) fn open(
        event_loop: &ActiveEventLoop,
        resources: &Path,
        names: Vec<String>,
        mods: bool,
        still: bool,
        game_window: Option<(f64, f64)>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let opening = Instant::now();
        let assets = Assets::load(resources)?;
        let assets_ms = opening.elapsed().as_millis();
        let monitor = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next());
        let base = |transparent: bool| {
            let attributes = Window::default_attributes()
                .with_title("Horizon")
                .with_transparent(transparent)
                .with_decorations(false)
                .with_resizable(false);
            #[cfg(target_os = "linux")]
            let attributes = winit::platform::wayland::WindowAttributesExtWayland::with_name(
                attributes,
                "horizon",
                "horizon-splash",
            );
            attributes
        };
        // Over the desktop: a transparent window over the whole screen, on
        // top until the game window takes over. Wayland places windows
        // itself: full screen there; elsewhere the monitor's own rectangle
        // (macOS's full screen would move to a Space of its own).
        let over = || -> Result<(Arc<Window>, Gpu), Box<dyn std::error::Error>> {
            let mut attributes = base(true).with_window_level(WindowLevel::AlwaysOnTop);
            if cfg!(target_os = "linux") {
                attributes = attributes
                    .with_fullscreen(Some(winit::window::Fullscreen::Borderless(monitor.clone())));
            } else if let Some(m) = &monitor {
                attributes = attributes
                    .with_position(m.position())
                    .with_inner_size(m.size());
            }
            let window = Arc::new(event_loop.create_window(attributes)?);
            // Clicks go to the desktop under it.
            if let Err(e) = window.set_cursor_hittest(false) {
                log::info!("Horizon: the startup splash can't let clicks through here: {e}");
            }
            let gpu = Gpu::for_window(window.clone(), &assets, true)?;
            Ok((window, gpu))
        };
        // HORIZON_SPLASH_OPAQUE=1: the fallback, for trying it on a system
        // that has transparency.
        let forced = std::env::var_os("HORIZON_SPLASH_OPAQUE").is_some();
        let attempt = if forced {
            Err("HORIZON_SPLASH_OPAQUE is set".into())
        } else {
            over()
        };
        let (window, gpu, fills) = match attempt {
            Ok((window, gpu)) => (window, gpu, false),
            Err(e) => {
                // Any system: the splash in the game window's own place,
                // over a dark backdrop; the pool becomes that very window.
                log::info!(
                    "Horizon: the startup splash can't be drawn over the desktop here ({e}): \
                     it plays where the game window opens"
                );
                let (gw, gh) = game_window.unwrap_or((1280.0, 720.0));
                let mut attributes =
                    base(false).with_inner_size(winit::dpi::LogicalSize::new(gw, gh));
                if let Some(m) = &monitor {
                    let scale = m.scale_factor();
                    let (mw, mh) = (f64::from(m.size().width), f64::from(m.size().height));
                    attributes = attributes.with_position(PhysicalPosition::new(
                        m.position().x + ((mw - gw * scale) / 2.0).max(0.0) as i32,
                        m.position().y + ((mh - gh * scale) / 2.0).max(0.0) as i32,
                    ));
                }
                let window = Arc::new(event_loop.create_window(attributes)?);
                let gpu = Gpu::for_window(window.clone(), &assets, false)?;
                (window, gpu, true)
            }
        };
        let window_ms = opening.elapsed().as_millis();
        let (w, h) = gpu.logical_size();
        let look = Look {
            still,
            ..Look::default()
        };
        // The game window's rectangle: its size, or the screen when the game
        // starts full screen.
        let rect = if fills {
            (w, h)
        } else {
            game_window.map_or((w, h), |(gw, gh)| (gw as f32, gh as f32))
        };
        let header = format!(
            "size {w} {h} window {} {} still {still} mods {mods}\n{}",
            rect.0,
            rect.1,
            names.join("\t")
        );
        let timeline = Timeline::new(w, h, rect, names, mods, look);
        let timeline = if fills { timeline.centred() } else { timeline };
        log::info!(
            "Horizon: startup splash shown ({}x{} pixels, {:?}, {}; assets {assets_ms} ms, window {window_ms} ms, device {} ms)",
            w,
            h,
            gpu.format(),
            if gpu.transparent() {
                "over the desktop"
            } else {
                "in the game window's place"
            },
            opening.elapsed().as_millis()
        );
        let trace = std::env::var_os("HORIZON_SPLASH_TRACE").and_then(|path| {
            use std::io::Write;
            let mut f = std::io::BufWriter::new(std::fs::File::create(path).ok()?);
            let _ = writeln!(f, "{header}");
            Some(f)
        });
        Ok(SplashRun {
            window,
            gpu,
            assets,
            timeline,
            look,
            last: Instant::now(),
            trace,
            fills,
        })
    }

    /// When the next frame is due: one per refresh of the splash's screen
    /// (60 a second when it doesn't say), at most 240.
    pub(super) fn next_frame(&self) -> Instant {
        let hz = self
            .window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            .map_or(60.0, |mhz| f64::from(mhz) / 1000.0)
            .clamp(30.0, 240.0);
        self.last + std::time::Duration::from_secs_f64(1.0 / hz)
    }

    pub(super) fn id(&self) -> WindowId {
        self.window.id()
    }

    pub(super) fn resized(&mut self) {
        let size = self.window.inner_size();
        self.gpu
            .resize((size.width, size.height), self.window.scale_factor() as f32);
    }

    /// Advances and draws one frame with the loading at `progress` and
    /// whether the title is ready; true when the game window should open.
    pub(super) fn step(&mut self, progress: f32, ready: bool) -> bool {
        // One frame per refresh: Mailbox doesn't wait, and the event loop is
        // woken more often than that.
        let due = self.next_frame();
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32();
        self.last = now;
        self.timeline.step(dt, progress, ready);
        let frame = self.timeline.frame();
        let drawing = Instant::now();
        if let Err(e) = self.gpu.draw(&frame, &self.look, &self.assets) {
            log::warn!("Horizon: startup splash frame failed: {e}");
        }
        if let Some(t) = &mut self.trace {
            use std::io::Write;
            let _ = writeln!(
                t,
                "{dt} {progress} {} {}",
                u8::from(ready),
                drawing.elapsed().as_secs_f32()
            );
        }
        self.timeline.handover()
    }

    /// Where the game window's top-left corner goes: the pool's rectangle on
    /// the screen (compositors that place windows themselves ignore it).
    pub(super) fn game_position(&self, size: (f64, f64)) -> Option<PhysicalPosition<i32>> {
        let origin = self.window.outer_position().ok()?;
        if self.fills {
            return Some(origin);
        }
        let scale = self.window.scale_factor();
        let (cx, cy) = self.timeline.centre();
        let left = f64::from(cx) - size.0 / 2.0;
        let top = f64::from(cy) - size.1 / 2.0;
        Some(PhysicalPosition::new(
            origin.x + (left * scale).round() as i32,
            origin.y + (top * scale).round() as i32,
        ))
    }
}

/// The names of the mods that load, for the splash's drops: those turned on
/// and those in development.
pub(super) fn mod_names(
    enabled: &std::collections::BTreeSet<String>,
    dev: &[std::path::PathBuf],
) -> Vec<String> {
    #[cfg(feature = "mods")]
    {
        let here = std::env::current_dir().unwrap_or_default();
        let mut paths = horizon_mods::find(&here.join(horizon_client::mod_list::DIR));
        paths.extend(dev.iter().cloned());
        let mut names: Vec<String> = paths
            .iter()
            .filter_map(|p| Some((p, horizon_mods::Package::read(p).ok()?.manifest)))
            .filter(|(p, m)| enabled.contains(&m.id) || dev.contains(p))
            .map(|(_, m)| m.name)
            .collect();
        names.sort();
        names.dedup();
        names
    }
    #[cfg(not(feature = "mods"))]
    {
        let _ = (enabled, dev);
        Vec::new()
    }
}
