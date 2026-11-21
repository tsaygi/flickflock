//! The image canvas: a custom wgpu render pass embedded in egui.
//!
//! Images live on the GPU as one or more tiles (premultiplied RGBA with
//! mipmaps). Zoom and pan are applied on the CPU by placing tile quads in
//! viewport pixels. Transitions render the outgoing and incoming images into
//! two off-screen textures and blend them with a small WGSL shader.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use ff_core::loader::Prepared;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// `(config name, shader body)` for every transition. The names match
/// `ff_core::config::TRANSITIONS` (minus "none").
const TRANSITION_SOURCES: [(&str, &str); 10] = [
    (
        "crossfade",
        include_str!("shaders/transitions/crossfade.wgsl"),
    ),
    ("slide", include_str!("shaders/transitions/slide.wgsl")),
    ("push", include_str!("shaders/transitions/push.wgsl")),
    ("zoom", include_str!("shaders/transitions/zoom.wgsl")),
    ("wipe", include_str!("shaders/transitions/wipe.wgsl")),
    ("iris", include_str!("shaders/transitions/iris.wgsl")),
    (
        "page_curl",
        include_str!("shaders/transitions/page_curl.wgsl"),
    ),
    ("ripple", include_str!("shaders/transitions/ripple.wgsl")),
    (
        "pixelate",
        include_str!("shaders/transitions/pixelate.wgsl"),
    ),
    (
        "dissolve",
        include_str!("shaders/transitions/dissolve.wgsl"),
    ),
];

/// Index of a transition by its config name.
pub fn transition_index(name: &str) -> Option<usize> {
    TRANSITION_SOURCES.iter().position(|(n, _)| *n == name)
}

const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct GpuTile {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// An image uploaded to the GPU.
pub struct GpuImage {
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    tiles: Vec<GpuTile>,
}

impl GpuImage {
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
}

/// Device handles plus the layout needed to create image textures. Cheap to clone.
#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    tile_layout: Arc<wgpu::BindGroupLayout>,
    /// Largest texture edge the device supports.
    pub max_texture: u32,
}

impl Gpu {
    /// Upload a prepared image (all tiles and mip levels).
    pub fn upload(&self, prepared: &Prepared) -> GpuImage {
        let tiles = prepared
            .tiles
            .iter()
            .map(|t| {
                let (texture, bind_group) =
                    self.create_tile(t.width, t.height, t.mips.len() as u32);
                let (mut w, mut h) = (t.width, t.height);
                for (level, data) in t.mips.iter().enumerate() {
                    self.write(&texture, level as u32, w, h, data);
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                }
                GpuTile {
                    x: t.x,
                    y: t.y,
                    width: t.width,
                    height: t.height,
                    texture,
                    bind_group,
                }
            })
            .collect();
        GpuImage {
            width: prepared.width,
            height: prepared.height,
            bytes: prepared.bytes(),
            tiles,
        }
    }

    /// A single-level texture whose pixels are replaced every animation frame.
    pub fn create_dynamic(&self, width: u32, height: u32) -> GpuImage {
        let (texture, bind_group) = self.create_tile(width, height, 1);
        GpuImage {
            width,
            height,
            bytes: width as usize * height as usize * 4,
            tiles: vec![GpuTile {
                x: 0,
                y: 0,
                width,
                height,
                texture,
                bind_group,
            }],
        }
    }

    /// Replace the pixels of a dynamic image (premultiplied RGBA8).
    pub fn write_frame(&self, image: &GpuImage, rgba: &[u8]) {
        if let Some(tile) = image.tiles.first() {
            if rgba.len() == tile.width as usize * tile.height as usize * 4 {
                self.write(&tile.texture, 0, tile.width, tile.height, rgba);
            }
        }
    }

