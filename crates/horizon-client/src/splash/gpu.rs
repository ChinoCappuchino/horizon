//! Drawing the startup splash (fork `afterwinter`): its own small wgpu
//! device, a window surface with a transparent background (or an offscreen
//! target for tests and previews), the liquid's signed-distance pass, and
//! quads for the fox, the text and the bar.
//!
//! The fox is the game's own: vanilla's fox model and the snow fox's texture
//! from the player's prepared resources, posed by `FoxModel`'s animation
//! (`mobs::body_anim::fox`): its run, the crouch with the hip wiggle, the
//! pounce.
use super::timeline::{Fox, Frame, Look, MAX_BALLS};
use crate::mobs::{Part, anim, body_anim, render};
use std::{collections::HashMap, path::Path, sync::Arc};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What the splash draws with, from the prepared resources.
pub struct Assets {
    fox: Part,
    fox_texture: Image,
    font: Image,
    /// Character → (column, row, advance in font pixels).
    glyphs: HashMap<char, (u32, u32, f32)>,
}
struct Image {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

fn read_png(path: &Path) -> Result<Image> {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("image too large")?];
    let info = reader.next_frame(&mut buf)?;
    let channels = info.color_type.samples();
    let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
    for px in buf[..info.buffer_size()].chunks_exact(channels) {
        match channels {
            4 => rgba.extend_from_slice(px),
            3 => rgba.extend_from_slice(&[px[0], px[1], px[2], 255]),
            2 => rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]),
            _ => rgba.extend_from_slice(&[px[0], px[0], px[0], 255]),
        }
    }
    Ok(Image {
        rgba,
        width: info.width,
        height: info.height,
    })
}

