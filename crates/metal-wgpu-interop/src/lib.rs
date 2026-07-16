//! Audited ownership boundary between immutable J2K Metal images and wgpu.
//!
//! The crate is intentionally macOS-only in behavior. Keeping every raw
//! Objective-C and wgpu-hal operation here lets the viewer and its core retain
//! `forbid(unsafe_code)`.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use j2k_metal_support::ResidentMetalImage;
#[cfg(target_os = "macos")]
pub use macos::{ImportedMetalBuffer, MetalWgpuBridge, MetalWgpuInteropError};
