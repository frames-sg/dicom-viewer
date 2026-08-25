use j2k_core::PixelFormat;
use j2k_metal_support::ResidentMetalImage;
use objc2::{rc::Retained, runtime::ProtocolObject, Message};
use objc2_metal::{MTLBuffer, MTLDevice, MTLResource};

type MetalDevice = Retained<ProtocolObject<dyn MTLDevice>>;

#[derive(Debug, thiserror::Error)]
pub enum MetalWgpuInteropError {
    #[error("the wgpu renderer is not using its Metal backend")]
    NotMetalBackend,
    #[error("the resident image belongs to Metal device {actual}, not renderer device {expected}")]
    DeviceMismatch { expected: u64, actual: u64 },
    #[error("only immutable RGB8 resident images can be imported; got {0:?}")]
    UnsupportedPixelFormat(PixelFormat),
    #[error("invalid resident Metal image layout: {0}")]
    InvalidLayout(&'static str),
    #[error("resident Metal allocation is empty")]
    EmptyAllocation,
    #[error("resident Metal allocation is too large for this platform")]
    AllocationTooLarge,
    #[error(
        "resident Metal allocation is {allocation_len} bytes, exceeding renderer limits (buffer {max_buffer_size}, storage binding {max_storage_buffer_binding_size})"
    )]
    WgpuBufferLimit {
        allocation_len: u64,
        max_buffer_size: u64,
        max_storage_buffer_binding_size: u64,
    },
    #[error("resident image addressing exceeds the renderer shader's 32-bit range")]
    ShaderAddressTooLarge,
    #[error("Metal support operation failed: {0}")]
    MetalSupport(#[from] j2k_metal_support::MetalSupportError),
}

/// Return the size of the allocation retained by a resident image.
pub fn resident_allocation_len(image: &ResidentMetalImage) -> Result<usize, MetalWgpuInteropError> {
    // SAFETY: Reading the immutable allocation length does not access its
    // contents, create an alias, or expose the raw handle.
    let allocation_len = unsafe { image.raw_buffer() }.length();
    if allocation_len == 0 {
        return Err(MetalWgpuInteropError::EmptyAllocation);
    }
    Ok(allocation_len)
}

fn validate_wgpu_buffer_limits(
    allocation_len: u64,
    max_buffer_size: u64,
    max_storage_buffer_binding_size: u64,
) -> Result<(), MetalWgpuInteropError> {
    if allocation_len > max_buffer_size || allocation_len > max_storage_buffer_binding_size {
        return Err(MetalWgpuInteropError::WgpuBufferLimit {
            allocation_len,
            max_buffer_size,
            max_storage_buffer_binding_size,
        });
    }
    Ok(())
}

/// The exact Metal device backing a wgpu renderer.
///
/// Construction validates the wgpu backend and retains the underlying
/// Objective-C device. The exposed retained device is therefore the only
/// device callers should supply to a Metal decoder session.
#[derive(Clone)]
pub struct MetalWgpuBridge {
    device: wgpu::Device,
    metal_device: MetalDevice,
    registry_id: u64,
}

impl std::fmt::Debug for MetalWgpuBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MetalWgpuBridge")
            .field("registry_id", &self.registry_id)
            .finish_non_exhaustive()
    }
}

impl MetalWgpuBridge {
    /// Extract and retain the exact `MTLDevice` owned by `device`.
    pub fn new(device: &wgpu::Device) -> Result<Self, MetalWgpuInteropError> {
        // SAFETY: The HAL guard is borrowed only long enough to clone the
        // retained Objective-C device. No HAL object is destroyed or mutated.
        let hal = unsafe { device.as_hal::<wgpu_hal::api::Metal>() }
            .ok_or(MetalWgpuInteropError::NotMetalBackend)?;
        let metal_device = hal.raw_device().clone();
        let registry_id = metal_device.registryID();
        Ok(Self {
            device: device.clone(),
            metal_device,
            registry_id,
        })
    }

    #[must_use]
    pub fn metal_device(&self) -> MetalDevice {
        self.metal_device.clone()
    }

    #[must_use]
    pub const fn device_registry_id(&self) -> u64 {
        self.registry_id
    }