impl Assets {
    pub fn load(root: &Path) -> Result<Self> {
        let fox = crate::mobs::load_model(root, "minecraft:fox#main").ok_or("no fox model")?;
        let textures = root.join("assets/minecraft/textures");
        let fox_texture = read_png(&textures.join("entity/fox/fox_snow.png"))?;
        let font = read_png(&textures.join("font/ascii.png"))?;
        // The default font's ascii page: which character sits where.
        let mut glyphs = HashMap::new();
        let providers: serde_json::Value = serde_json::from_slice(&std::fs::read(
            root.join("assets/minecraft/font/include/default.json"),
        )?)?;
        let cell = font.width / 16;
        for p in providers["providers"].as_array().into_iter().flatten() {
            if p["file"].as_str() != Some("minecraft:font/ascii.png") {
                continue;
            }
            for (row, line) in p["chars"].as_array().into_iter().flatten().enumerate() {
                for (col, c) in line.as_str().unwrap_or("").chars().enumerate() {
                    if c == '\0' {
                        continue;
                    }
                    // The advance: the rightmost opaque column, plus one.
                    let mut width = 0;
                    for x in 0..cell {
                        for y in 0..cell {
                            let px = col as u32 * cell + x;
                            let py = row as u32 * cell + y;
                            if font.rgba[((py * font.width + px) * 4 + 3) as usize] > 0 {
                                width = width.max(x + 1);
                            }
                        }
                    }
                    let advance = if c == ' ' { 4.0 } else { width as f32 + 1.0 };
                    glyphs.insert(c, (col as u32, row as u32, advance * 8.0 / cell as f32));
                }
            }
        }
        Ok(Assets {
            fox,
            fox_texture,
            font,
            glyphs,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LiquidUniform {
    res: [f32; 4],
    balls: [[f32; 4]; MAX_BALLS],
    params: [f32; 4],
    rect: [f32; 4],
    edge: [f32; 4],
    deep: [f32; 4],
}

enum Target {
    Window {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        output: Output,
    },
    Offscreen,
}

/// How the scene reaches a window.
#[derive(Clone, Copy)]
enum Output {
    Premultiplied,
    /// Straight alpha (a compositor blending post-multiplied).
    Straight,
    /// No transparency here: over a dark backdrop.
    Opaque,
}

const SCENE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The opaque fallback's backdrop: the game window's own dark face.
const BACKDROP: [f32; 4] = [0.07, 0.11, 0.16, 1.0];

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Target,
    scene: wgpu::Texture,
    /// The scene to the window (none offscreen).
    resolve: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Buffer)>,
    /// Physical size, and physical pixels per logical pixel.
    size: (u32, u32),
    scale: f32,
    depth: wgpu::TextureView,
    liquid: wgpu::RenderPipeline,
    liquid_uniform: wgpu::Buffer,
    liquid_group: wgpu::BindGroup,
    model: wgpu::RenderPipeline,
    overlay: wgpu::RenderPipeline,
    view_uniform: wgpu::Buffer,
    view_group: wgpu::BindGroup,
    fox_group: wgpu::BindGroup,
    font_group: wgpu::BindGroup,
}

const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

impl Gpu {
    /// A surface for `window`. `transparent`: drawn over the desktop (the
    /// window was made transparent), else over a dark backdrop. Errs when a
    /// transparent surface isn't offered here: on Windows only DirectX 12
    /// through DirectComposition offers one, on macOS Metal, elsewhere
    /// Vulkan where the compositor does.
    pub fn for_window(
        window: Arc<winit::window::Window>,
        assets: &Assets,
        transparent: bool,
    ) -> Result<Self> {
        let started = std::time::Instant::now();
        // The splash needs no validation or debug layers (they made opening
        // its device take seconds); OpenGL only when nothing else opens.
        let backends: &[wgpu::Backends] = if transparent && cfg!(windows) {
            &[wgpu::Backends::DX12]
        } else if transparent {
            &[wgpu::Backends::PRIMARY]
        } else {
            &[wgpu::Backends::PRIMARY, wgpu::Backends::GL]
        };
        let mut opened = None;
        let mut failures = Vec::new();
        for &backends in backends {
            let mut descriptor = crate::gfx::instance_descriptor(backends);
            descriptor.flags = wgpu::InstanceFlags::empty();
            descriptor.backend_options.dx12.presentation_system =
                wgpu::Dx12SwapchainKind::DxgiFromVisual;
            let instance = wgpu::Instance::new(descriptor);
            let surface = match instance.create_surface(window.clone()) {
                Ok(s) => s,
                Err(e) => {
                    failures.push(e.to_string());
                    continue;
                }
            };
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })) {
                Ok(adapter) => {
                    opened = Some((surface, adapter));
                    break;
                }
                Err(e) => failures.push(e.to_string()),
            }
        }
        let (surface, adapter) = opened.ok_or_else(|| failures.join("; "))?;
        let adapter_ms = started.elapsed().as_millis();
        let caps = surface.get_capabilities(&adapter);
        let (alpha_mode, output) = if transparent {
            if caps
                .alpha_modes
                .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
            {
                (
                    wgpu::CompositeAlphaMode::PreMultiplied,
                    Output::Premultiplied,
                )
            } else if caps
                .alpha_modes
                .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
            {
                // Metal's "post-multiplied" is a layer that isn't opaque:
                // Core Animation still blends its contents premultiplied.
                let metal = adapter.get_info().backend == wgpu::Backend::Metal;
                (
                    wgpu::CompositeAlphaMode::PostMultiplied,
                    if metal {
                        Output::Premultiplied
                    } else {
                        Output::Straight
                    },
                )
            } else {
                return Err("the window can't be transparent here".into());
            }
        } else {
            let mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
                wgpu::CompositeAlphaMode::Opaque
            } else {
                wgpu::CompositeAlphaMode::Auto
            };
            (mode, Output::Opaque)
        };
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        log::info!(
            "Horizon: startup splash on {:?}: adapter {adapter_ms} ms, device {} ms",
            adapter.get_info().backend,
            started.elapsed().as_millis()
        );
        let physical = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: physical.width.max(1),
            height: physical.height.max(1),
            // Mailbox where offered (as the game's own window): Fifo held a
            // transparent full-screen surface on KWin to 20 frames a second.
            present_mode: if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::Fifo
            },
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::default(),
        };
        surface.configure(&device, &config);
        let size = (config.width, config.height);
        let scene = Self::scene(&device, size, false);
        Ok(Self::build(
            device,
            queue,
            Target::Window {
                surface,
                config,
                output,
            },
            scene,
            size,
            window.scale_factor() as f32,
            assets,
        ))
    }

    /// The splash's own colour target: premultiplied, sRGB values.
    fn scene(device: &wgpu::Device, size: (u32, u32), readable: bool) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Splash scene"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | if readable {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
    }

    /// An offscreen target of `size` physical pixels, for tests and previews.
    pub fn offscreen(size: (u32, u32), scale: f32, assets: &Assets) -> Result<Self> {
        let instance =
            wgpu::Instance::new(crate::gfx::instance_descriptor(wgpu::Backends::PRIMARY));
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        let scene = Self::scene(&device, size, true);
        Ok(Self::build(
            device,
            queue,
            Target::Offscreen,
            scene,
            size,
            scale,
            assets,
        ))
    }

    fn texture(device: &wgpu::Device, queue: &wgpu::Queue, image: &Image) -> wgpu::TextureView {
        let size = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Splash image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &image.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            size,
        );
        texture.create_view(&Default::default())
    }

    fn depth_view(device: &wgpu::Device, size: (u32, u32)) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Splash depth"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default())
    }

    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        target: Target,
        scene: wgpu::Texture,
        size: (u32, u32),
        scale: f32,
        assets: &Assets,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Splash shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("splash.wgsl").into()),
        });
        let uniform_entry = |visibility| wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let blend = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let color = [Some(wgpu::ColorTargetState {
            format: SCENE,
            blend,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        // The liquid.
        let liquid_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Splash liquid bindings"),
            entries: &[uniform_entry(wgpu::ShaderStages::FRAGMENT)],
        });
        let liquid_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splash liquid"),
            size: std::mem::size_of::<LiquidUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let liquid_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Splash liquid"),
            layout: &liquid_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: liquid_uniform.as_entire_binding(),
            }],
        });
        let liquid = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Splash liquid"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Splash liquid"),
                    bind_group_layouts: &[Some(&liquid_layout)],
                    ..Default::default()
                }),
            ),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_full"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_liquid"),
                compilation_options: Default::default(),
                targets: &color,
            }),
            multiview_mask: None,
            cache: None,
        });

        // Quads: the fox (depth tested) and the text and bar (on top).
        let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Splash view bindings"),
            entries: &[uniform_entry(wgpu::ShaderStages::VERTEX)],
        });
        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Splash image bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let view_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splash view"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Splash view"),
            layout: &view_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_uniform.as_entire_binding(),
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Splash nearest"),
            ..Default::default()
        });
        let image_group = |view: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Splash image"),
                layout: &image_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let fox_group = image_group(&Self::texture(&device, &queue, &assets.fox_texture));
        let font_group = image_group(&Self::texture(&device, &queue, &assets.font));
        let quad_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Splash quads"),
            bind_group_layouts: &[Some(&view_layout), Some(&image_layout)],
            ..Default::default()
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4];
        let quads = |label, depth: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&quad_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_quad"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(depth),
                    depth_compare: Some(if depth {
                        wgpu::CompareFunction::LessEqual
                    } else {
                        wgpu::CompareFunction::Always
                    }),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_quad"),
                    compilation_options: Default::default(),
                    targets: &color,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let model = quads("Splash fox", true);
        let overlay = quads("Splash overlay", false);
        let depth = Self::depth_view(&device, size);
        let resolve = match &target {
            Target::Window { config, .. } => {
                Some(Self::resolve_pipeline(&device, &shader, config.format))
            }
            Target::Offscreen => None,
        };
        Gpu {
            device,
            queue,
            target,
            scene,
            resolve,
            size,
            scale,
            depth,
            liquid,
            liquid_uniform,
            liquid_group,
            model,
            overlay,
            view_uniform,
            view_group,
            fox_group,
            font_group,
        }
    }

    /// The target's size in logical pixels.
    pub fn logical_size(&self) -> (f32, f32) {
        (
            self.size.0 as f32 / self.scale,
            self.size.1 as f32 / self.scale,
        )
    }

    pub fn resize(&mut self, physical: (u32, u32), scale: f32) {
        self.scale = scale;
        self.size = (physical.0.max(1), physical.1.max(1));
        let readable = matches!(self.target, Target::Offscreen);
        if let Target::Window {
            surface, config, ..
        } = &mut self.target
        {
            config.width = self.size.0;
            config.height = self.size.1;
            surface.configure(&self.device, config);
        }
        self.scene = Self::scene(&self.device, self.size, readable);
        self.depth = Self::depth_view(&self.device, self.size);
    }

    /// Draws `frame`; on a window, presents it.
    pub fn draw(&mut self, frame: &Frame, look: &Look, assets: &Assets) -> Result<()> {
        let (lw, lh) = self.logical_size();
        let mut balls = [[0.0; 4]; MAX_BALLS];
        for (b, f) in balls.iter_mut().zip(&frame.balls) {
            *b = [f[0], f[1], f[2], 0.0];
        }
        let uniform = LiquidUniform {
            res: [self.scale, self.size.0 as f32, self.size.1 as f32, 0.0],
            balls,
            params: [
                frame.balls.len().min(MAX_BALLS) as f32,
                frame.morph,
                frame.window,
                look.gooeyness,
            ],
            rect: frame.rect,
            edge: [
                look.edge[0],
                look.edge[1],
                look.edge[2],
                look.window_corners,
            ],
            deep: [look.deep[0], look.deep[1], look.deep[2], 0.0],
        };
        self.queue
            .write_buffer(&self.liquid_uniform, 0, bytemuck::bytes_of(&uniform));
        self.queue.write_buffer(
            &self.view_uniform,
            0,
            bytemuck::cast_slice(&[lw, lh, 0.0, 0.0]),
        );
        let mut fox = Vec::new();
        if let Some(f) = &frame.fox {
            fox_vertices(assets, f, &mut fox);
        }
        let mut overlay = Vec::new();
        overlay_vertices(assets, frame, look, (lw, lh), &mut overlay);
        let buffer = |data: &[Vertex]| {
            use wgpu::util::DeviceExt;
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Splash quads"),
                    contents: bytemuck::cast_slice(if data.is_empty() {
                        &[Vertex {
                            pos: [0.0; 3],
                            uv: [0.0; 2],
                            color: [0.0; 4],
                        }]
                    } else {
                        data
                    }),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        };
        let fox_buffer = buffer(&fox);
        let overlay_buffer = buffer(&overlay);

        let surface_texture = match &self.target {
            Target::Window { surface, .. } => match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t)
                | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
                _ => return Ok(()),
            },
            Target::Offscreen { .. } => None,
        };
        let view = self.scene.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let behind = frame.fox.is_some_and(|f| f.behind);
        let fox_count = fox.len() as u32;
        for (i, load) in [true, false, false].into_iter().enumerate() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Splash"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: if load {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            match i {
                // The fox behind the pool, then the liquid, then the fox in
                // front and the text.
                0 | 2 => {
                    if fox_count > 0 && behind == (i == 0) {
                        pass.set_pipeline(&self.model);
                        pass.set_bind_group(0, &self.view_group, &[]);
                        pass.set_bind_group(1, &self.fox_group, &[]);
                        pass.set_vertex_buffer(0, fox_buffer.slice(..));
                        pass.draw(0..fox_count, 0..1);
                    }
                    if i == 2 && !overlay.is_empty() {
                        pass.set_pipeline(&self.overlay);
                        pass.set_bind_group(0, &self.view_group, &[]);
                        pass.set_bind_group(1, &self.font_group, &[]);
                        pass.set_vertex_buffer(0, overlay_buffer.slice(..));
                        pass.draw(0..overlay.len() as u32, 0..1);
                    }
                }
                _ => {
                    pass.set_pipeline(&self.liquid);
                    pass.set_bind_group(0, &self.liquid_group, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }
        // The scene to the window, converted for how it composites.
        if let (Some(t), Some((pipeline, layout, uniform)), Target::Window { config, output, .. }) =
            (&surface_texture, &self.resolve, &self.target)
        {
            let mode = match output {
                Output::Premultiplied => 0.0,
                Output::Straight => 1.0,
                Output::Opaque => 2.0,
            };
            let srgb = if config.format.is_srgb() { 1.0 } else { 0.0 };
            self.queue.write_buffer(
                uniform,
                0,
                bytemuck::cast_slice(&[[mode, srgb, 0.0, 0.0], BACKDROP]),
            );
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Splash resolve"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                ],
            });
            let target = t.texture.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Splash resolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        if let Some(t) = surface_texture {
            self.queue.present(t);
        }
        Ok(())
    }

    fn resolve_pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Buffer) {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Splash resolve bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splash resolve"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Splash resolve"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Splash resolve"),
                    bind_group_layouts: &[Some(&layout)],
                    ..Default::default()
                }),
            ),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_full"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_resolve"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        (pipeline, layout, uniform)
    }

    /// The offscreen target's pixels (RGBA, premultiplied).
    pub fn read(&self) -> Result<Vec<u8>> {
        let Target::Offscreen = &self.target else {
            return Err("not offscreen".into());
        };
        let texture = &self.scene;
        let row = (self.size.0 * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Splash readback"),
            size: u64::from(row * self.size.1),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = sender.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        receiver.recv()??;
        let data = buffer.get_mapped_range(..).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity((self.size.0 * self.size.1 * 4) as usize);
        for y in 0..self.size.1 {
            let start = (y * row) as usize;
            out.extend_from_slice(&data[start..start + (self.size.0 * 4) as usize]);
        }
        Ok(out)
    }

    /// The window's format, or the scene's offscreen.
    pub fn format(&self) -> wgpu::TextureFormat {
        match &self.target {
            Target::Window { config, .. } => config.format,
            Target::Offscreen => SCENE,
        }
    }
    /// Whether the splash is drawn over the desktop (else over its backdrop).
    pub fn transparent(&self) -> bool {
        !matches!(
            self.target,
            Target::Window {
                output: Output::Opaque,
                ..
            }
        )
    }
}

