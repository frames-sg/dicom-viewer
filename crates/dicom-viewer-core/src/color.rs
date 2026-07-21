use std::sync::{Arc, Mutex};

use lcms2::{DisallowCache, Flags, Intent, PixelFormat, Profile, ThreadContext, Transform};
use sha2::{Digest, Sha256};
use wsi_rs::{Dataset, IccProfileProvenance};

use crate::model::SelectedView;
use crate::{
    ColorLut3d, ColorManagementMode, ColorManagementStatus, ColorManagementSummary, RenderTile,
    RgbaTile, TileDecodeBackend,
};

const LUT_EDGE: u32 = 65;
const MAX_LUT_CHANNEL_ERROR: u8 = 2;

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

        let sha256 = profile_sha256(&profile.bytes);
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
            let generated = generate_lut(&transform, &sha256);
            match validate_lut(&transform, &generated) {
                Ok(()) => {
                    lut = Some(Arc::new(generated));
                    ColorManagementMode::MetalLut65
                }
                Err(max_error) => {
                    force_cpu = true;
                    status = ColorManagementStatus::LutValidationFailed;
                    warnings.push(format!(
                        "ICC Metal LUT validation reached {max_error} code values; using the direct CPU color path"
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

fn profile_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
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

fn validate_lut(transform: &IccTransform, lut: &ColorLut3d) -> Result<(), u8> {
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

    for (blue, blue_axis) in axes.iter().enumerate() {
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
            ToneCurve::new(1.8),
            ToneCurve::new(1.8),
            ToneCurve::new(1.8),
        ];
        Profile::new_rgb(&white, &primaries, &[&curves[0], &curves[1], &curves[2]])
            .unwrap()
            .icc()
            .unwrap()
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
    fn non_srgb_profile_changes_pixels_and_metal_lut_matches_direct_output() {
        let dataset = dataset_with_profiles(vec![source_profile(
            gamma_18_rgb_profile(),
            None,
            "gamma 1.8 test profile",
        )]);
        let build = ColorManagement::build(&dataset, selected_view(), TileDecodeBackend::Metal);
        let mut tile = RgbaTile {
            width: 1,
            height: 1,
            rgba: vec![128, 96, 64, 255],
        };

        build.color.apply_rgba_tile(&mut tile);

        assert_ne!(tile.rgba[..3], [128, 96, 64]);
        assert_eq!(build.summary.applied_mode, ColorManagementMode::MetalLut65);
        let lut = build.color.lut.as_deref().unwrap();
        validate_lut(build.color.transform.as_deref().unwrap(), lut).unwrap();
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
}
