use std::sync::Arc;

use dicom_viewer_core::{RgbaTile, ViewerOpenOptions};
use eframe::{egui, egui_wgpu, wgpu};

use super::DecodedTile;

const RGB_TO_RGBA_SHADER: &str = r#"
struct TileLayout {
    width: u32,
    height: u32,
    pitch_bytes: u32,
    byte_offset: u32,
}

@group(0) @binding(0) var<storage, read> source: array<u32>;
@group(0) @binding(1) var destination: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> tile_layout: TileLayout;

fn source_byte(index: u32) -> u32 {
    let word = source[index >> 2u];
    let shift = (index & 3u) << 3u;
    return (word >> shift) & 255u;
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
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(rgb, 1.0));
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
    metal_bridge: Option<metal_wgpu_interop::MetalWgpuBridge>,
    #[cfg(target_os = "macos")]
    metal_bridge_error: Option<String>,
}

impl WgpuTileUploader {
    pub(super) fn new(state: egui_wgpu::RenderState) -> Self {
        let conversion_layout = conversion_layout(&state.device);
        let conversion_pipeline = conversion_pipeline(&state.device, &conversion_layout);
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
            metal_bridge,
            #[cfg(target_os = "macos")]
            metal_bridge_error,
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

    /// Upload one coherent UI batch. Every Metal conversion is encoded into a
    /// single command buffer, while CPU and Metal outputs share registration.
    pub(super) fn upload_batch(
        &mut self,
        tiles: Vec<DecodedTile>,
    ) -> Vec<Result<RegisteredTileTexture, TileUploadError>> {
        let device = &self.context.state.device;
        let queue = &self.context.state.queue;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("DICOM viewer tile upload batch"),
        });
        let mut prepared = Vec::with_capacity(tiles.len());

        for tile in tiles {
            let result = match tile {
                DecodedTile::Cpu(tile) => prepare_cpu_texture(device, queue, tile),
                #[cfg(target_os = "macos")]
                DecodedTile::Metal(tile) => self.prepare_metal_texture(&mut encoder, tile),
            };
            prepared.push(result);
        }

        queue.submit([encoder.finish()]);
        prepared
            .into_iter()
            .map(|result| result.map(|texture| self.register(texture)))
            .collect()
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
        &self,
        encoder: &mut wgpu::CommandEncoder,
        tile: dicom_viewer_core::MetalRenderTile,
    ) -> Result<PreparedTexture, TileUploadError> {
        let image = tile
            .resident_image()
            .map_err(|error| TileUploadError::MetalTile(error.to_string()))?;
        self.prepare_metal_image(encoder, image)
    }

    #[cfg(target_os = "macos")]
    fn prepare_metal_image(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        image: &metal_wgpu_interop::ResidentMetalImage,
    ) -> Result<PreparedTexture, TileUploadError> {
        let bridge = self.metal_bridge.as_ref().ok_or_else(|| {
            TileUploadError::MetalTile(
                self.metal_bridge_error
                    .clone()
                    .unwrap_or_else(|| "Metal/wgpu bridge is unavailable".into()),
            )
        })?;
        let imported = bridge.import_rgb8(image)?;
        let (width, height) = imported.dimensions();
        let texture = create_rgba_texture(
            &self.context.state.device,
            width,
            height,
            wgpu::TextureUsages::STORAGE_BINDING,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let params = [
            width,
            height,
            imported.pitch_bytes(),
            imported.byte_offset(),
        ];
        let uniform = self
            .context
            .state
            .device
            .create_buffer(&wgpu::BufferDescriptor {
                label: Some("DICOM viewer Metal tile layout"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        let mut bytes = [0_u8; 16];
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
        })
    }
}

struct PreparedTexture {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
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
    })
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
mod tests {
    use std::sync::{mpsc, Arc};

    use super::*;

    fn render_state() -> Option<egui_wgpu::RenderState> {
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

    #[cfg(target_os = "macos")]
    #[test]
    fn metal_resident_rgb8_is_converted_to_exact_rgba_without_host_readback() {
        let Some(state) = render_state() else {
            return;
        };
        let device = state.device.clone();
        let queue = state.queue.clone();
        let uploader = WgpuTileUploader::new(state);
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
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Metal conversion test"),
        });
        let prepared = uploader.prepare_metal_image(&mut encoder, &image).unwrap();
        queue.submit([encoder.finish()]);
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
}
