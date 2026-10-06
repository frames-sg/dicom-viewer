use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex, OnceLock, TryLockError};

use lcms2::{DisallowCache, Flags, Intent, PixelFormat, Profile, ThreadContext, Transform};
use sha2::{Digest, Sha256};
use wsi_rs::{Dataset, IccProfileProvenance};

use crate::model::SelectedView;
use crate::{
    ColorLut3d, ColorManagementMode, ColorManagementStatus, ColorManagementSummary, RenderTile,
    RgbaTile, TileDecodeBackend,
};

const LUT_EDGE: u32 = 65;
const EXACT_LUT_EDGE: u32 = 256;
const MAX_LUT_CHANNEL_ERROR: u8 = 2;
const LUT_VALIDATION_ALGORITHM_VERSION: u32 = 2;
// An exact table is 64 MiB. Bound cached/in-flight profiles to two so adding
// exact fallback cannot turn the former 16-entry cache into a 1 GiB cache.
const ICC_PROOF_CACHE_CAPACITY: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
enum IccProofFailure {
    Validation { max_error: u8 },
    Infrastructure(String),
}

type IccProofResult = Result<Arc<ColorLut3d>, IccProofFailure>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IccProofKey {
    profile_sha256: [u8; 32],
    lut_edge: u32,
    max_channel_error: u8,
    algorithm_version: u32,
}

enum IccProofState {
    InFlight,
    Complete(IccProofResult),
}

struct IccProofEntry {
    state: Mutex<IccProofState>,
    ready: Condvar,
}

impl IccProofEntry {
    fn wait(&self) -> IccProofResult {
        let mut state = self.state.lock().map_err(|_| {
            IccProofFailure::Infrastructure("ICC proof entry lock is poisoned".into())
        })?;
        loop {
            match &*state {
                IccProofState::Complete(result) => return result.clone(),
                IccProofState::InFlight => {
                    state = self.ready.wait(state).map_err(|_| {
                        IccProofFailure::Infrastructure("ICC proof wait lock is poisoned".into())
                    })?;
                }
            }
        }
    }
}

#[derive(Default)]
struct IccProofCacheState {
    entries: HashMap<IccProofKey, Arc<IccProofEntry>>,
    lru: VecDeque<IccProofKey>,
}

struct IccProofCache {
    capacity: usize,
    state: Mutex<IccProofCacheState>,
}

impl IccProofCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(IccProofCacheState::default()),
        }
    }

    fn get_or_compute(
        &self,
        key: IccProofKey,
        compute: impl FnOnce() -> IccProofResult,
    ) -> IccProofResult {
        let (entry, computes) = 'admit: loop {
            let mut cache = self.state.lock().map_err(|_| {
                IccProofFailure::Infrastructure("ICC proof cache lock is poisoned".into())
            })?;
            if let Some(entry) = cache.entries.get(&key).cloned() {
                touch_lru(&mut cache.lru, &key);
                break (entry, false);
            } else {
                while cache.entries.len() >= self.capacity {
                    let Some(evicted) = oldest_completed_entry(&cache) else {
                        let oldest = cache
                            .lru
                            .front()
                            .expect("nonempty ICC proof cache has an LRU entry");
                        let pending = Arc::clone(&cache.entries[oldest]);
                        drop(cache);
                        // Wait without holding the cache lock. A completed
                        // slot can then be evicted without duplicating work or
                        // falling back to CPU just because another study opens.
                        if let Err(error @ IccProofFailure::Infrastructure(_)) = pending.wait() {
                            return Err(error);
                        }
                        continue 'admit;
                    };
                    cache.entries.remove(&evicted);
                    remove_lru(&mut cache.lru, &evicted);
                }
                let entry = Arc::new(IccProofEntry {
                    state: Mutex::new(IccProofState::InFlight),
                    ready: Condvar::new(),
                });
                cache.entries.insert(key.clone(), Arc::clone(&entry));
                cache.lru.push_back(key);
                break (entry, true);
            }
        };

        if computes {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(compute))
                .unwrap_or_else(|_| {
                    Err(IccProofFailure::Infrastructure(
                        "ICC proof computation panicked".into(),
                    ))
                });
            let mut state = match entry.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            *state = IccProofState::Complete(result.clone());
            entry.ready.notify_all();
            return result;
        }

        entry.wait()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state.lock().map_or(0, |state| state.entries.len())
    }
}

fn touch_lru(lru: &mut VecDeque<IccProofKey>, key: &IccProofKey) {
    remove_lru(lru, key);
    lru.push_back(key.clone());
}