    fn create_tile(&self, width: u32, height: u32, mips: u32) -> (wgpu::Texture, wgpu::BindGroup) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ff image tile"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mips.max(1),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ff image tile"),
            layout: &self.tile_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        (texture, bind_group)
    }

    fn write(&self, texture: &wgpu::Texture, level: u32, width: u32, height: u32, data: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// One image placed in the viewport.
#[derive(Clone)]
pub struct Layer {
    pub image: Arc<GpuImage>,
    /// Top-left corner and size of the whole image, in viewport pixels.
    pub rect: [f32; 4],
    pub nearest: bool,
    pub checker: bool,
}

/// What the canvas shows this frame.
#[derive(Clone, Default)]
pub struct Scene {
    pub layer: Option<Layer>,
    /// Outgoing image, transition index and progress (0..1) while a transition runs.
    pub transition: Option<(Option<Layer>, usize, f32)>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    viewport: [f32; 2],
    opacity: f32,
    checker: f32,
    checker_size: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransitionParams {
    progress: f32,
    aspect: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    local: [f32; 2],
}

/// A layer with its per-frame GPU buffers.
struct DrawLayer {
    layer: Layer,
    vertices: wgpu::Buffer,
    globals: wgpu::BindGroup,
}

struct Offscreen {
    size: [u32; 2],
    from: wgpu::TextureView,
    to: wgpu::TextureView,
}

enum Frame {
    Empty,
    Direct(DrawLayer),
    Transition {
        bind_group: wgpu::BindGroup,
        index: usize,
    },
}

/// Pipelines and per-frame state, stored in egui-wgpu's callback resources.
pub struct CanvasRenderer {
    image_pipeline: wgpu::RenderPipeline,
    image_pipeline_offscreen: wgpu::RenderPipeline,
    transition_pipelines: Vec<wgpu::RenderPipeline>,
    globals_layout: wgpu::BindGroupLayout,
    transition_layout: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
    clamp_linear: wgpu::Sampler,
    offscreen: Option<Offscreen>,
    frame: Frame,
}

/// Create the renderer, register it with egui-wgpu and return the handles
/// needed to upload images.
pub fn install(render_state: &egui_wgpu::RenderState) -> Gpu {
    let device = &render_state.device;
    let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ff tile layout"),
        entries: &[texture_entry(0)],
    });
    let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ff globals layout"),
        entries: &[
            uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
            sampler_entry(1),
        ],
    });
    let transition_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ff transition layout"),
        entries: &[
            uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
            sampler_entry(1),
            texture_entry(2),
            texture_entry(3),
        ],
    });

    let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ff image shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image.wgsl").into()),
    });
    let image_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ff image pipeline layout"),
        bind_group_layouts: &[Some(&globals_layout), Some(&tile_layout)],
        immediate_size: 0,
    });
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2],
    };
    let make_image_pipeline = |format: wgpu::TextureFormat| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ff image pipeline"),
            layout: Some(&image_layout),
            vertex: wgpu::VertexState {
                module: &image_shader,
                entry_point: Some("vs"),
                buffers: &[Some(vertex_layout.clone())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
                entry_point: Some("fs"),
                targets: &[Some(premultiplied_target(format))],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    };
    let image_pipeline = make_image_pipeline(render_state.target_format);
    let image_pipeline_offscreen = make_image_pipeline(OFFSCREEN_FORMAT);

    let transition_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ff transition pipeline layout"),
            bind_group_layouts: &[Some(&transition_layout)],
            immediate_size: 0,
        });
    let common = include_str!("shaders/transition_common.wgsl");
    let transition_pipelines = TRANSITION_SOURCES
        .iter()
        .map(|(name, body)| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(name),
                source: wgpu::ShaderSource::Wgsl(format!("{common}\n{body}").into()),
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(name),
                layout: Some(&transition_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    targets: &[Some(premultiplied_target(render_state.target_format))],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        })
        .collect();

    let sampler = |filter: wgpu::FilterMode, mip: wgpu::MipmapFilterMode| {
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ff sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: filter,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: mip,
            ..Default::default()
        })
    };
    let renderer = CanvasRenderer {
        image_pipeline,
        image_pipeline_offscreen,
        transition_pipelines,
        globals_layout,
        transition_layout,
        linear: sampler(wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear),
        nearest: sampler(wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Linear),
        clamp_linear: sampler(wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Nearest),
        offscreen: None,
        frame: Frame::Empty,
    };
    render_state
        .renderer
        .write()
        .callback_resources
        .insert(renderer);

    Gpu {
        device: device.clone(),
        queue: render_state.queue.clone(),
        tile_layout: Arc::new(tile_layout),
        max_texture: device.limits().max_texture_dimension_2d,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn premultiplied_target(format: wgpu::TextureFormat) -> wgpu::ColorTargetState {
    wgpu::ColorTargetState {
        format,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    }
}

impl CanvasRenderer {
    fn build_layer(&self, device: &wgpu::Device, layer: &Layer, viewport: [f32; 2]) -> DrawLayer {
        let img = &layer.image;
        let [x0, y0, w, h] = layer.rect;
        let (sx, sy) = (w / img.width.max(1) as f32, h / img.height.max(1) as f32);
        let mut verts = Vec::with_capacity(img.tiles.len() * 6);
        for t in &img.tiles {
            let (lx0, ly0) = (t.x as f32 * sx, t.y as f32 * sy);
            let (lx1, ly1) = ((t.x + t.width) as f32 * sx, (t.y + t.height) as f32 * sy);
            let corner = |lx: f32, ly: f32, u: f32, v: f32| Vertex {
                pos: [x0 + lx, y0 + ly],
                uv: [u, v],
                local: [lx, ly],
            };
            let (a, b, c, d) = (
                corner(lx0, ly0, 0.0, 0.0),
                corner(lx1, ly0, 1.0, 0.0),
                corner(lx1, ly1, 1.0, 1.0),
                corner(lx0, ly1, 0.0, 1.0),
            );
            verts.extend_from_slice(&[a, b, c, a, c, d]);
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ff layer vertices"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let globals = Globals {
            viewport,
            opacity: 1.0,
            checker: if layer.checker { 1.0 } else { 0.0 },
            checker_size: 12.0,
            _pad: [0.0; 3],
        };
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ff layer globals"),
            contents: bytemuck::bytes_of(&globals),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let globals = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ff layer globals"),
            layout: &self.globals_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(if layer.nearest {
                        &self.nearest
                    } else {
                        &self.linear
                    }),
                },
            ],
        });
        DrawLayer {
            layer: layer.clone(),
            vertices,
            globals,
        }
    }

    fn ensure_offscreen(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.offscreen.as_ref().map(|o| o.size) == Some(size) {
            return;
        }
        let make = || {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("ff transition target"),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: OFFSCREEN_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        self.offscreen = Some(Offscreen {
            size,
            from: make(),
            to: make(),
        });
    }
}

