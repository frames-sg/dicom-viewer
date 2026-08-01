#[cfg(target_os = "macos")]
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dicom_viewer_core::{ColorLut3d, RgbaTile, ViewerOpenOptions};
use eframe::{egui, egui_wgpu, wgpu};

use super::DecodedTile;

#[cfg(target_os = "macos")]
// Keep the renderer copy bound aligned with the process-wide ICC proof cache.
const RENDERER_COLOR_LUT_CACHE_CAPACITY: usize = 16;

const RGB_TO_RGBA_SHADER: &str = r#"
struct TileLayout {
    width: u32,
    height: u32,
    pitch_bytes: u32,
    byte_offset: u32,
    use_color_lut: u32,
    color_lut_edge: u32,
    _padding0: u32,
    _padding1: u32,
}

@group(0) @binding(0) var<storage, read> source: array<u32>;
@group(0) @binding(1) var destination: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> tile_layout: TileLayout;
@group(0) @binding(3) var color_lut: texture_3d<f32>;
@group(0) @binding(4) var color_lut_sampler: sampler;

fn source_byte(index: u32) -> u32 {
    let word = source[index >> 2u];
    let shift = (index & 3u) << 3u;
    return (word >> shift) & 255u;
}

fn apply_color_lut(rgb: vec3<f32>) -> vec3<f32> {
    if (tile_layout.use_color_lut == 0u) {
        return rgb;
    }
    let edge = f32(tile_layout.color_lut_edge);
    let coordinate = (rgb * (edge - 1.0) + vec3<f32>(0.5)) / edge;
    return textureSampleLevel(color_lut, color_lut_sampler, coordinate, 0.0).rgb;
}

@compute @workgroup_size(8, 8, 1)
fn convert(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= tile_layout.width || id.y >= tile_layout.height) {
        return;
    }
    let offset = tile_layout.byte_offset + id.y * tile_layout.pitch_bytes + id.x * 3u;
    let rgb = vec3<f32>(
        f32(source_byte(offset)),
        f32(source_byte(offset + 1u)),
        f32(source_byte(offset + 2u)),
    ) / 255.0;
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(apply_color_lut(rgb), 1.0));
}
"#;

