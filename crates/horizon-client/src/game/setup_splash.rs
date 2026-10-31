//! The startup splash during the setup (fork `NeuHorizon`):
//! `horizon-client --setup-splash [--jar PATH] -- COMMAND ARGS...` runs the
//! setup (installing, preparing the resources, porting the pack, then the
//! game) and shows its progress with the splash: a drop for each step of
//! the way, the step and its latest line under the pool, and the bar.
//!
//! Nothing is prepared yet when it opens, so the font and the fox's texture
//! come from Minecraft's jar (`--jar`); the fox runs in once the resources
//! are prepared (its model is the game's own, exported with them). The
//! setup says where it is with `== N/M title ==` lines. The game it ends in
//! opens its own splash where this one is (`moment`, through the file
//! `HORIZON_SPLASH_RESUME` names), and this one closes.
use super::splash_window::SplashRun;
use horizon_client::splash::{gpu::Assets, timeline::Moment};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::WindowId,
};

const RESUME: &str = "HORIZON_SPLASH_RESUME";
/// Drops over the whole setup.
const DROPS: usize = 30;
/// The longest line under the pool, in characters.
const LINE: usize = 96;
/// A failed setup's message stays this long.
const FAILED_FOR: Duration = Duration::from_secs(8);

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Where the setup's splash is, when this game was started under one.
pub(super) fn moment() -> Option<Moment> {
    let text = std::fs::read_to_string(std::env::var_os(RESUME)?).ok()?;
    let mut words = text.split_whitespace().map(|w| w.parse::<f64>().ok());
    let opened = words.next()??;
    let fox_from = words.next()?? as f32;
    let pool = words.next()?? as f32;
    Some(Moment {
        t: (unix_now() - opened) as f32,
        fox_from,
        pool,
    })
}

enum Event {
    Line(String),
    Exited(i32),
}

/// Runs the setup under the splash; its exit code.
pub fn run(args: &[String]) -> i32 {
    let mut jar = None;
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--jar" => jar = rest.next().map(PathBuf::from),
            "--" => break,
            other => {
                eprintln!("Horizon: --setup-splash: unknown option {other}");
                return 2;
            }
        }
    }
    let command: Vec<&String> = rest.collect();
    let Some((program, program_args)) = command.split_first() else {
        eprintln!("Horizon: --setup-splash needs a command after --");
        return 2;
    };
    let resume = std::env::temp_dir().join(format!("horizon-splash-{}.txt", std::process::id()));
    let mut child = match Command::new(program)
        .args(program_args)
        .env(RESUME, &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Horizon: can't run {program}: {e}");
            return 1;
        }
    };
    let (tx, rx) = mpsc::channel();
    // Its output still reaches ours (the launcher's console shows it).
    fn relay(
        from: impl std::io::Read + Send + 'static,
        mut to: impl Write + Send + 'static,
        tx: mpsc::Sender<Event>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut from = std::io::BufReader::new(from);
            let mut line = Vec::new();
            while from.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                let _ = to.write_all(&line);
                let _ = to.flush();
                let _ = tx.send(Event::Line(
                    String::from_utf8_lossy(&line).trim().to_owned(),
                ));
                line.clear();
            }
        })
    }
    let relays = [
        relay(child.stdout.take().unwrap(), std::io::stdout(), tx.clone()),
        relay(child.stderr.take().unwrap(), std::io::stderr(), tx.clone()),
    ];
    std::thread::spawn(move || {
        let code = child.wait().map_or(1, |s| s.code().unwrap_or(1));
        let _ = tx.send(Event::Exited(code));
    });

    let mut app = Setup {
        rx,
        assets: jar.as_deref().and_then(|jar| match Assets::from_jar(jar) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("Horizon: no setup splash (Minecraft's jar: {e})");
                None
            }
        }),
        splash: None,
        resume: resume.clone(),
        written: None,
        stage: None,
        line: String::new(),
        exited: None,
        failed: None,
    };
    if app.assets.is_some() {
        match EventLoop::new() {
            Ok(event_loop) => {
                if let Err(e) = event_loop.run_app(&mut app) {
                    eprintln!("Horizon: the setup splash stopped: {e}");
                }
            }
            Err(e) => eprintln!("Horizon: no setup splash: {e}"),
        }
    }
    // The splash is gone (or never came): the setup's output until it ends.
    let code = loop {
        if let Some(code) = app.exited {
            break code;
        }
        match app.rx.recv() {
            Ok(Event::Exited(code)) => break code,
            Ok(Event::Line(_)) => {}
            Err(_) => break 1,
        }
    };
    for r in relays {
        let _ = r.join();
    }
    let _ = std::fs::remove_file(&resume);
    code
}

struct Setup {
    rx: mpsc::Receiver<Event>,
    assets: Option<Assets>,
    splash: Option<SplashRun>,
    resume: PathBuf,
    written: Option<Instant>,
    /// The step: its number, the steps' count, its title, since when.
    stage: Option<(usize, usize, String, Instant)>,
    /// The setup's latest line.
    line: String,
    exited: Option<i32>,
    failed: Option<Instant>,
}