fn draw_layer(pass: &mut wgpu::RenderPass<'_>, pipeline: &wgpu::RenderPipeline, layer: &DrawLayer) {
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &layer.globals, &[]);
    pass.set_vertex_buffer(0, layer.vertices.slice(..));
    for (i, tile) in layer.layer.image.tiles.iter().enumerate() {
        pass.set_bind_group(1, &tile.bind_group, &[]);
        let first = i as u32 * 6;
        pass.draw(first..first + 6, 0..1);
    }
}

/// The egui paint callback for one frame of the canvas.
pub struct CanvasCallback {
    pub scene: Scene,
    /// Size of the canvas in physical pixels.
    pub size_px: [u32; 2],
}

impl egui_wgpu::CallbackTrait for CanvasCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(r) = resources.get_mut::<CanvasRenderer>() else {
            return Vec::new();
        };
        let size = [self.size_px[0].max(1), self.size_px[1].max(1)];
        let viewport = [size[0] as f32, size[1] as f32];
        r.frame = match (&self.scene.transition, &self.scene.layer) {
            (Some((from, index, progress)), to) if *index < r.transition_pipelines.len() => {
                r.ensure_offscreen(device, size);
                let from_draw = from.as_ref().map(|l| r.build_layer(device, l, viewport));
                let to_draw = to.as_ref().map(|l| r.build_layer(device, l, viewport));
                let off = r.offscreen.as_ref().expect("created above");
                for (view, layer) in [(&off.from, &from_draw), (&off.to, &to_draw)] {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("ff transition source"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            depth_slice: None,
                            resolve_target: None,
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
                    if let Some(layer) = layer {
                        draw_layer(&mut pass, &r.image_pipeline_offscreen, layer);
                    }
                }
                let params = TransitionParams {
                    progress: *progress,
                    aspect: viewport[0] / viewport[1],
                    _pad: [0.0; 2],
                };
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ff transition params"),
                    contents: bytemuck::bytes_of(&params),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("ff transition"),
                    layout: &r.transition_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&r.clamp_linear),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&off.from),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&off.to),
                        },
                    ],
                });
                Frame::Transition {
                    bind_group,
                    index: *index,
                }
            }
            (_, Some(layer)) => Frame::Direct(r.build_layer(device, layer, viewport)),
            _ => Frame::Empty,
        };
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(r) = resources.get::<CanvasRenderer>() else {
            return;
        };
        match &r.frame {
            Frame::Empty => {}
            Frame::Direct(layer) => draw_layer(pass, &r.image_pipeline, layer),
            Frame::Transition { bind_group, index } => {
                pass.set_pipeline(&r.transition_pipelines[*index]);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }
}

