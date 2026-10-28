//! Renders frames of the startup splash offscreen, as PNGs over a dark
//! desktop, for checking its look without opening windows (fork
//! `afterwinter`):
//!
//!     splash_frames <prepared resources> <out dir> [--mods N] [--load SECONDS]
//!         [--size W H] [--at SECONDS]...
//!
//! The loading is simulated: N mods finishing evenly over SECONDS.
use horizon_client::splash::{
    gpu::{Assets, Gpu},
    timeline::{Fox, Frame, Look, Timeline},
};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or("resources")?);
    let out = PathBuf::from(args.next().ok_or("out dir")?);
    let (mut mods, mut load, mut size, mut at) = (24usize, 8.0f32, (1600u32, 900u32), Vec::new());
    while let Some(a) = args.next() {
        let mut next = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--mods" => mods = next()?.parse()?,
            "--load" => load = next()?.parse()?,
            "--size" => size = (next()?.parse()?, next()?.parse()?),
            "--at" => at.push(next()?.parse::<f32>()?),
            // Foxes at four headings, nothing else: for checking the model's turn.
            "--fox-test" => at.push(-1.0),
            _ => return Err(format!("unknown argument {a}").into()),
        }
    }
    if at.is_empty() {
        at = vec![1.0, 3.0, 5.0, 8.5, 9.6, 10.2, 10.6, 11.2];
    }
    std::fs::create_dir_all(&out)?;
    let assets = Assets::load(&root)?;
    let mut gpu = Gpu::offscreen(size, 1.0, &assets)?;
    let look = Look::default();
    let names = (1..=mods).map(|i| format!("Mod number {i}")).collect();
    let (w, h) = gpu.logical_size();
    let mut timeline = Timeline::new(
        w,
        h,
        (1280.0 * w / 1920.0, 720.0 * w / 1920.0),
        names,
        true,
        look,
    );
    at.sort_by(f32::total_cmp);
    let dt = 1.0 / 60.0;
    for target in at {
        if target < 0.0 {
            for (i, heading) in [0.0f32, 1.5708, 3.1416, -1.5708].into_iter().enumerate() {
                let frame = Frame {
                    balls: Vec::new(),
                    morph: 0.0,
                    rect: [0.0; 4],
                    window: 0.0,
                    fox: Some(Fox {
                        x: w / 2.0,
                        y: h * 0.7,
                        scale: 300.0,
                        heading,
                        pitch: 0.0,
                        run: 0.0,
                        speed: 0.0,
                        crouch: 0.0,
                        age: 0.0,
                        behind: false,
                    }),
                    text: String::new(),
                    text_alpha: 0.0,
                    text_y: 0.0,
                    progress: 0.0,
                    pool: 0.0,
                };
                gpu.draw(&frame, &look, &assets)?;
                save(&gpu.read()?, size, &out.join(format!("fox-{i}.png")))?;
            }
            continue;
        }
        while timeline.time() < target {
            let t = timeline.time();
            timeline.step(dt, (t / load).min(1.0), t >= load);
            timeline.frame();
        }
        let frame = timeline.frame();
        gpu.draw(&frame, &look, &assets)?;
        let path = out.join(format!("splash-{:05.2}.png", target));
        save(&gpu.read()?, size, &path)?;
        println!("{}  ({})", path.display(), frame.text);
    }
    Ok(())
}

fn save(
    rgba: &[u8],
    size: (u32, u32),
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    {
        // Over a dark blue desktop gradient.
        let mut img = Vec::with_capacity(rgba.len() / 4 * 3);
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            let y = (i as u32 / size.0) as f32 / size.1 as f32;
            let bg = [18.0 + 20.0 * y, 28.0 + 24.0 * y, 44.0 + 30.0 * y];
            let a = f32::from(px[3]) / 255.0;
            for c in 0..3 {
                img.push((f32::from(px[c]) + bg[c] * (1.0 - a)).min(255.0) as u8);
            }
        }
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut enc = png::Encoder::new(file, size.0, size.1);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&img)?;
    }
    Ok(())
}
