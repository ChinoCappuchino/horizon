//! Renders frames of the startup splash offscreen, as PNGs over a dark
//! desktop, for checking its look without opening windows (fork
//! `NeuHorizon`):
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
    let mut replay: Option<(PathBuf, PathBuf)> = None;
    while let Some(a) = args.next() {
        let mut next = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--mods" => mods = next()?.parse()?,
            "--load" => load = next()?.parse()?,
            "--size" => size = (next()?.parse()?, next()?.parse()?),
            "--at" => at.push(next()?.parse::<f32>()?),
            // Foxes at four headings, nothing else: for checking the model's turn.
            "--fox-test" => at.push(-1.0),
            // A live run's trace (HORIZON_SPLASH_TRACE): every frame, over a
            // background image (a screenshot of the desktop).
            "--replay" => replay = Some((PathBuf::from(next()?), PathBuf::from(next()?))),
            _ => return Err(format!("unknown argument {a}").into()),
        }
    }
    std::fs::create_dir_all(&out)?;
    let assets = Assets::load(&root)?;
    if let Some((trace, background)) = replay {
        return replay_trace(&assets, &trace, &background, &out);
    }
    if at.is_empty() {
        at = vec![1.0, 3.0, 5.0, 8.5, 9.6, 10.2, 10.6, 11.2];
    }

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
            for (i, heading) in [
                0.0f32,
                std::f32::consts::FRAC_PI_2,
                std::f32::consts::PI,
                -std::f32::consts::FRAC_PI_2,
            ]
            .into_iter()
            .enumerate()
            {
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

/// Every frame of a live run, from its trace, composited over `background`
/// (scaled to the run's screen): `out/frame-00000.png`...
fn replay_trace(
    assets: &Assets,
    trace: &std::path::Path,
    background: &std::path::Path,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(trace)?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().ok_or("empty trace")?.split(' ').collect();
    let num = |i: usize| -> Result<f32, Box<dyn std::error::Error>> { Ok(head[i].parse()?) };
    let (w, h, ww, wh) = (num(1)?, num(2)?, num(4)?, num(5)?);
    let still = head[7] == "true";
    let mods = head[9] == "true";
    let names: Vec<String> = lines
        .next()
        .unwrap_or("")
        .split('\t')
        .map(str::to_owned)
        .collect();
    // Half the screen's pixels, the same layout; one frame per 1/60 s of
    // the run.
    let size = ((w / 2.0) as u32, (h / 2.0) as u32);
    let mut gpu = Gpu::offscreen(size, 0.5, assets)?;
    let mut due = 0.0f32;
    let mut written = 0;
    let look = Look {
        still,
        ..Look::default()
    };
    let mut timeline = Timeline::new(w, h, (ww, wh), names, mods, look);
    let bg = read_rgb(background, size)?;
    for line in lines {
        let v: Vec<f32> = line.split(' ').filter_map(|x| x.parse().ok()).collect();
        if v.len() < 3 {
            continue;
        }
        timeline.step(v[0], v[1], v[2] > 0.5);
        let frame = timeline.frame();
        if timeline.time() < due {
            continue;
        }
        due += 1.0 / 60.0;
        gpu.draw(&frame, &look, assets)?;
        let rgba = gpu.read()?;
        let mut img = Vec::with_capacity(rgba.len() / 4 * 3);
        for (px, b) in rgba.as_chunks::<4>().0.iter().zip(bg.as_chunks::<3>().0) {
            let a = f32::from(px[3]) / 255.0;
            for c in 0..3 {
                img.push((f32::from(px[c]) + f32::from(b[c]) * (1.0 - a)).min(255.0) as u8);
            }
        }
        write_rgb(&img, size, &out.join(format!("frame-{written:05}.png")))?;
        written += 1;
    }
    println!("{written} frames");
    Ok(())
}

/// An image as RGB at `size` (nearest scaling).
fn read_rgb(
    path: &std::path::Path,
    size: (u32, u32),
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("too large")?];
    let info = reader.next_frame(&mut buf)?;
    let ch = info.color_type.samples();
    let mut out = Vec::with_capacity((size.0 * size.1 * 3) as usize);
    for y in 0..size.1 {
        for x in 0..size.0 {
            let sx = (x * info.width / size.0) as usize;
            let sy = (y * info.height / size.1) as usize;
            let i = (sy * info.width as usize + sx) * ch;
            out.extend_from_slice(&buf[i..i + 3]);
        }
    }
    Ok(out)
}

fn write_rgb(
    img: &[u8],
    size: (u32, u32),
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, size.0, size.1);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(img)?;
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
        for (i, px) in rgba.as_chunks::<4>().0.iter().enumerate() {
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