/// The fox's quads: vanilla's fox model in screen space. The view looks
/// down on the ground a little (so the lap's ellipse reads as a circle on the
/// ground), orthographic; nearer is smaller depth.
fn fox_vertices(assets: &Assets, fox: &Fox, out: &mut Vec<Vertex>) {
    let part = &assets.fox;
    let mut poses = anim::Poses::new(part);
    let state = anim::State {
        pos: fox.run,
        speed: fox.speed,
        age: fox.age,
        y_rot: 0.0,
        x_rot: 0.0,
        swing: 0.0,
        baby: false,
        partial: 0.0,
    };
    let pose = body_anim::Fox {
        crouching: fox.crouch > 0.0,
        crouch_amount: fox.crouch,
        ..Default::default()
    };
    body_anim::fox(&mut poses, &state, &pose, &HashMap::new());
    use glam::{Mat4, Vec3};
    const TILT: f32 = 0.42;
    // LivingEntityRenderer: model space to the entity's own (y up, feet at 0,
    // facing +z); then its facing and pitch about the body's middle.
    let entity = Mat4::from_rotation_y(std::f32::consts::PI)
        * Mat4::from_scale(Vec3::new(-1.0, -1.0, 1.0))
        * Mat4::from_translation(Vec3::new(0.0, -1.501, 0.0));
    let turn = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2 - fox.heading)
        * Mat4::from_translation(Vec3::new(0.0, 0.35, 0.0))
        * Mat4::from_rotation_x(-fox.pitch)
        * Mat4::from_translation(Vec3::new(0.0, -0.35, 0.0));
    // The world to the screen: tilt toward the viewer, y down, pixels.
    let screen = Mat4::from_translation(Vec3::new(fox.x, fox.y, 0.0))
        * Mat4::from_scale(Vec3::new(fox.scale, -fox.scale, fox.scale))
        * Mat4::from_rotation_x(TILT);
    let base = screen * turn * entity;
    let mut verts = Vec::new();
    render::draw(
        part,
        base,
        &poses,
        [0.0, 0.0, 1.0, 1.0],
        u32::MAX,
        0,
        0,
        &mut verts,
    );
    let light = Vec3::new(-0.35, -0.8, 0.5).normalize();
    for quad in verts.chunks_exact(4) {
        let n = quad[0].normal;
        let n = Vec3::new(f32::from(n[0]), f32::from(n[1]), f32::from(n[2])) / 127.0;
        let shade = 0.62 + 0.38 * n.dot(light).max(0.0);
        let corner = |v: &crate::mesher::Vertex| {
            let c = v.color.to_le_bytes();
            Vertex {
                // Nearer the viewer (larger z after the tilt) is smaller depth.
                pos: [v.pos[0], v.pos[1], 0.5 - v.pos[2] / 4000.0],
                uv: v.uv,
                color: [
                    f32::from(c[0]) / 255.0 * shade,
                    f32::from(c[1]) / 255.0 * shade,
                    f32::from(c[2]) / 255.0 * shade,
                    1.0,
                ],
            }
        };
        for i in [0, 1, 2, 0, 2, 3] {
            out.push(corner(&quad[i]));
        }
    }
}

