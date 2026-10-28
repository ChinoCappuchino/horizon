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
        let assets = Assets::load(resources)?;
        let monitor = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next());
        let attributes = Window::default_attributes()
            .with_title("Horizon")
            .with_transparent(true)
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_fullscreen(Some(winit::window::Fullscreen::Borderless(monitor)));
        #[cfg(target_os = "linux")]
        let attributes = winit::platform::wayland::WindowAttributesExtWayland::with_name(
            attributes,
            "horizon",
            "horizon-splash",
        );
        let window = Arc::new(event_loop.create_window(attributes)?);
        // Clicks go to the desktop under it.
        if let Err(e) = window.set_cursor_hittest(false) {
            log::info!("Horizon: the startup splash can't let clicks through here: {e}");
        }
        let gpu = Gpu::for_window(window.clone(), &assets)?;
        let (w, h) = gpu.logical_size();
        let look = Look {
            still,
            ..Look::default()
        };
        // The game window's rectangle: its size, or the screen when the game
        // starts full screen.
        let rect = game_window.map_or((w, h), |(gw, gh)| (gw as f32, gh as f32));
        let timeline = Timeline::new(w, h, rect, names, mods, look);
        log::info!(
            "Horizon: startup splash shown ({}x{} pixels, {:?})",
            w,
            h,
            gpu.format()
        );
        Ok(SplashRun {
            window,
            gpu,
            assets,
            timeline,
            look,
            last: Instant::now(),
        })
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
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32();
        self.last = now;
        self.timeline.step(dt, progress, ready);
        let frame = self.timeline.frame();
        if let Err(e) = self.gpu.draw(&frame, &self.look, &self.assets) {
            log::warn!("Horizon: startup splash frame failed: {e}");
        }
        self.timeline.handover()
    }

    /// Where the game window's top-left corner goes: the pool's rectangle on
    /// the screen (compositors that place windows themselves ignore it).
    pub(super) fn game_position(&self, size: (f64, f64)) -> Option<PhysicalPosition<i32>> {
        let origin = self.window.outer_position().ok()?;
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