impl Setup {
    /// Takes in the setup's lines; whether the splash should close (the
    /// game opened its own).
    fn take_lines(&mut self) -> bool {
        let mut close = false;
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Exited(code) => self.exited = Some(code),
                Event::Line(line) => {
                    if let Some(stage) = step_line(&line) {
                        self.stage = Some(stage);
                        self.line.clear();
                    } else if let Some(root) =
                        line.strip_prefix("Prepared Horizon native-client resources: ")
                    {
                        if let Some(s) = &mut self.splash {
                            s.add_fox(Path::new(root.trim()));
                        }
                    } else if line.contains("Horizon: startup splash shown")
                        || line.contains("Horizon: no startup splash")
                        || line.contains("Horizon: the game window opened")
                    {
                        close = true;
                    } else if !line.is_empty() && !line.starts_with("Horizon") {
                        self.line = line;
                    }
                }
            }
        }
        close
    }

    /// The line under the pool and the bar.
    fn status(&self) -> (String, f32) {
        let Some((n, of, title, since)) = &self.stage else {
            return ("Setting up".to_owned(), 0.0);
        };
        // Within a step: its latest line's count ("mod 3/7"), else on
        // toward its end with time, never quite there.
        let within = count_in(&self.line)
            .unwrap_or_else(|| 0.92 * (1.0 - (-since.elapsed().as_secs_f32() / 50.0).exp()));
        let progress = ((*n as f32 - 1.0 + within) / (*of).max(1) as f32).clamp(0.0, 1.0);
        let mut text = if self.line.is_empty() {
            title.clone()
        } else {
            format!("{title} · {}", self.line)
        };
        if let Some(failed) = self.exited.filter(|c| *c != 0) {
            text = format!(
                "The setup failed (exit {failed}, see the log) · {}",
                self.line
            );
        }
        if text.chars().count() > LINE {
            text = text.chars().take(LINE - 3).collect::<String>() + "...";
        }
        (text, progress)
    }

    /// Where the splash is, for the game's (written now and then).
    fn write_moment(&mut self) {
        let Some(s) = &self.splash else { return };
        if self
            .written
            .is_some_and(|w| w.elapsed() < Duration::from_millis(200))
        {
            return;
        }
        self.written = Some(Instant::now());
        let m = s.moment();
        let opened = unix_now() - f64::from(m.t);
        let _ = std::fs::write(
            &self.resume,
            format!("{opened} {} {}\n", m.fox_from, m.pool),
        );
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        self.splash = None;
        event_loop.exit();
    }
}

/// How far a line's count is ("Carrying over mod 3/7: name" is 3/7).
fn count_in(line: &str) -> Option<f32> {
    line.split(|c: char| c.is_whitespace() || c == ':')
        .find_map(|word| {
            let (k, n) = word.split_once('/')?;
            let (k, n) = (k.parse::<u32>().ok()?, n.parse::<u32>().ok()?);
            (n > 0 && k <= n).then(|| k as f32 / n as f32)
        })
}

/// `== 2/5 Preparing the resources ==`: (2, 5, its title).
fn step_line(line: &str) -> Option<(usize, usize, String, Instant)> {
    let inner = line.strip_prefix("== ")?.strip_suffix(" ==")?;
    let (count, title) = inner.split_once(' ')?;
    let (n, of) = count.split_once('/')?;
    Some((
        n.parse().ok()?,
        of.parse().ok()?,
        title.to_owned(),
        Instant::now(),
    ))
}

impl ApplicationHandler for Setup {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.splash.is_some() {
            return;
        }
        let Some(assets) = self.assets.take() else {
            event_loop.exit();
            return;
        };
        let names = (1..=DROPS).map(|i| format!("step {i}")).collect();
        match SplashRun::open_with(
            event_loop,
            assets,
            Instant::now(),
            names,
            false,
            false,
            Some((1280.0, 720.0)),
            None,
        ) {
            Ok(s) => self.splash = Some(s),
            Err(e) => {
                eprintln!("Horizon: no setup splash: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, _: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(s) = self.splash.as_mut().filter(|s| s.id() == id)
            && matches!(
                event,
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
            )
        {
            s.resized();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.take_lines() {
            self.close(event_loop);
            return;
        }
        match self.exited {
            // Ended well, or failed with its message shown long enough.
            Some(0) => return self.close(event_loop),
            Some(_) if self.failed.get_or_insert_with(Instant::now).elapsed() > FAILED_FOR => {
                return self.close(event_loop);
            }
            _ => {}
        }
        let (text, progress) = self.status();
        let Some(s) = self.splash.as_mut() else {
            // (Before `resumed` opened it, on some systems.)
            if self.assets.is_none() {
                event_loop.exit();
            }
            return;
        };
        s.set_status(text, progress);
        s.step(progress, false);
        let next = s.next_frame();
        self.write_moment();
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }
}

#[cfg(test)]
mod tests {
    use super::count_in;

    #[test]
    fn a_lines_count_is_its_progress() {
        assert_eq!(count_in("Carrying over mod 3/6: chipped"), Some(0.5));
        assert_eq!(count_in("Mod code 7/7: t_and_t: 0 blocks"), Some(1.0));
        assert_eq!(
            count_in("Exporting mob data (VanillaMobDataExporter)"),
            None
        );
        assert_eq!(count_in("path a/b"), None);
    }
}