    /// Adopt an immutable RGB8 Metal allocation as a read-only wgpu buffer.
    ///
    /// The returned wgpu buffer retains the allocation independently of the
    /// source image. Its descriptor exactly describes the existing allocation;
    /// only read usages are granted.
    pub fn import_rgb8(
        &self,
        image: &ResidentMetalImage,
    ) -> Result<ImportedMetalBuffer, MetalWgpuInteropError> {
        let metadata = ValidatedImage::new(image, &self.metal_device)?;
        let limits = self.device.limits();
        validate_wgpu_buffer_limits(
            metadata.allocation_len,
            limits.max_buffer_size,
            limits.max_storage_buffer_binding_size,
        )?;

        // SAFETY: `ResidentMetalImage` promises an immutable, completed
        // allocation. Retaining it does not expose a writable alias.
        let retained = unsafe { image.raw_buffer() }.retain();
        // SAFETY: The buffer belongs to this exact HAL device, is initialized,
        // immutable, nonempty, and the descriptor below exactly matches its
        // allocation size and permitted read-only uses.
        let hal_buffer =
            unsafe { wgpu_hal::metal::Device::buffer_from_raw(retained, metadata.allocation_len) };
        let descriptor = wgpu::BufferDescriptor {
            label: Some("DICOM viewer resident Metal tile"),
            size: metadata.allocation_len,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        };
        // SAFETY: `hal_buffer` was created from this device's MTLBuffer and
        // satisfies every ownership, initialization, size, and usage invariant
        // documented by `create_buffer_from_hal`.
        let buffer = unsafe {
            self.device
                .create_buffer_from_hal::<wgpu_hal::api::Metal>(hal_buffer, &descriptor)
        };

        Ok(ImportedMetalBuffer {
            buffer,
            width: metadata.width,
            height: metadata.height,
            pitch_bytes: metadata.pitch_bytes,
            byte_offset: metadata.byte_offset,
            decoded_byte_len: metadata.decoded_byte_len,
        })
    }

    /// Build an immutable resident image from copied bytes for downstream
    /// interop tests. This is excluded from normal production builds.
    #[cfg(feature = "test-support")]
    pub fn resident_rgb8_test_fixture(
        &self,
        bytes: &[u8],
        byte_offset: usize,
        dimensions: (u32, u32),
        pitch_bytes: usize,
    ) -> Result<ResidentMetalImage, MetalWgpuInteropError> {
        use j2k_metal_support::MetalImageLayout;

        if bytes.is_empty() {
            return Err(MetalWgpuInteropError::EmptyAllocation);
        }
        let buffer =
            j2k_metal_support::checked_shared_buffer_with_bytes(&self.metal_device, bytes)?;
        let layout = MetalImageLayout::new(byte_offset, dimensions, pitch_bytes, PixelFormat::Rgb8)
            .map_err(|_| {
                MetalWgpuInteropError::InvalidLayout("invalid RGB8 test fixture layout")
            })?;
        // SAFETY: Metal copied `bytes` into a fresh allocation synchronously.
        // No raw alias survives and no CPU or GPU writer can mutate it.
        unsafe { ResidentMetalImage::from_completed_buffer(buffer, layout) }
            .map_err(|_| MetalWgpuInteropError::InvalidLayout("test fixture exceeds allocation"))
    }
}

#[derive(Debug)]
pub struct ImportedMetalBuffer {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    pitch_bytes: u32,
    byte_offset: u32,
    decoded_byte_len: u64,
}

impl ImportedMetalBuffer {
    #[must_use]
    pub const fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    #[must_use]
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[must_use]
    pub const fn pitch_bytes(&self) -> u32 {
        self.pitch_bytes
    }

    #[must_use]
    pub const fn byte_offset(&self) -> u32 {
        self.byte_offset
    }

    #[must_use]
    pub const fn decoded_byte_len(&self) -> u64 {
        self.decoded_byte_len
    }
}

#[derive(Debug, Clone, Copy)]
struct ValidatedImage {
    width: u32,
    height: u32,
    pitch_bytes: u32,
    byte_offset: u32,
    decoded_byte_len: u64,
    allocation_len: u64,
}