#[derive(Debug, thiserror::Error)]
pub(super) enum TileUploadError {
    #[error("invalid CPU RGBA tile: {0}")]
    InvalidCpuTile(&'static str),
    #[cfg(target_os = "macos")]
    #[error("Metal tile import failed: {0}")]
    MetalInterop(#[from] metal_wgpu_interop::MetalWgpuInteropError),
    #[cfg(target_os = "macos")]
    #[error("Metal tile contract failed: {0}")]
    MetalTile(String),
}

impl TileUploadError {
    pub(super) const fn permits_cpu_retry(&self) -> bool {
        #[cfg(target_os = "macos")]
        return matches!(self, Self::MetalInterop(_) | Self::MetalTile(_));
        #[cfg(not(target_os = "macos"))]
        false
    }
}

struct WgpuContext {
    state: egui_wgpu::RenderState,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ColorLutTextureKey {
    profile_sha256: String,
    edge: u32,
}

#[cfg(target_os = "macos")]
struct ColorLutTextureCache {
    entries: VecDeque<(ColorLutTextureKey, Arc<wgpu::Texture>)>,
}

#[cfg(target_os = "macos")]
impl ColorLutTextureCache {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    fn get_or_insert_with(
        &mut self,
        key: ColorLutTextureKey,
        create: impl FnOnce() -> wgpu::Texture,
    ) -> Arc<wgpu::Texture> {
        if let Some(position) = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == &key)
        {
            let entry = self
                .entries
                .remove(position)
                .expect("located color LUT entry remains present");
            let texture = Arc::clone(&entry.1);
            self.entries.push_back(entry);
            return texture;
        }
        if self.entries.len() >= RENDERER_COLOR_LUT_CACHE_CAPACITY {
            self.entries.pop_front();
        }
        let texture = Arc::new(create());
        self.entries.push_back((key, Arc::clone(&texture)));
        texture
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn contains_profile(&self, profile_sha256: &str) -> bool {
        self.entries
            .iter()
            .any(|(key, _)| key.profile_sha256 == profile_sha256)
    }
}

#[cfg(target_os = "macos")]
struct ColorLutBinding {
    view: wgpu::TextureView,
    edge: u32,
    enabled: bool,
    owner: Arc<wgpu::Texture>,
}

/// Owns a native wgpu texture and its egui registration as one lifetime.
pub(super) struct RegisteredTileTexture {
    context: Arc<WgpuContext>,
    texture_id: egui::TextureId,
    _texture: wgpu::Texture,
    width: u32,
    height: u32,
}

impl RegisteredTileTexture {
    pub(super) const fn id(&self) -> egui::TextureId {
        self.texture_id
    }

    pub(super) const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[cfg(test)]
    const fn texture(&self) -> &wgpu::Texture {
        &self._texture
    }
}

impl Drop for RegisteredTileTexture {
    fn drop(&mut self) {
        self.context
            .state
            .renderer
            .write()
            .free_texture(&self.texture_id);
    }
}

pub(super) struct WgpuTileUploader {
    context: Arc<WgpuContext>,
    conversion_layout: wgpu::BindGroupLayout,
    conversion_pipeline: wgpu::ComputePipeline,
    #[cfg(target_os = "macos")]
    color_lut_sampler: wgpu::Sampler,
    #[cfg(target_os = "macos")]
    identity_color_lut: Arc<wgpu::Texture>,
    #[cfg(target_os = "macos")]
    color_luts: ColorLutTextureCache,
    #[cfg(target_os = "macos")]
    metal_bridge: Option<metal_wgpu_interop::MetalWgpuBridge>,
    #[cfg(target_os = "macos")]
    metal_bridge_error: Option<String>,
    submissions: u64,
}

/// An upload result aligned with one owned input tile.
///
/// Deferred tiles retain their original decoded allocation so the caller can
/// put them back into its decoded cache without a copy or a new source read.
pub(super) enum BudgetedUploadOutcome {
    Ready(RegisteredTileTexture),
    Failed(TileUploadError),
    Deferred(DecodedTile),
}

pub(super) trait TileUploadSink {
    fn upload_batch_budgeted(
        &mut self,
        tiles: Vec<DecodedTile>,
        cpu_budget: Duration,
    ) -> Vec<BudgetedUploadOutcome>;
}

enum PreparedBudgetedUpload {
    Registered(RegisteredTileTexture),
    Prepared(PreparedTexture),
    Failed(TileUploadError),
    Deferred(DecodedTile),
}

enum OwnedUploadInput {
    Decoded(DecodedTile),
    #[cfg(all(test, target_os = "macos"))]
    TestMetal(metal_wgpu_interop::ResidentMetalImage),
}

impl WgpuTileUploader {
    pub(super) fn new(state: egui_wgpu::RenderState) -> Self {
        let conversion_layout = conversion_layout(&state.device);
        let conversion_pipeline = conversion_pipeline(&state.device, &conversion_layout);
        #[cfg(target_os = "macos")]
        let color_lut_sampler = state.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("DICOM viewer ICC LUT sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        #[cfg(target_os = "macos")]
        let identity_color_lut = Arc::new(create_color_lut_texture(
            &state.device,
            &state.queue,
            2,
            &identity_color_lut_rgba(),
            "DICOM viewer identity color LUT",
        ));
        #[cfg(target_os = "macos")]
        let (metal_bridge, metal_bridge_error) =
            match metal_wgpu_interop::MetalWgpuBridge::new(&state.device) {
                Ok(bridge) => (Some(bridge), None),
                Err(error) => (None, Some(error.to_string())),
            };
        Self {
            context: Arc::new(WgpuContext { state }),
            conversion_layout,
            conversion_pipeline,
            #[cfg(target_os = "macos")]
            color_lut_sampler,
            #[cfg(target_os = "macos")]
            identity_color_lut,
            #[cfg(target_os = "macos")]
            color_luts: ColorLutTextureCache::new(),
            #[cfg(target_os = "macos")]
            metal_bridge,
            #[cfg(target_os = "macos")]
            metal_bridge_error,
            submissions: 0,
        }
    }

    pub(super) fn viewer_open_options(&self) -> Result<ViewerOpenOptions, String> {
        let options = ViewerOpenOptions::from_environment().map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        if let Some(bridge) = &self.metal_bridge {
            return Ok(options.with_metal_device(bridge.metal_device()));
        }
        Ok(options)
    }

    pub(super) fn backend_warning(&self) -> Option<&str> {
        #[cfg(target_os = "macos")]
        return self.metal_bridge_error.as_deref();
        #[cfg(not(target_os = "macos"))]
        None
    }

    pub(super) fn clear_study_resources(&mut self) {
        #[cfg(target_os = "macos")]
        self.color_luts.clear();
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn cache_color_lut_for_test(&mut self, color_lut: &ColorLut3d) {
        drop(self.color_lut_binding(Some(color_lut)));
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn cached_color_lut_count_for_test(&self) -> usize {
        self.color_luts.len()
    }

    /// Upload one coherent UI batch. Every Metal conversion is encoded into a
    /// single command buffer, while CPU and Metal outputs share registration.
    pub(super) fn upload_batch(
        &mut self,
        tiles: Vec<DecodedTile>,
    ) -> Vec<Result<RegisteredTileTexture, TileUploadError>> {
        self.upload_batch_inner(tiles, None)
            .into_iter()
            .map(|outcome| match outcome {
                BudgetedUploadOutcome::Ready(texture) => Ok(texture),
                BudgetedUploadOutcome::Failed(error) => Err(error),
                BudgetedUploadOutcome::Deferred(_) => {
                    unreachable!("an unbudgeted upload cannot defer a tile")
                }
            })
            .collect()
    }

    /// Uploads an owned, ordered batch while limiting actual CPU texture work.
    ///
    /// The first CPU tile is always attempted. The budget is checked only
    /// between CPU tiles, and every later tile that does not fit is returned as
    /// `Deferred` with its original decoded allocation. Metal work is selected
    /// by the caller's count cap and encoded into at most one submission.
    pub(super) fn upload_batch_budgeted(
        &mut self,
        tiles: Vec<DecodedTile>,
        cpu_budget: Duration,
    ) -> Vec<BudgetedUploadOutcome> {
        if cpu_budget == Duration::MAX {
            return self
                .upload_batch(tiles)
                .into_iter()
                .map(|result| match result {
                    Ok(texture) => BudgetedUploadOutcome::Ready(texture),
                    Err(error) => BudgetedUploadOutcome::Failed(error),
                })
                .collect();
        }
        self.upload_batch_inner(tiles, Some(cpu_budget))
    }

    fn upload_batch_inner(
        &mut self,
        tiles: Vec<DecodedTile>,
        cpu_budget: Option<Duration>,
    ) -> Vec<BudgetedUploadOutcome> {
        self.upload_inputs(
            tiles.into_iter().map(OwnedUploadInput::Decoded).collect(),
            cpu_budget,
        )
    }

    fn upload_inputs(
        &mut self,
        inputs: Vec<OwnedUploadInput>,
        cpu_budget: Option<Duration>,
    ) -> Vec<BudgetedUploadOutcome> {
        if inputs.is_empty() {
            return Vec::new();
        }
        let device = self.context.state.device.clone();
        let queue = self.context.state.queue.clone();
        let mut encoder = None;
        let mut prepared = Vec::with_capacity(inputs.len());
        let mut cpu_uploads = 0_usize;
        let mut cpu_elapsed = Duration::ZERO;

        for input in inputs {
            let result = match input {
                OwnedUploadInput::Decoded(tile) => match tile {
                    DecodedTile::Cpu(tile)
                        if cpu_uploads > 0
                            && cpu_budget.is_some_and(|budget| cpu_elapsed >= budget) =>
                    {
                        PreparedBudgetedUpload::Deferred(DecodedTile::Cpu(tile))
                    }
                    DecodedTile::Cpu(tile) => {
                        let started = Instant::now();
                        let result = prepare_and_register_cpu(
                            || prepare_cpu_texture(&device, &queue, tile),
                            |texture| self.register(texture),
                        );
                        cpu_elapsed = cpu_elapsed.saturating_add(started.elapsed());
                        cpu_uploads = cpu_uploads.saturating_add(1);
                        match result {
                            Ok(texture) => PreparedBudgetedUpload::Registered(texture),
                            Err(error) => PreparedBudgetedUpload::Failed(error),
                        }
                    }
                    #[cfg(target_os = "macos")]
                    DecodedTile::Metal(tile) => {
                        match self.prepare_metal_texture(&mut encoder, tile) {
                            Ok(texture) => PreparedBudgetedUpload::Prepared(texture),
                            Err(error) => PreparedBudgetedUpload::Failed(error),
                        }
                    }
                },
                #[cfg(all(test, target_os = "macos"))]
                OwnedUploadInput::TestMetal(image) => {
                    match self.prepare_metal_image(&mut encoder, &image, None) {
                        Ok(texture) => PreparedBudgetedUpload::Prepared(texture),
                        Err(error) => PreparedBudgetedUpload::Failed(error),
                    }
                }
            };
            prepared.push(result);
        }

        if let Some(encoder) = encoder {
            queue.submit([encoder.finish()]);
            self.submissions = self.submissions.saturating_add(1);
        }
        prepared
            .into_iter()
            .map(|result| match result {
                PreparedBudgetedUpload::Registered(texture) => {
                    BudgetedUploadOutcome::Ready(texture)
                }
                PreparedBudgetedUpload::Prepared(texture) => {
                    BudgetedUploadOutcome::Ready(self.register(texture))
                }
                PreparedBudgetedUpload::Failed(error) => BudgetedUploadOutcome::Failed(error),
                PreparedBudgetedUpload::Deferred(tile) => BudgetedUploadOutcome::Deferred(tile),
            })
            .collect()
    }

    pub(super) const fn submission_count(&self) -> u64 {
        self.submissions
    }

    fn register(&self, prepared: PreparedTexture) -> RegisteredTileTexture {
        let view = prepared
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let texture_id = self.context.state.renderer.write().register_native_texture(
            &self.context.state.device,
            &view,
            wgpu::FilterMode::Nearest,
        );
        RegisteredTileTexture {
            context: Arc::clone(&self.context),
            texture_id,
            _texture: prepared.texture,
            width: prepared.width,
            height: prepared.height,
        }
    }

    #[cfg(target_os = "macos")]
    fn prepare_metal_texture(
        &mut self,
        encoder: &mut Option<wgpu::CommandEncoder>,
        tile: dicom_viewer_core::MetalRenderTile,
    ) -> Result<PreparedTexture, TileUploadError> {
        let image = tile
            .resident_image()
            .map_err(|error| TileUploadError::MetalTile(error.to_string()))?;
        self.prepare_metal_image(encoder, image, tile.color_lut())
    }

    #[cfg(target_os = "macos")]
    fn prepare_metal_image(
        &mut self,
        encoder: &mut Option<wgpu::CommandEncoder>,
        image: &metal_wgpu_interop::ResidentMetalImage,
        color_lut: Option<&ColorLut3d>,
    ) -> Result<PreparedTexture, TileUploadError> {
        let (width, height) = image.dimensions();
        validate_texture_dimensions(
            width,
            height,
            self.context.state.device.limits().max_texture_dimension_2d,
        )
        .map_err(|message| TileUploadError::MetalTile(message.into()))?;
        let bridge = self.metal_bridge.as_ref().ok_or_else(|| {
            TileUploadError::MetalTile(
                self.metal_bridge_error
                    .clone()
                    .unwrap_or_else(|| "Metal/wgpu bridge is unavailable".into()),
            )
        })?;
        let imported = bridge.import_rgb8(image)?;
        let encoder = encoder.get_or_insert_with(|| {
            self.context
                .state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("DICOM viewer tile upload batch"),
                })
        });
        let (width, height) = imported.dimensions();
        let texture = create_rgba_texture(
            &self.context.state.device,
            width,
            height,
            wgpu::TextureUsages::STORAGE_BINDING,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color_lut = self.color_lut_binding(color_lut);
        let params = [
            width,
            height,
            imported.pitch_bytes(),
            imported.byte_offset(),
            u32::from(color_lut.enabled),
            color_lut.edge,
            0,
            0,
        ];
        let uniform = self
            .context
            .state
            .device
            .create_buffer(&wgpu::BufferDescriptor {
                label: Some("DICOM viewer Metal tile layout"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        let mut bytes = [0_u8; 32];
        for (chunk, value) in bytes.chunks_exact_mut(4).zip(params) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        self.context.state.queue.write_buffer(&uniform, 0, &bytes);
        let bind_group = self
            .context
            .state
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("DICOM viewer Metal RGB8 conversion"),
                layout: &self.conversion_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: imported.buffer().as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&color_lut.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&self.color_lut_sampler),
                    },
                ],
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("DICOM viewer Metal RGB8 conversion"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.conversion_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        }
        Ok(PreparedTexture {
            texture,
            width,
            height,
            _color_lut_owner: Some(color_lut.owner),
        })
    }

    #[cfg(target_os = "macos")]
    fn color_lut_binding(&mut self, color_lut: Option<&ColorLut3d>) -> ColorLutBinding {
        let Some(color_lut) = color_lut else {
            let owner = Arc::clone(&self.identity_color_lut);
            return ColorLutBinding {
                view: owner.create_view(&wgpu::TextureViewDescriptor::default()),
                edge: 2,
                enabled: false,
                owner,
            };
        };
        let key = ColorLutTextureKey {
            profile_sha256: color_lut.profile_sha256().to_string(),
            edge: color_lut.edge(),
        };
        let device = self.context.state.device.clone();
        let queue = self.context.state.queue.clone();
        let owner = self.color_luts.get_or_insert_with(key, || {
            create_color_lut_texture(
                &device,
                &queue,
                color_lut.edge(),
                color_lut.rgba(),
                "DICOM viewer embedded ICC color LUT",
            )
        });
        ColorLutBinding {
            view: owner.create_view(&wgpu::TextureViewDescriptor::default()),
            edge: color_lut.edge(),
            enabled: true,
            owner,
        }
    }
}

impl TileUploadSink for WgpuTileUploader {
    fn upload_batch_budgeted(
        &mut self,
        tiles: Vec<DecodedTile>,
        cpu_budget: Duration,
    ) -> Vec<BudgetedUploadOutcome> {
        Self::upload_batch_budgeted(self, tiles, cpu_budget)
    }
}

#[cfg(target_os = "macos")]
fn identity_color_lut_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(2 * 2 * 2 * 4);
    for blue in 0..=1 {
        for green in 0..=1 {
            for red in 0..=1 {
                rgba.extend_from_slice(&[red * 255, green * 255, blue * 255, 255]);
            }
        }
    }
    rgba
}

#[cfg(target_os = "macos")]
fn create_color_lut_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    edge: u32,
    rgba: &[u8],
    label: &str,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: edge,
            height: edge,
            depth_or_array_layers: edge,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(edge * 4),
            rows_per_image: Some(edge),
        },
        wgpu::Extent3d {
            width: edge,
            height: edge,
            depth_or_array_layers: edge,
        },
    );
    texture
}

#[cfg(test)]
fn upload_batch_needs_encoder(tiles: &[DecodedTile]) -> bool {
    #[cfg(target_os = "macos")]
    {
        tiles
            .iter()
            .any(|tile| matches!(tile, DecodedTile::Metal(_)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = tiles;
        false
    }
}

struct PreparedTexture {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    #[cfg(target_os = "macos")]
    _color_lut_owner: Option<Arc<wgpu::Texture>>,
}

fn prepare_and_register_cpu<Prepared, Registered, Error>(
    prepare: impl FnOnce() -> Result<Prepared, Error>,
    register: impl FnOnce(Prepared) -> Registered,
) -> Result<Registered, Error> {
    prepare().map(register)
}

fn prepare_cpu_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tile: RgbaTile,
) -> Result<PreparedTexture, TileUploadError> {
    if tile.width == 0 || tile.height == 0 {
        return Err(TileUploadError::InvalidCpuTile(
            "dimensions must be nonzero",
        ));
    }
    validate_texture_dimensions(
        tile.width,
        tile.height,
        device.limits().max_texture_dimension_2d,
    )
    .map_err(TileUploadError::InvalidCpuTile)?;
    let expected = usize::try_from(tile.width)
        .ok()
        .and_then(|width| width.checked_mul(tile.height as usize))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(TileUploadError::InvalidCpuTile("RGBA byte size overflows"))?;
    if tile.rgba.len() != expected {
        return Err(TileUploadError::InvalidCpuTile(
            "pixel byte count does not match dimensions",
        ));
    }
    let texture = create_rgba_texture(
        device,
        tile.width,
        tile.height,
        wgpu::TextureUsages::COPY_DST,
    );
    queue.write_texture(
        texture.as_image_copy(),
        &tile.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(tile.width * 4),
            rows_per_image: Some(tile.height),
        },
        wgpu::Extent3d {
            width: tile.width,
            height: tile.height,
            depth_or_array_layers: 1,
        },
    );
    Ok(PreparedTexture {
        texture,
        width: tile.width,
        height: tile.height,
        #[cfg(target_os = "macos")]
        _color_lut_owner: None,
    })
}

fn validate_texture_dimensions(
    width: u32,
    height: u32,
    max_dimension_2d: u32,
) -> Result<(), &'static str> {
    if width > max_dimension_2d {
        return Err("width exceeds the renderer texture limit");
    }
    if height > max_dimension_2d {
        return Err("height exceeds the renderer texture limit");
    }
    Ok(())
}