fn remove_lru(lru: &mut VecDeque<IccProofKey>, key: &IccProofKey) {
    if let Some(position) = lru.iter().position(|candidate| candidate == key) {
        lru.remove(position);
    }
}

fn oldest_completed_entry(cache: &IccProofCacheState) -> Option<IccProofKey> {
    cache.lru.iter().find_map(|key| {
        let entry = cache.entries.get(key)?;
        let complete = match entry.state.try_lock() {
            Ok(state) => matches!(*state, IccProofState::Complete(_)),
            Err(TryLockError::Poisoned(_)) => true,
            Err(TryLockError::WouldBlock) => false,
        };
        complete.then(|| key.clone())
    })
}

fn process_icc_proof_cache() -> &'static IccProofCache {
    static CACHE: OnceLock<IccProofCache> = OnceLock::new();
    CACHE.get_or_init(|| IccProofCache::new(ICC_PROOF_CACHE_CAPACITY))
}

type RgbaTransform = Transform<u8, u8, ThreadContext, DisallowCache>;

struct IccTransform {
    transform: RgbaTransform,
    // LittleCMS transforms are destroyed before their originating context.
    _context: Mutex<ThreadContext>,
}

impl IccTransform {
    fn new(profile_bytes: &[u8]) -> Result<Self, lcms2::Error> {
        let context = ThreadContext::new();
        let input = Profile::new_icc_context(&context, profile_bytes)?;
        let output = Profile::new_srgb_context(&context);
        let transform = Transform::new_flags_context(
            &context,
            &input,
            PixelFormat::RGBA_8,
            &output,
            PixelFormat::RGBA_8,
            Intent::Perceptual,
            Flags::NO_CACHE | Flags::COPY_ALPHA,
        )?;
        Ok(Self {
            transform,
            _context: Mutex::new(context),
        })
    }

    fn transform_rgba(&self, rgba: &mut [u8]) {
        self.transform.transform_in_place(rgba);
    }
}

pub(crate) struct ColorManagement {
    transform: Option<Arc<IccTransform>>,
    lut: Option<Arc<ColorLut3d>>,
}

impl std::fmt::Debug for ColorManagement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ColorManagement")
            .field("active", &self.transform.is_some())
            .field("has_metal_lut", &self.lut.is_some())
            .finish()
    }
}