/// Zoom state of the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    /// Fit the image inside the window.
    Fit,
    /// Physical screen pixels per image pixel (1.0 = 100%).
    Scale(f32),
}

pub const MIN_SCALE: f32 = 0.01;
pub const MAX_SCALE: f32 = 64.0;

/// Zoom and (unconstrained) pan of the image in the viewport. All lengths
/// are physical pixels; `pan` is the offset of the image centre from the
/// viewport centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub zoom: Zoom,
    pub pan: [f32; 2],
}

impl Default for View {
    fn default() -> Self {
        Self {
            zoom: Zoom::Fit,
            pan: [0.0, 0.0],
        }
    }
}

impl View {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Scale that fits `image` inside `viewport`. Small images are only
    /// enlarged when `enlarge` is set.
    pub fn fit_scale(image: [f32; 2], viewport: [f32; 2], enlarge: bool) -> f32 {
        if image[0] <= 0.0 || image[1] <= 0.0 {
            return 1.0;
        }
        let s = (viewport[0] / image[0]).min(viewport[1] / image[1]);
        if enlarge {
            s
        } else {
            s.min(1.0)
        }
    }

    pub fn scale(&self, image: [f32; 2], viewport: [f32; 2], enlarge: bool) -> f32 {
        match self.zoom {
            Zoom::Fit => Self::fit_scale(image, viewport, enlarge),
            Zoom::Scale(s) => s,
        }
    }

    /// Image rectangle `[x, y, w, h]` in viewport pixels.
    pub fn image_rect(&self, image: [f32; 2], viewport: [f32; 2], enlarge: bool) -> [f32; 4] {
        let s = self.scale(image, viewport, enlarge);
        let (w, h) = (image[0] * s, image[1] * s);
        // Snap to whole pixels at fit/100% so the image stays crisp.
        let x = ((viewport[0] - w) / 2.0 + self.pan[0]).round();
        let y = ((viewport[1] - h) / 2.0 + self.pan[1]).round();
        [x, y, w, h]
    }