impl ValidatedImage {
    fn new(
        image: &ResidentMetalImage,
        expected_device: &ProtocolObject<dyn MTLDevice>,
    ) -> Result<Self, MetalWgpuInteropError> {
        let expected_registry_id = expected_device.registryID();
        if image.device_registry_id() != expected_registry_id {
            return Err(MetalWgpuInteropError::DeviceMismatch {
                expected: expected_registry_id,
                actual: image.device_registry_id(),
            });
        }
        // SAFETY: Device identity is read without exposing or retaining the
        // immutable allocation handle outside this audited boundary.
        let allocation_device = unsafe { image.raw_buffer() }.device();
        if !std::ptr::eq(&*allocation_device, expected_device) {
            return Err(MetalWgpuInteropError::DeviceMismatch {
                expected: expected_registry_id,
                actual: allocation_device.registryID(),
            });
        }
        if image.pixel_format() != PixelFormat::Rgb8 {
            return Err(MetalWgpuInteropError::UnsupportedPixelFormat(
                image.pixel_format(),
            ));
        }
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 {
            return Err(MetalWgpuInteropError::InvalidLayout(
                "image dimensions must be nonzero",
            ));
        }
        let minimum_pitch = width
            .checked_mul(3)
            .ok_or(MetalWgpuInteropError::InvalidLayout(
                "RGB row size overflows",
            ))?;
        let pitch_bytes = u32::try_from(image.pitch_bytes()).map_err(|_| {
            MetalWgpuInteropError::InvalidLayout("row pitch exceeds the shader's u32 range")
        })?;
        if pitch_bytes < minimum_pitch {
            return Err(MetalWgpuInteropError::InvalidLayout(
                "row pitch is shorter than an RGB row",
            ));
        }
        let byte_offset = u32::try_from(image.byte_offset())
            .map_err(|_| MetalWgpuInteropError::ShaderAddressTooLarge)?;
        let decoded_byte_len = u64::try_from(image.byte_len())
            .map_err(|_| MetalWgpuInteropError::AllocationTooLarge)?;
        // SAFETY: Reading the allocation length does not access contents or
        // create a mutable alias. The raw handle remains private to this crate.
        let allocation_len = u64::try_from(unsafe { image.raw_buffer() }.length())
            .map_err(|_| MetalWgpuInteropError::AllocationTooLarge)?;
        if allocation_len == 0 {
            return Err(MetalWgpuInteropError::EmptyAllocation);
        }
        if allocation_len % 4 != 0 {
            return Err(MetalWgpuInteropError::InvalidLayout(
                "storage-buffer allocation length must be a multiple of four bytes",
            ));
        }
        let end = u64::from(byte_offset).checked_add(decoded_byte_len).ok_or(
            MetalWgpuInteropError::InvalidLayout("resident image range overflows"),
        )?;
        if end > allocation_len {
            return Err(MetalWgpuInteropError::InvalidLayout(
                "resident image range exceeds its allocation",
            ));
        }
        if end > u64::from(u32::MAX) {
            return Err(MetalWgpuInteropError::ShaderAddressTooLarge);
        }
        Ok(Self {
            width,
            height,
            pitch_bytes,
            byte_offset,
            decoded_byte_len,
            allocation_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use j2k_metal_support::MetalImageLayout;

    use super::*;

    #[test]
    fn rejects_allocations_larger_than_wgpu_buffer_or_binding_limits() {
        assert!(validate_wgpu_buffer_limits(512, 512, 512).is_ok());
        assert!(validate_wgpu_buffer_limits(513, 512, 1_024).is_err());
        assert!(validate_wgpu_buffer_limits(513, 1_024, 512).is_err());
    }

    fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    #[test]
    fn extracts_the_exact_renderer_device_and_imports_a_retained_rgb8_buffer() {
        let Some((device, queue)) = test_device() else {
            return;
        };
        let bridge = MetalWgpuBridge::new(&device).unwrap();
        let bytes = [1_u8, 2, 3, 4, 5, 6, 0, 0];
        let metal_device = bridge.metal_device();
        let metal_buffer =
            j2k_metal_support::checked_shared_buffer_with_bytes(&metal_device, &bytes).unwrap();
        let layout = MetalImageLayout::new(0, (2, 1), 8, PixelFormat::Rgb8).unwrap();
        // SAFETY: The shared test buffer has completed CPU initialization and
        // no mutable aliases survive this call.
        let image =
            unsafe { ResidentMetalImage::from_completed_buffer(metal_buffer, layout) }.unwrap();

        let imported = bridge.import_rgb8(&image).unwrap();
        drop(image);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("retained Metal import lifetime test"),
            size: 8,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("retained Metal import lifetime test"),
        });
        encoder.copy_buffer_to_buffer(imported.buffer(), 0, &readback, 0, 8);
        queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let mapped = readback.slice(..).get_mapped_range();

        assert_eq!(
            bridge.device_registry_id(),
            bridge.metal_device().registryID()
        );
        assert_eq!(imported.dimensions(), (2, 1));
        assert_eq!(imported.pitch_bytes(), 8);
        assert_eq!(imported.decoded_byte_len(), 8);
        assert_eq!(imported.buffer().size(), 8);
        assert_eq!(&mapped[..6], &bytes[..6]);
    }

    #[test]
    fn resident_allocation_accounting_charges_the_full_shared_buffer() {
        let Some((device, _queue)) = test_device() else {
            return;
        };
        let bridge = MetalWgpuBridge::new(&device).unwrap();
        let metal_device = bridge.metal_device();
        let metal_buffer = j2k_metal_support::checked_shared_buffer(&metal_device, 64).unwrap();
        let layout = MetalImageLayout::new(16, (1, 1), 4, PixelFormat::Rgb8).unwrap();
        // SAFETY: This fresh test allocation has no pending writer or aliases.
        let image =
            unsafe { ResidentMetalImage::from_completed_buffer(metal_buffer, layout) }.unwrap();

        assert_eq!(image.byte_len(), 4);
        assert_eq!(resident_allocation_len(&image).unwrap(), 64);
    }

    #[test]
    fn rejects_non_rgb8_before_import() {
        let Some((device, _queue)) = test_device() else {
            return;
        };
        let bridge = MetalWgpuBridge::new(&device).unwrap();
        let metal_device = bridge.metal_device();
        let metal_buffer = j2k_metal_support::checked_shared_buffer(&metal_device, 4).unwrap();
        let layout = MetalImageLayout::new(0, (1, 1), 4, PixelFormat::Rgba8).unwrap();
        // SAFETY: This fresh test allocation has no pending writer or aliases.
        let image =
            unsafe { ResidentMetalImage::from_completed_buffer(metal_buffer, layout) }.unwrap();

        let error = bridge.import_rgb8(&image).unwrap_err();

        assert!(matches!(
            error,
            MetalWgpuInteropError::UnsupportedPixelFormat(PixelFormat::Rgba8)
        ));
    }
}