impl ColorManagement {
    pub(crate) fn build(
        dataset: &Dataset,
        view: SelectedView,
        backend: TileDecodeBackend,
    ) -> ColorManagementBuild {
        let profiles = dataset
            .source_icc_profiles_for_series(view.scene, view.series)
            .collect::<Vec<_>>();
        let selected = profiles
            .iter()
            .copied()
            .find(|profile| profile.key.optical_path.is_none() && profile.key.channel.is_none())
            .or_else(|| {
                profiles.iter().copied().find(|profile| {
                    profile.key.optical_path == Some(0) && profile.key.channel.is_none()
                })
            });
        let Some(profile) = selected else {
            return ColorManagementBuild {
                color: Self {
                    transform: None,
                    lut: None,
                },
                summary: ColorManagementSummary::unprofiled(),
                warnings: if profiles.is_empty() {
                    Vec::new()
                } else {
                    vec![
                        "embedded ICC profiles do not apply to the default optical path; displaying uncorrected pixels"
                            .into(),
                    ]
                },
                force_cpu: false,
            };
        };

        let profile_digest = profile_sha256_digest(&profile.bytes);
        let sha256 = profile_sha256(&profile_digest);
        let provenance = provenance_label(&profile.provenance);
        let mut warnings = Vec::new();
        if profiles.len() > 1 {
            warnings.push(format!(
                "dataset contains {} ICC profiles for the selected series; using {}",
                profiles.len(),
                provenance
            ));
        }
        let transform = match IccTransform::new(&profile.bytes) {
            Ok(transform) => Arc::new(transform),
            Err(error) => {
                warnings.push(format!(
                    "embedded ICC profile is malformed ({error}); displaying uncorrected pixels"
                ));
                return ColorManagementBuild {
                    color: Self {
                        transform: None,
                        lut: None,
                    },
                    summary: ColorManagementSummary {
                        status: ColorManagementStatus::MalformedProfile,
                        sha256: Some(sha256),
                        byte_size: Some(profile.bytes.len()),
                        provenance: Some(provenance),
                        applied_mode: ColorManagementMode::UncorrectedMalformedProfile,
                    },
                    warnings,
                    force_cpu: false,
                };
            }
        };

        let mut lut = None;
        let mut force_cpu = false;
        let mut status = ColorManagementStatus::Applied;
        let applied_mode = if backend == TileDecodeBackend::Metal {
            let key = IccProofKey {
                profile_sha256: profile_digest,
                lut_edge: LUT_EDGE,
                max_channel_error: MAX_LUT_CHANNEL_ERROR,
                algorithm_version: LUT_VALIDATION_ALGORITHM_VERSION,
            };
            match process_icc_proof_cache().get_or_compute(key, || {
                let generated = generate_lut(&transform, &sha256);
                match validate_lut_parallel(&profile.bytes, &generated) {
                    Ok(()) => Ok(Arc::new(generated)),
                    Err(IccProofFailure::Validation { .. }) => {
                        // A failed interpolation proof does not prevent exact
                        // GPU lookup over the finite RGB8 input domain.
                        drop(generated);
                        generate_exact_lut(&transform, &sha256).map(Arc::new)
                    }
                    Err(error) => Err(error),
                }
            }) {
                Ok(proved_lut) => {
                    let mode = if proved_lut.edge() == EXACT_LUT_EDGE {
                        // These profiles previously used CPU decoding. Keep
                        // that fast source route and defer only color to Metal;
                        // forcing classic JPEG2000 device decode regresses the
                        // measured 256-pixel workload by roughly two orders.
                        force_cpu = true;
                        ColorManagementMode::MetalExactLut
                    } else {
                        ColorManagementMode::MetalLut65
                    };
                    lut = Some(proved_lut);
                    mode
                }
                Err(IccProofFailure::Validation { max_error }) => {
                    force_cpu = true;
                    status = ColorManagementStatus::LutValidationFailed;
                    warnings.push(format!(
                        "ICC Metal LUT validation reached {max_error} code values; using the direct CPU color path"
                    ));
                    ColorManagementMode::CpuLutValidationFallback
                }
                Err(IccProofFailure::Infrastructure(error)) => {
                    force_cpu = true;
                    status = ColorManagementStatus::LutValidationFailed;
                    warnings.push(format!(
                        "ICC Metal LUT proof infrastructure failed ({error}); using the direct CPU color path"
                    ));
                    ColorManagementMode::CpuLutValidationFallback
                }
            }
        } else {
            ColorManagementMode::CpuLittleCms
        };

        ColorManagementBuild {
            color: Self {
                transform: Some(transform),
                lut,
            },
            summary: ColorManagementSummary {
                status,
                sha256: Some(sha256),
                byte_size: Some(profile.bytes.len()),
                provenance: Some(provenance),
                applied_mode,
            },
            warnings,
            force_cpu,
        }
    }

    pub(crate) fn apply_rgba_tile(&self, tile: &mut RgbaTile) {
        if let Some(transform) = &self.transform {
            transform.transform_rgba(&mut tile.rgba);
        }
    }

    pub(crate) fn prepare_render_tile(&self, tile: RenderTile) -> RenderTile {
        match tile {
            RenderTile::Cpu(mut tile) => {
                #[cfg(target_os = "macos")]
                if let Some(lut) = self.lut.as_ref().filter(|lut| lut.edge() == EXACT_LUT_EDGE) {
                    return RenderTile::CpuWithColorLut {
                        tile,
                        color_lut: Arc::clone(lut),
                    };
                }
                self.apply_rgba_tile(&mut tile);
                RenderTile::Cpu(tile)
            }
            #[cfg(target_os = "macos")]
            RenderTile::Metal(tile) => RenderTile::Metal(tile.with_color_lut(self.lut.clone())),
            #[allow(unreachable_patterns)]
            other => other,
        }
    }
}

pub(crate) struct ColorManagementBuild {
    pub(crate) color: ColorManagement,
    pub(crate) summary: ColorManagementSummary,
    pub(crate) warnings: Vec<String>,
    pub(crate) force_cpu: bool,
}