    /// Change the zoom to `new_scale`, keeping the image point under
    /// `anchor` (viewport pixels) fixed on screen.
    pub fn zoom_to(
        &mut self,
        new_scale: f32,
        anchor: [f32; 2],
        image: [f32; 2],
        viewport: [f32; 2],
        enlarge: bool,
    ) {
        let old = self.scale(image, viewport, enlarge);
        let new = new_scale.clamp(MIN_SCALE, MAX_SCALE);
        // Anchor relative to the image centre.
        let cx = viewport[0] / 2.0 + self.pan[0];
        let cy = viewport[1] / 2.0 + self.pan[1];
        let k = new / old;
        self.pan = [
            anchor[0] - (anchor[0] - cx) * k - viewport[0] / 2.0,
            anchor[1] - (anchor[1] - cy) * k - viewport[1] / 2.0,
        ];
        self.zoom = Zoom::Scale(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: [f32; 2] = [4000.0, 2000.0];
    const VP: [f32; 2] = [1000.0, 1000.0];

    #[test]
    fn fit_centres_and_does_not_enlarge_small_images() {
        let v = View::default();
        assert_eq!(v.image_rect(IMG, VP, false), [0.0, 250.0, 1000.0, 500.0]);
        assert_eq!(v.scale([100.0, 100.0], VP, false), 1.0);
        assert_eq!(v.scale([100.0, 100.0], VP, true), 10.0);
        assert_eq!(
            v.image_rect([100.0, 100.0], VP, false),
            [450.0, 450.0, 100.0, 100.0]
        );
    }

    #[test]
    fn zoom_keeps_the_anchor_point_fixed() {
        let mut v = View::default();
        let anchor = [800.0, 300.0];
        // Image point under the anchor before zooming.
        let before = v.image_rect(IMG, VP, false);
        let u = (anchor[0] - before[0]) / before[2];
        let w = (anchor[1] - before[1]) / before[3];
        v.zoom_to(1.0, anchor, IMG, VP, false);
        let after = v.image_rect(IMG, VP, false);
        assert!((after[0] + u * after[2] - anchor[0]).abs() <= 1.0);
        assert!((after[1] + w * after[3] - anchor[1]).abs() <= 1.0);
        assert_eq!(v.zoom, Zoom::Scale(1.0));
        v.zoom_to(1e9, anchor, IMG, VP, false);
        assert_eq!(v.zoom, Zoom::Scale(MAX_SCALE));
    }

    #[test]
    fn pan_is_unconstrained() {
        // Drag the image so its bottom-right corner sits at the window centre.
        let mut v = View {
            zoom: Zoom::Scale(1.0),
            pan: [0.0, 0.0],
        };
        v.pan = [-IMG[0] / 2.0, -IMG[1] / 2.0];
        let r = v.image_rect(IMG, VP, false);
        assert_eq!([r[0] + r[2], r[1] + r[3]], [500.0, 500.0]);
        assert!(!v.is_default());
    }

    #[test]
    fn transition_names_match_the_config_list() {
        for name in ff_core::config::TRANSITIONS
            .iter()
            .filter(|n| **n != "none")
        {
            assert!(transition_index(name).is_some(), "{name} has no shader");
        }
        assert_eq!(
            TRANSITION_SOURCES.len() + 1,
            ff_core::config::TRANSITIONS.len()
        );
        assert_eq!(transition_index("none"), None);
    }

    /// Every shader must at least parse and validate (naga is what wgpu uses at runtime).
    #[test]
    fn shaders_validate() {
        let common = include_str!("shaders/transition_common.wgsl");
        let mut sources = vec![(
            "image".to_owned(),
            include_str!("shaders/image.wgsl").to_owned(),
        )];
        for (name, body) in TRANSITION_SOURCES {
            sources.push((name.to_owned(), format!("{common}\n{body}")));
        }
        for (name, src) in sources {
            let module = wgpu::naga::front::wgsl::parse_str(&src)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&src)));
            wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }
}