fn create_rgba_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    additional_usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("DICOM viewer slide tile"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | additional_usage,
        view_formats: &[],
    })
}

fn conversion_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("DICOM viewer RGB8 conversion layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn conversion_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::ComputePipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("DICOM viewer RGB8 conversion shader"),
        source: wgpu::ShaderSource::Wgsl(RGB_TO_RGBA_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("DICOM viewer RGB8 conversion pipeline layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("DICOM viewer RGB8 conversion pipeline"),
        layout: Some(&layout),
        module: &shader,
        entry_point: Some("convert"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

#[cfg(test)]
pub(super) fn render_state() -> Option<egui_wgpu::RenderState> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    #[cfg(target_os = "macos")]
    {
        descriptor.backends = wgpu::Backends::METAL;
    }
    let instance = wgpu::Instance::new(descriptor);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let renderer = egui_wgpu::Renderer::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        egui_wgpu::RendererOptions::PREDICTABLE,
    );
    Some(egui_wgpu::RenderState {
        adapter,
        #[cfg(not(target_arch = "wasm32"))]
        available_adapters: Vec::new(),
        device,
        queue,
        target_format: wgpu::TextureFormat::Rgba8Unorm,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::{mpsc, Arc};

    use super::*;

    #[test]
    fn texture_dimensions_are_rejected_before_wgpu_resource_creation() {
        assert_eq!(validate_texture_dimensions(4096, 4096, 4096), Ok(()));
        assert_eq!(
            validate_texture_dimensions(4097, 1, 4096),
            Err("width exceeds the renderer texture limit")
        );
        assert_eq!(
            validate_texture_dimensions(1, 4097, 4096),
            Err("height exceeds the renderer texture limit")
        );
    }

    #[test]
    fn cpu_upload_unit_completes_registration_inside_the_budgeted_operation() {
        let events = RefCell::new(Vec::new());
        let result = prepare_and_register_cpu(
            || {
                events.borrow_mut().push("prepare");
                Ok::<_, ()>(7_u8)
            },
            |prepared| {
                events.borrow_mut().push("register");
                prepared + 1
            },
        );

        assert_eq!(result, Ok(8));
        assert_eq!(*events.borrow(), ["prepare", "register"]);
    }

    fn read_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let row_bytes = width * 4;
        let padded_row_bytes = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile upload test readback"),
            size: u64::from(padded_row_bytes) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile upload test readback"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let mapped = buffer.slice(..).get_mapped_range();
        let mut rgba = Vec::with_capacity(row_bytes as usize * height as usize);
        for row in mapped.chunks_exact(padded_row_bytes as usize) {
            rgba.extend_from_slice(&row[..row_bytes as usize]);
        }
        drop(mapped);
        buffer.unmap();
        rgba
    }

    #[test]
    fn cpu_upload_is_exact_and_registration_is_released_with_the_texture() {
        let Some(state) = render_state() else {
            return;
        };
        let device = state.device.clone();
        let queue = state.queue.clone();
        let renderer = Arc::clone(&state.renderer);
        let mut uploader = WgpuTileUploader::new(state);
        let expected = vec![255, 0, 0, 255, 0, 127, 255, 64];

        let mut uploads = uploader.upload_batch(vec![DecodedTile::Cpu(RgbaTile {
            width: 2,
            height: 1,
            rgba: expected.clone(),
        })]);
        let uploaded = uploads.pop().unwrap().unwrap();
        let id = uploaded.id();

        assert!(renderer.read().texture(&id).is_some());
        assert_eq!(
            read_texture(&device, &queue, uploaded.texture(), 2, 1),
            expected
        );
        drop(uploaded);
        assert!(renderer.read().texture(&id).is_none());
    }

    #[test]
    fn empty_and_cpu_only_batches_need_no_command_encoder() {
        assert!(!upload_batch_needs_encoder(&[]));
        assert!(!upload_batch_needs_encoder(&[DecodedTile::Cpu(RgbaTile {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        })]));
    }

    #[test]
    fn zero_cpu_budget_uploads_the_first_tile_and_defers_the_second_without_loss() {
        let Some(state) = render_state() else {
            return;
        };
        let mut uploader = WgpuTileUploader::new(state);
        let first = RgbaTile {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        };
        let second = RgbaTile {
            width: 1,
            height: 1,
            rgba: vec![5, 6, 7, 255],
        };

        let outcomes = uploader.upload_batch_budgeted(
            vec![DecodedTile::Cpu(first), DecodedTile::Cpu(second)],
            std::time::Duration::ZERO,
        );
        let mut outcomes = outcomes.into_iter();

        assert!(matches!(
            outcomes.next(),
            Some(BudgetedUploadOutcome::Ready(_))
        ));
        let Some(BudgetedUploadOutcome::Deferred(DecodedTile::Cpu(deferred))) = outcomes.next()
        else {
            panic!("second CPU tile should be returned to the caller as deferred");
        };
        assert_eq!(deferred.rgba, vec![5, 6, 7, 255]);
        assert!(outcomes.next().is_none());
    }

    #[test]
    fn empty_and_cpu_only_budgeted_batches_do_not_submit_command_buffers() {
        let Some(state) = render_state() else {
            return;
        };
        let mut uploader = WgpuTileUploader::new(state);

        assert!(uploader
            .upload_batch_budgeted(Vec::new(), std::time::Duration::ZERO)
            .is_empty());
        assert_eq!(uploader.submission_count(), 0);

        let outcomes = uploader.upload_batch_budgeted(
            vec![DecodedTile::Cpu(RgbaTile {
                width: 1,
                height: 1,
                rgba: vec![9, 10, 11, 255],
            })],
            std::time::Duration::ZERO,
        );
        assert!(matches!(
            outcomes.as_slice(),
            [BudgetedUploadOutcome::Ready(_)]
        ));
        assert_eq!(uploader.submission_count(), 0);
    }

    #[cfg(target_os = "macos")]
    fn synthetic_color_lut(profile: &str) -> ColorLut3d {
        ColorLut3d::from_rgba8(2, identity_color_lut_rgba(), profile).unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn renderer_color_lut_cache_is_bounded_and_evicts_the_least_recent_profile() {
        let Some(state) = render_state() else {
            return;
        };
        let mut uploader = WgpuTileUploader::new(state);
        for index in 0..16 {
            let lut = synthetic_color_lut(&format!("profile-{index}"));
            drop(uploader.color_lut_binding(Some(&lut)));
        }

        let recently_used = synthetic_color_lut("profile-0");
        drop(uploader.color_lut_binding(Some(&recently_used)));
        let newest = synthetic_color_lut("profile-16");
        drop(uploader.color_lut_binding(Some(&newest)));

        assert_eq!(uploader.color_luts.len(), 16);
        assert!(uploader.color_luts.contains_profile("profile-0"));
        assert!(!uploader.color_luts.contains_profile("profile-1"));
        assert!(uploader.color_luts.contains_profile("profile-16"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn renderer_study_clear_releases_all_cached_color_luts() {
        let Some(state) = render_state() else {
            return;
        };
        let mut uploader = WgpuTileUploader::new(state);
        let lut = synthetic_color_lut("study-profile");
        drop(uploader.color_lut_binding(Some(&lut)));
        assert_eq!(uploader.color_luts.len(), 1);

        uploader.clear_study_resources();

        assert_eq!(uploader.color_luts.len(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mixed_cpu_and_multiple_metal_tiles_use_exactly_one_submission() {
        let Some(state) = render_state() else {
            return;
        };
        let mut uploader = WgpuTileUploader::new(state);
        let (first_metal, second_metal) = {
            let bridge = uploader.metal_bridge.as_ref().unwrap();
            (
                bridge
                    .resident_rgb8_test_fixture(&[1, 2, 3, 0], 0, (1, 1), 4)
                    .unwrap(),
                bridge
                    .resident_rgb8_test_fixture(&[4, 5, 6, 0], 0, (1, 1), 4)
                    .unwrap(),
            )
        };

        let outcomes = uploader.upload_inputs(
            vec![
                OwnedUploadInput::Decoded(DecodedTile::Cpu(RgbaTile {
                    width: 1,
                    height: 1,
                    rgba: vec![7, 8, 9, 255],
                })),
                OwnedUploadInput::TestMetal(first_metal),
                OwnedUploadInput::TestMetal(second_metal),
            ],
            Some(std::time::Duration::MAX),
        );

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes
            .iter()
            .all(|outcome| matches!(outcome, BudgetedUploadOutcome::Ready(_))));
        assert_eq!(uploader.submission_count(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn all_failed_metal_imports_submit_nothing_and_each_permits_one_cpu_retry() {
        let Some(state) = render_state() else {
            return;
        };
        let mut uploader = WgpuTileUploader::new(state);
        let (first_metal, second_metal) = {
            let bridge = uploader.metal_bridge.as_ref().unwrap();
            (
                bridge
                    .resident_rgb8_test_fixture(&[1, 2, 3, 0], 0, (1, 1), 4)
                    .unwrap(),
                bridge
                    .resident_rgb8_test_fixture(&[4, 5, 6, 0], 0, (1, 1), 4)
                    .unwrap(),
            )
        };
        uploader.metal_bridge = None;
        uploader.metal_bridge_error = Some("forced import failure".into());

        let outcomes = uploader.upload_inputs(
            vec![
                OwnedUploadInput::TestMetal(first_metal),
                OwnedUploadInput::TestMetal(second_metal),
            ],
            Some(std::time::Duration::MAX),
        );

        assert_eq!(outcomes.len(), 2);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    BudgetedUploadOutcome::Failed(error) if error.permits_cpu_retry()
                ))
                .count(),
            2,
            "each failed Metal tile should request exactly one caller-owned CPU retry"
        );
        assert_eq!(
            uploader.submission_count(),
            0,
            "a batch with no successfully encoded Metal work must not submit"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metal_resident_rgb8_is_converted_to_exact_rgba_without_host_readback() {
        let Some(state) = render_state() else {
            return;
        };
        let device = state.device.clone();
        let queue = state.queue.clone();
        let mut uploader = WgpuTileUploader::new(state);
        let bridge = uploader.metal_bridge.as_ref().unwrap();
        let rgb = [
            1_u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
        ];
        let mut pitched = vec![0_u8; 28];
        pitched[4..13].copy_from_slice(&rgb[..9]);
        pitched[16..25].copy_from_slice(&rgb[9..]);
        let image = bridge
            .resident_rgb8_test_fixture(&pitched, 4, (3, 2), 12)
            .unwrap();
        let mut encoder = None;
        let prepared = uploader
            .prepare_metal_image(&mut encoder, &image, None)
            .unwrap();
        queue.submit([encoder.take().unwrap().finish()]);
        let uploaded = uploader.register(prepared);
        let expected = rgb
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect::<Vec<_>>();

        assert_eq!(
            read_texture(&device, &queue, uploaded.texture(), 3, 2),
            expected
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn active_metal_color_lut_survives_cache_clear_and_matches_expected_code_values() {
        let Some(state) = render_state() else {
            return;
        };
        let device = state.device.clone();
        let queue = state.queue.clone();
        let mut uploader = WgpuTileUploader::new(state);
        let bridge = uploader.metal_bridge.as_ref().unwrap();
        let rgb = [0_u8, 0, 0, 255, 0, 255, 0, 255, 255, 255, 255, 255];
        let image = bridge
            .resident_rgb8_test_fixture(&rgb, 0, (4, 1), 12)
            .unwrap();
        let mut lut = Vec::new();
        for blue in [0_u8, 255] {
            for green in [0_u8, 255] {
                for red in [0_u8, 255] {
                    lut.extend_from_slice(&[255 - red, 255 - green, 255 - blue, 255]);
                }
            }
        }
        let lut = ColorLut3d::from_rgba8(2, lut, "synthetic-inverse").unwrap();
        let mut encoder = None;
        let prepared = uploader
            .prepare_metal_image(&mut encoder, &image, Some(&lut))
            .unwrap();
        assert!(prepared._color_lut_owner.is_some());
        assert_eq!(uploader.color_luts.len(), 1);
        uploader.clear_study_resources();
        assert_eq!(uploader.color_luts.len(), 0);
        queue.submit([encoder.take().unwrap().finish()]);
        let uploaded = uploader.register(prepared);

        assert_eq!(
            read_texture(&device, &queue, uploaded.texture(), 4, 1),
            vec![255, 255, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 0, 0, 0, 255,]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn local_dicom_corner_uploads_match_cpu_pixels() {
        let Some(path) = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE") else {
            eprintln!("skipping local DICOM upload parity; DICOM_VIEWER_WSI_FIXTURE unset");
            return;
        };
        let Some(state) = render_state() else {
            return;
        };
        let device = state.device.clone();
        let queue = state.queue.clone();
        let mut uploader = WgpuTileUploader::new(state);
        let options = uploader.viewer_open_options().expect("viewer options");
        let study = dicom_viewer_core::ViewerStudy::open_path_with_options(path, options)
            .expect("open local DICOM fixture");
        let level = study.summary().levels.last().expect("coarsest level");
        let (cols, rows) = level.tile_layout.grid_size().expect("regular tile grid");
        let requests = [
            (level.index, dicom_viewer_core::TileCoord::new(0, 0)),
            (
                level.index,
                dicom_viewer_core::TileCoord::new(cols - 1, rows - 1),
            ),
        ];
        let control = dicom_viewer_core::ReadControl::default();
        let expected = study
            .read_tiles_rgba_controlled(&requests, &control)
            .expect("read CPU reference corners");
        let decoded = study
            .read_tiles_for_render_controlled(&requests, &control)
            .expect("read renderer corners")
            .into_iter()
            .map(DecodedTile::from_render_tile)
            .collect::<Result<Vec<_>, _>>()
            .expect("supported renderer tiles");
        let actual = uploader.upload_batch(decoded);

        for (index, (expected, actual)) in expected.into_iter().zip(actual).enumerate() {
            let actual = actual.expect("upload corner tile");
            assert_eq!(actual.dimensions(), (expected.width, expected.height));
            let actual = read_texture(
                &device,
                &queue,
                actual.texture(),
                expected.width,
                expected.height,
            );
            let max_delta = actual
                .iter()
                .zip(&expected.rgba)
                .map(|(actual, expected)| actual.abs_diff(*expected))
                .max()
                .unwrap_or(0);
            assert!(
                max_delta <= 2,
                "corner {index} max channel delta {max_delta}"
            );
        }
    }
}