fn profile_sha256_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn profile_sha256(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn provenance_label(provenance: &IccProfileProvenance) -> String {
    match provenance {
        IccProfileProvenance::TiffTag { ifd_id, tag } => {
            format!("TIFF IFD {ifd_id}, tag {tag}")
        }
        IccProfileProvenance::DicomOpticalPath {
            sop_instance_uid,
            optical_path_identifier,
        } => optical_path_identifier.as_ref().map_or_else(
            || format!("DICOM SOP {sop_instance_uid}"),
            |identifier| format!("DICOM optical path {identifier} in SOP {sop_instance_uid}"),
        ),
        IccProfileProvenance::ReaderMetadata { source } => source.clone(),
        _ => "source metadata".into(),
    }
}

fn generate_lut(transform: &IccTransform, profile_sha256: &str) -> ColorLut3d {
    let edge = LUT_EDGE as usize;
    let mut rgba = Vec::with_capacity(edge * edge * edge * 4);
    for blue in 0..edge {
        for green in 0..edge {
            for red in 0..edge {
                rgba.extend_from_slice(&[
                    lut_axis_value(red),
                    lut_axis_value(green),
                    lut_axis_value(blue),
                    255,
                ]);
            }
        }
    }
    transform.transform_rgba(&mut rgba);
    ColorLut3d::new(LUT_EDGE, rgba, profile_sha256.into())
}

fn lut_axis_value(index: usize) -> u8 {
    ((index as u32 * 255 + (LUT_EDGE - 1) / 2) / (LUT_EDGE - 1)) as u8
}

fn generate_exact_lut(
    transform: &IccTransform,
    profile_sha256: &str,
) -> Result<ColorLut3d, IccProofFailure> {
    const BYTE_LEN: usize = 256 * 256 * 256 * 4;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(BYTE_LEN).map_err(|error| {
        IccProofFailure::Infrastructure(format!("cannot allocate exact RGB8 color table: {error}"))
    })?;
    for blue in 0..=255u8 {
        for green in 0..=255u8 {
            for red in 0..=255u8 {
                rgba.extend_from_slice(&[red, green, blue, 255]);
            }
        }
    }
    // Every possible RGB8 input is transformed directly by the same LittleCMS
    // transform used for CPU tiles. The GPU loads the corresponding texel;
    // there is no interpolation, resampling, or approximation to prove.
    transform.transform_rgba(&mut rgba);
    Ok(ColorLut3d::new(EXACT_LUT_EDGE, rgba, profile_sha256.into()))
}

#[derive(Clone, Copy)]
struct LutAxisSample {
    lower: usize,
    upper: usize,
    lower_weight: u32,
    upper_weight: u32,
}

fn lut_axis_samples(edge: usize) -> [LutAxisSample; 256] {
    std::array::from_fn(|value| {
        let scaled = value * (edge - 1);
        let lower = scaled / 255;
        let upper_weight = (scaled % 255) as u32;
        LutAxisSample {
            lower,
            upper: (lower + 1).min(edge - 1),
            lower_weight: 255 - upper_weight,
            upper_weight,
        }
    })
}

#[cfg(test)]
fn validate_lut(transform: &IccTransform, lut: &ColorLut3d) -> Result<(), u8> {
    validate_lut_range(transform, lut, 0..256)
}

fn icc_validation_worker_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, 4)
}

fn validate_lut_parallel(profile_bytes: &[u8], lut: &ColorLut3d) -> Result<(), IccProofFailure> {
    let worker_count = icc_validation_worker_count();
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let start = worker_index * 256 / worker_count;
            let end = (worker_index + 1) * 256 / worker_count;
            let worker = std::thread::Builder::new()
                .name(format!("icc-proof-{worker_index}"))
                .spawn_scoped(scope, move || {
                    let transform = IccTransform::new(profile_bytes).map_err(|error| {
                        IccProofFailure::Infrastructure(format!(
                            "worker {worker_index} could not create its color transform: {error}"
                        ))
                    })?;
                    validate_lut_range(&transform, lut, start..end)
                        .map_err(|max_error| IccProofFailure::Validation { max_error })
                })
                .map_err(|error| {
                    IccProofFailure::Infrastructure(format!(
                        "could not start ICC proof worker {worker_index}: {error}"
                    ))
                })?;
            workers.push(worker);
        }
        for worker in workers {
            worker.join().map_err(|_| {
                IccProofFailure::Infrastructure("ICC proof worker panicked".into())
            })??;
        }
        Ok(())
    })
}