/// The stage line and the progress bar under the pool.
fn overlay_vertices(
    assets: &Assets,
    frame: &Frame,
    look: &Look,
    size: (f32, f32),
    out: &mut Vec<Vertex>,
) {
    if frame.text_alpha <= 0.0 {
        return;
    }
    let k = size.0.min(size.1) / 640.0;
    let a = frame.text_alpha;
    let px = (2.0 * k).max(1.0);
    let cx = size.0 / 2.0;
    let chars: Vec<char> = frame
        .text
        .chars()
        .map(|c| if c == '·' { '∙' } else { c })
        .collect();
    let width: f32 = chars
        .iter()
        .map(|c| assets.glyphs.get(c).map_or(4.0, |g| g.2))
        .sum::<f32>()
        * px;
    let y = frame.text_y;
    let cell = 1.0 / 16.0;
    for (shadow, color) in [(px, [0.15, 0.1, 0.1, 0.8 * a]), (0.0, [1.0, 0.97, 0.92, a])] {
        let mut x = cx - width / 2.0;
        for c in &chars {
            let Some(&(col, row, advance)) = assets.glyphs.get(c) else {
                x += 4.0 * px;
                continue;
            };
            let (u, v) = (col as f32 * cell, row as f32 * cell);
            quad(
                out,
                [
                    x + shadow,
                    y + shadow,
                    x + 8.0 * px + shadow,
                    y + 8.0 * px + shadow,
                ],
                [u, v, u + cell, v + cell],
                color,
            );
            x += advance * px;
        }
    }
    let (w, h) = (220.0 * k, (2.0 * k).max(2.0));
    let by = y + 14.0 * px;
    quad(
        out,
        [cx - w / 2.0, by, cx + w / 2.0, by + h],
        [-1.0; 4],
        [1.0, 1.0, 1.0, 0.18 * a],
    );
    let fill = [look.edge[0], look.edge[1], look.edge[2], 0.9 * a];
    quad(
        out,
        [cx - w / 2.0, by, cx - w / 2.0 + w * frame.progress, by + h],
        [-1.0; 4],
        fill,
    );
}

fn quad(out: &mut Vec<Vertex>, r: [f32; 4], uv: [f32; 4], color: [f32; 4]) {
    let v = |x: f32, y: f32, u: f32, w: f32| Vertex {
        pos: [x, y, 0.0],
        uv: [u, w],
        color,
    };
    let (a, b, c, d) = (
        v(r[0], r[1], uv[0], uv[1]),
        v(r[2], r[1], uv[2], uv[1]),
        v(r[2], r[3], uv[2], uv[3]),
        v(r[0], r[3], uv[0], uv[3]),
    );
    out.extend_from_slice(&[a, b, c, a, c, d]);
}