fn validate_lut_range(
    transform: &IccTransform,
    lut: &ColorLut3d,
    blue_values: Range<usize>,
) -> Result<(), u8> {
    const CODE_VALUES: usize = 256;
    const WEIGHT_SCALE: u32 = 255;
    const TRILINEAR_SCALE: u32 = WEIGHT_SCALE * WEIGHT_SCALE * WEIGHT_SCALE;

    let edge = lut.edge() as usize;
    let axes = lut_axis_samples(edge);
    // Validate one complete blue plane at a time. This covers all 256³ RGB
    // inputs while keeping the LittleCMS working buffer bounded to 256 KiB.
    let mut direct = vec![0_u8; CODE_VALUES * CODE_VALUES * 4];
    let mut blue_plane = vec![[0_u16; 3]; edge * edge];
    let mut green_row = vec![[0_u32; 3]; edge];

    for blue in blue_values {
        let blue_axis = &axes[blue];
        for green in 0..CODE_VALUES {
            for red in 0..CODE_VALUES {
                let offset = (green * CODE_VALUES + red) * 4;
                direct[offset..offset + 4].copy_from_slice(&[
                    red as u8,
                    green as u8,
                    blue as u8,
                    255,
                ]);
            }
        }
        transform.transform_rgba(&mut direct);

        for green_node in 0..edge {
            for red_node in 0..edge {
                let mixed = &mut blue_plane[green_node * edge + red_node];
                for (channel, value) in mixed.iter_mut().enumerate() {
                    let lower =
                        ((blue_axis.lower * edge + green_node) * edge + red_node) * 4 + channel;
                    let upper =
                        ((blue_axis.upper * edge + green_node) * edge + red_node) * 4 + channel;
                    let interpolated = u32::from(lut.rgba()[lower]) * blue_axis.lower_weight
                        + u32::from(lut.rgba()[upper]) * blue_axis.upper_weight;
                    *value = interpolated as u16;
                }
            }
        }

        for (green, green_axis) in axes.iter().enumerate() {
            for (red_node, mixed) in green_row.iter_mut().enumerate() {
                let lower = blue_plane[green_axis.lower * edge + red_node];
                let upper = blue_plane[green_axis.upper * edge + red_node];
                for (channel, value) in mixed.iter_mut().enumerate() {
                    *value = u32::from(lower[channel]) * green_axis.lower_weight
                        + u32::from(upper[channel]) * green_axis.upper_weight;
                }
            }

            for (red, red_axis) in axes.iter().enumerate() {
                let expected_offset = (green * CODE_VALUES + red) * 4;
                for channel in 0..3 {
                    let interpolated = green_row[red_axis.lower][channel] * red_axis.lower_weight
                        + green_row[red_axis.upper][channel] * red_axis.upper_weight;
                    let sampled = ((interpolated + TRILINEAR_SCALE / 2) / TRILINEAR_SCALE) as u8;
                    let error = sampled.abs_diff(direct[expected_offset + channel]);
                    if error > MAX_LUT_CHANNEL_ERROR {
                        return Err(error);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use lcms2::{CIExyY, CIExyYTRIPLE, Profile, ToneCurve};
    use wsi_rs::{
        DatasetId, IccProfileProvenance, SceneId, SeriesId, SourceIccProfile, SourceIccProfileKey,
    };

    use super::*;

    fn selected_view() -> SelectedView {
        SelectedView {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
            plane: wsi_rs::PlaneIdx::default(),
        }
    }

    fn source_profile(
        bytes: Vec<u8>,
        optical_path: Option<usize>,
        source: &str,
    ) -> SourceIccProfile {
        let mut key = SourceIccProfileKey::new(SceneId::new(0), SeriesId::new(0));
        if let Some(optical_path) = optical_path {
            key = key.with_optical_path(optical_path);
        }
        SourceIccProfile::new(
            key,
            bytes,
            IccProfileProvenance::ReaderMetadata {
                source: source.into(),
            },
        )
    }

    fn dataset_with_profiles(profiles: Vec<SourceIccProfile>) -> Dataset {
        let mut dataset = Dataset::new(DatasetId::new(1), Vec::new());
        for profile in profiles {
            dataset.push_source_icc_profile(profile).unwrap();
        }
        dataset
    }

    fn srgb_profile() -> Vec<u8> {
        Profile::new_srgb().icc().unwrap()
    }

    fn gamma_18_rgb_profile() -> Vec<u8> {
        gamma_rgb_profile(1.8)
    }

    pub(super) fn gamma_rgb_profile(gamma: f64) -> Vec<u8> {
        let white = CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let primaries = CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.64,
                y: 0.33,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.30,
                y: 0.60,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.15,
                y: 0.06,
                Y: 1.0,
            },
        };
        let curves = [
            ToneCurve::new(gamma),
            ToneCurve::new(gamma),
            ToneCurve::new(gamma),
        ];
        Profile::new_rgb(&white, &primaries, &[&curves[0], &curves[1], &curves[2]])
            .unwrap()
            .icc()
            .unwrap()
    }

    #[test]
    #[ignore = "release RGB expansion and exact ICC characterization"]
    fn color_cpu_performance() {
        for edge in [64u32, 256, 1024] {
            let rgb: Vec<u8> = (0..edge as usize * edge as usize * 3)
                .map(|i| (i % 251) as u8)
                .collect();
            let expected: Vec<u8> = rgb
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect();
            for (profile_name, profile) in [
                ("unprofiled", None),
                ("srgb", Some(srgb_profile())),
                ("gamma18", Some(gamma_18_rgb_profile())),
            ] {
                let transform = profile
                    .as_ref()
                    .map(|profile| IccTransform::new(profile).unwrap());
                let mut oracle = expected.clone();
                if let Some(transform) = &transform {
                    transform.transform_rgba(&mut oracle);
                }
                for sample in 0..15 {
                    let tiles: Vec<_> = (0..8)
                        .map(|_| {
                            wsi_rs::CpuTile::from_u8_interleaved(
                                edge,
                                edge,
                                3,
                                wsi_rs::ColorSpace::Rgb,
                                rgb.clone(),
                            )
                            .unwrap()
                        })
                        .collect();
                    let started = std::time::Instant::now();
                    let mut rgba: Vec<_> = tiles
                        .into_iter()
                        .map(|tile| crate::tile_output::rgba_tile_from_cpu_tile(tile).unwrap())
                        .collect();
                    let expansion = started.elapsed();
                    assert!(rgba.iter().all(|tile| tile.rgba == expected));
                    let started = std::time::Instant::now();
                    if let Some(transform) = &transform {
                        for tile in &mut rgba {
                            transform.transform_rgba(&mut tile.rgba);
                        }
                    }
                    let icc = started.elapsed();
                    assert!(rgba.iter().all(|tile| tile.rgba == oracle));
                    println!("{{\"workload\":\"color\",\"edge\":{edge},\"batch\":8,\"profile\":\"{profile_name}\",\"sample\":{sample},\"expansion_ms\":{},\"icc_ms\":{}}}", expansion.as_secs_f64()*1000.0, icc.as_secs_f64()*1000.0);
                }
            }
        }
    }

    #[test]
    fn unprofiled_pixels_use_identity_mode() {
        let build = ColorManagement::build(
            &Dataset::new(DatasetId::new(1), Vec::new()),
            selected_view(),
            TileDecodeBackend::Cpu,
        );
        let mut tile = RgbaTile {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
        };

        build.color.apply_rgba_tile(&mut tile);

        assert_eq!(tile.rgba, vec![1, 2, 3, 4]);
        assert_eq!(build.summary, ColorManagementSummary::unprofiled());
    }

    #[test]
    fn valid_srgb_profile_applies_direct_littlecms_without_changing_alpha() {
        let dataset = dataset_with_profiles(vec![source_profile(
            srgb_profile(),
            None,
            "unqualified sRGB",
        )]);
        let build = ColorManagement::build(&dataset, selected_view(), TileDecodeBackend::Cpu);
        let mut tile = RgbaTile {
            width: 2,
            height: 1,
            rgba: vec![12, 34, 56, 78, 200, 150, 100, 42],
        };

        build.color.apply_rgba_tile(&mut tile);

        assert_eq!(tile.rgba[3], 78);
        assert_eq!(tile.rgba[7], 42);
        assert_eq!(build.summary.status, ColorManagementStatus::Applied);
        assert_eq!(
            build.summary.applied_mode,
            ColorManagementMode::CpuLittleCms
        );
    }

    #[test]
    fn linear_rgb_profile_retains_metal_color_conversion() {
        let dataset = dataset_with_profiles(vec![source_profile(
            gamma_rgb_profile(1.0),
            None,
            "linear RGB regression",
        )]);
        let build = ColorManagement::build(&dataset, selected_view(), TileDecodeBackend::Metal);
        assert!(build.force_cpu, "exact color retains CPU source decoding");
        assert_eq!(
            build.summary.applied_mode,
            ColorManagementMode::MetalExactLut
        );
        let lut = build.color.lut.as_deref().unwrap();
        assert_eq!(lut.edge(), 256);
        #[cfg(target_os = "macos")]
        {
            let rgba = vec![1, 2, 3, 127];
            let rendered = build.color.prepare_render_tile(RenderTile::Cpu(RgbaTile {
                width: 1,
                height: 1,
                rgba: rgba.clone(),
            }));
            let RenderTile::CpuWithColorLut { tile, color_lut } = rendered else {
                panic!("color must be deferred to Metal")
            };
            assert_eq!(
                tile.rgba, rgba,
                "source pixels must not be transformed twice"
            );
            assert!(Arc::ptr_eq(&color_lut, build.color.lut.as_ref().unwrap()));
        }
        let inputs = [
            [0, 0, 0, 255],
            [1, 2, 3, 255],
            [129, 124, 0, 255],
            [255, 255, 255, 255],
        ];
        let mut expected = inputs.concat();
        build
            .color
            .transform
            .as_ref()
            .unwrap()
            .transform_rgba(&mut expected);
        for (input, expected) in inputs.iter().zip(expected.as_chunks::<4>().0) {
            let [r, g, b, _] = input.map(usize::from);
            let offset = ((b * 256 + g) * 256 + r) * 4;
            assert_eq!(&lut.rgba()[offset..offset + 4], expected);
        }
    }

    #[test]
    fn non_srgb_profile_changes_pixels_and_metal_lut_matches_direct_output() {
        let dataset = dataset_with_profiles(vec![source_profile(
            gamma_18_rgb_profile(),
            None,
            "gamma 1.8 test profile",
        )]);
        let cold_started = std::time::Instant::now();
        let build = ColorManagement::build(&dataset, selected_view(), TileDecodeBackend::Metal);
        let cold_elapsed = cold_started.elapsed();
        let warm_started = std::time::Instant::now();
        let warm = ColorManagement::build(&dataset, selected_view(), TileDecodeBackend::Metal);
        let warm_elapsed = warm_started.elapsed();
        let mut tile = RgbaTile {
            width: 1,
            height: 1,
            rgba: vec![128, 96, 64, 255],
        };

        build.color.apply_rgba_tile(&mut tile);

        assert_ne!(tile.rgba[..3], [128, 96, 64]);
        assert_eq!(build.summary.applied_mode, ColorManagementMode::MetalLut65);
        let lut = build.color.lut.as_deref().unwrap();
        assert!(Arc::ptr_eq(
            build.color.lut.as_ref().unwrap(),
            warm.color.lut.as_ref().unwrap()
        ));
        validate_lut(build.color.transform.as_deref().unwrap(), lut).unwrap();
        eprintln!(
            "ICC proof timing: cold={:.3}ms warm={:.3}ms",
            cold_elapsed.as_secs_f64() * 1_000.0,
            warm_elapsed.as_secs_f64() * 1_000.0,
        );
    }

    #[test]
    fn lut_validation_checks_values_between_seventeen_code_steps() {
        let transform = IccTransform::new(&srgb_profile()).unwrap();
        let generated = generate_lut(&transform, "sRGB test profile");
        let mut rgba = generated.rgba().to_vec();
        let interior_node = ((2 * LUT_EDGE as usize + 2) * LUT_EDGE as usize + 2) * 4;
        rgba[interior_node] = 255;
        let corrupted = ColorLut3d::new(LUT_EDGE, rgba, "sRGB test profile".into());

        assert!(matches!(
            validate_lut(&transform, &corrupted),
            Err(error) if error > MAX_LUT_CHANNEL_ERROR
        ));
    }

    #[test]
    fn unqualified_profile_wins_over_optical_path_zero_and_warns_on_multiple() {
        let dataset = dataset_with_profiles(vec![
            source_profile(gamma_18_rgb_profile(), Some(0), "optical path zero"),
            source_profile(srgb_profile(), None, "unqualified profile"),
        ]);

        let build = ColorManagement::build(&dataset, selected_view(), TileDecodeBackend::Cpu);

        assert_eq!(
            build.summary.provenance.as_deref(),
            Some("unqualified profile")
        );
        assert!(build
            .warnings
            .iter()
            .any(|warning| warning.contains("2 ICC profiles")));
    }

    #[test]
    fn malformed_profile_is_nonfatal_and_persistently_warned() {
        let dataset = dataset_with_profiles(vec![source_profile(
            vec![1, 2, 3, 4],
            None,
            "malformed fixture",
        )]);

        let build = ColorManagement::build(&dataset, selected_view(), TileDecodeBackend::Metal);

        assert_eq!(
            build.summary.status,
            ColorManagementStatus::MalformedProfile
        );
        assert_eq!(
            build.summary.applied_mode,
            ColorManagementMode::UncorrectedMalformedProfile
        );
        assert!(build.color.transform.is_none());
        assert!(build
            .warnings
            .iter()
            .any(|warning| warning.contains("displaying uncorrected pixels")));
    }

    fn proof_key(byte: u8) -> IccProofKey {
        IccProofKey {
            profile_sha256: [byte; 32],
            lut_edge: LUT_EDGE,
            max_channel_error: MAX_LUT_CHANNEL_ERROR,
            algorithm_version: LUT_VALIDATION_ALGORITHM_VERSION,
        }
    }

    #[test]
    fn same_profile_icc_proof_is_coalesced_and_failure_is_cached() {
        let cache = Arc::new(IccProofCache::new(16));
        let executions = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let executions = Arc::clone(&executions);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                cache.get_or_compute(proof_key(7), || {
                    executions.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    Err(IccProofFailure::Validation { max_error: 9 })
                })
            }));
        }

        for worker in workers {
            assert_eq!(
                worker.join().unwrap().unwrap_err(),
                IccProofFailure::Validation { max_error: 9 }
            );
        }
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(cache.len(), 1);

        let cached = cache.get_or_compute(proof_key(7), || {
            panic!("cached failed proofs must not execute again")
        });
        assert_eq!(
            cached.unwrap_err(),
            IccProofFailure::Validation { max_error: 9 }
        );
    }

    #[test]
    fn icc_proof_capacity_waits_instead_of_forcing_cpu_fallback() {
        let cache = Arc::new(IccProofCache::new(1));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_cache = Arc::clone(&cache);
        let first = std::thread::spawn(move || {
            first_cache.get_or_compute(proof_key(1), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Arc::new(ColorLut3d::new(2, vec![0; 32], "first".into())))
            })
        });
        started_rx.recv().unwrap();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            let result = cache.get_or_compute(proof_key(2), || {
                Ok(Arc::new(ColorLut3d::new(2, vec![0; 32], "second".into())))
            });
            result_tx.send(result).unwrap();
        });
        let early = result_rx.recv_timeout(std::time::Duration::from_millis(50));
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        let result = early.unwrap_or_else(|_| result_rx.recv().unwrap());
        assert_eq!(result.unwrap().profile_sha256(), "second");
        second.join().unwrap();
    }

    #[test]
    fn icc_proof_cache_is_a_bounded_lru() {
        let cache = IccProofCache::new(2);
        let executions = AtomicUsize::new(0);
        let compute = || {
            executions.fetch_add(1, Ordering::SeqCst);
            Err(IccProofFailure::Validation { max_error: 3 })
        };

        cache.get_or_compute(proof_key(1), compute).unwrap_err();
        cache.get_or_compute(proof_key(2), compute).unwrap_err();
        cache
            .get_or_compute(proof_key(1), || panic!("LRU hit must be cached"))
            .unwrap_err();
        cache.get_or_compute(proof_key(3), compute).unwrap_err();
        assert_eq!(cache.len(), 2);
        cache.get_or_compute(proof_key(2), compute).unwrap_err();

        assert_eq!(executions.load(Ordering::SeqCst), 4);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn successful_icc_proof_result_is_cached() {
        let cache = IccProofCache::new(16);
        let executions = AtomicUsize::new(0);
        let build = || {
            executions.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(ColorLut3d::new(
                2,
                vec![0; 2 * 2 * 2 * 4],
                "cached-pass".into(),
            )))
        };

        let first = cache.get_or_compute(proof_key(8), build).unwrap();
        let second = cache
            .get_or_compute(proof_key(8), || panic!("proved LUT must be cached"))
            .unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cold_proof_uses_at_most_four_worker_local_transforms() {
        assert!((1..=4).contains(&icc_validation_worker_count()));
        let profile = srgb_profile();
        let transform = IccTransform::new(&profile).unwrap();
        let generated = generate_lut(&transform, "sRGB parallel proof");
        let mut rgba = generated.rgba().to_vec();
        let interior_node = ((2 * LUT_EDGE as usize + 2) * LUT_EDGE as usize + 2) * 4;
        rgba[interior_node] = 255;
        let corrupted = ColorLut3d::new(LUT_EDGE, rgba, "sRGB parallel proof".into());

        assert!(matches!(
            validate_lut_parallel(&profile, &corrupted),
            Err(IccProofFailure::Validation { max_error })
                if max_error > MAX_LUT_CHANNEL_ERROR
        ));
    }
}

#[cfg(test)]
mod metal_performance_tests;
