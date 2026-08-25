use std::path::Path;

use sha2::{Digest, Sha256};
use wsi_dicom_annotations::{
    AlgorithmIdentification, AnnotationDocument, AnnotationGeometry, AnnotationGroup,
    AnnotationMeasurement, DiagnosticDisposition, DiagnosticSeverity, DicomAnnotationContext,
    DicomCode, DicomCodeValueKind, InteroperabilityDiagnostic, SegmentationDocument,
    SegmentationKind, SegmentationSegment,
};

use crate::PEAK_ALLOC;

use super::{schema::*, Arguments, PayloadMode, SCHEMA_VERSION};

pub(super) fn operation_error_report(
    arguments: &Arguments,
    code: &'static str,
    message: String,
) -> ErrorReport {
    ErrorReport {
        schema_version: SCHEMA_VERSION,
        status: "error",
        operation: arguments.operation.label(),
        implementation: implementation_report(),
        error: ErrorBody { code, message },
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn success_report(
    arguments: &Arguments,
    input: FileReport,
    output: Option<FileReport>,
    semantic: SemanticReport,
    diagnostics: Vec<DiagnosticReport>,
    parse_ms: f64,
    write_ms: f64,
    verify_ms: f64,
    canonicalize_ms: f64,
) -> SuccessReport {
    SuccessReport {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        operation: arguments.operation.label(),
        implementation: implementation_report(),
        input,
        output,
        payload: arguments.payload.label(),
        semantic,
        diagnostics,
        runtime: RuntimeReport {
            parse_ms,
            write_ms,
            verify_ms,
            canonicalize_ms,
            peak_tracked_heap_bytes: PEAK_ALLOC.peak_usage(),
        },
    }
}

fn implementation_report() -> ImplementationReport {
    ImplementationReport {
        name: "dicom-viewer",
        version: env!("CARGO_PKG_VERSION"),
    }
}

pub(super) fn build_ann_report(
    document: &AnnotationDocument,
    canonical_source: &DicomAnnotationContext,
    payload: PayloadMode,
) -> Result<AnnReport, String> {
    let mut groups = document.groups().iter().collect::<Vec<_>>();
    groups.sort_by_key(|group| group.uid());
    let groups = groups
        .into_iter()
        .map(|group| build_group_report(document, canonical_source, group, payload))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AnnReport {
        sop_instance_uid: document.sop_instance_uid().to_string(),
        series_instance_uid: document.series_instance_uid().to_string(),
        coordinate_type: document.coordinate_type().to_string(),
        pixel_origin_interpretation: document.pixel_origin_interpretation().map(str::to_string),
        referenced_frame_number: document.referenced_frame_number(),
        content: ContentReport {
            label: document.content_label().to_string(),
            description: document.content_description().to_string(),
            creator_name: document.content_creator_name().map(str::to_string),
        },
        source: source_report(document.source(), canonical_source),
        groups,
    })
}

fn build_group_report(
    document: &AnnotationDocument,
    canonical_source: &DicomAnnotationContext,
    group: &AnnotationGroup,
    payload: PayloadMode,
) -> Result<AnnotationGroupReport, String> {
    let (dimensions, native_coordinates, indices) = flatten_geometry(group.geometry())?;
    let geometry = geometry_report(
        document,
        canonical_source,
        group,
        dimensions,
        native_coordinates,
        indices,
        payload,
    )?;
    Ok(AnnotationGroupReport {
        uid: group.uid().to_string(),
        label: group.label().to_string(),
        description: group.description().to_string(),
        generation_type: group.generation_type().dicom_value(),
        algorithms: group.algorithms().iter().map(algorithm_report).collect(),
        category: code_report(group.category()),
        property_type: code_report(group.property_type()),
        property_type_modifiers: group
            .property_type_modifiers()
            .iter()
            .map(code_report)
            .collect(),
        anatomic_regions: group.anatomic_regions().iter().map(code_report).collect(),
        primary_anatomic_structures: group
            .primary_anatomic_structures()
            .iter()
            .map(code_report)
            .collect(),
        applies_to_all_optical_paths: group.applies_to_all_optical_paths(),
        referenced_optical_paths: group.referenced_optical_paths().to_vec(),
        applies_to_all_z_planes: group.applies_to_all_z_planes(),
        common_z_coordinates_mm: group.common_z_coordinates().to_vec(),
        recommended_display_cielab: group.recommended_display_cielab(),
        graphic_type: group.geometry().graphic_type().dicom_value(),
        annotation_count: group.annotation_count(),
        measurements: group
            .measurements()
            .iter()
            .map(measurement_report)
            .collect(),
        geometry,
    })
}

fn flatten_geometry(geometry: &AnnotationGeometry) -> Result<(usize, Vec<f64>, Vec<u32>), String> {
    match geometry {
        AnnotationGeometry::Points(points) => Ok((
            2,
            points.iter().flat_map(|point| [point.x, point.y]).collect(),
            Vec::new(),
        )),
        AnnotationGeometry::Polygons(polygons) => {
            let mut coordinates = Vec::new();
            let mut indices = Vec::with_capacity(polygons.len());
            for polygon in polygons {
                indices.push(
                    u32::try_from(coordinates.len() + 1)
                        .map_err(|_| "ANN primitive index exceeds u32".to_string())?,
                );
                coordinates.extend(polygon.iter().flat_map(|point| [point.x, point.y]));
            }
            Ok((2, coordinates, indices))
        }
        AnnotationGeometry::ReadOnly {
            coordinates,
            primitive_point_indices,
            coordinate_dimensions,
            ..
        } => Ok((
            *coordinate_dimensions,
            coordinates.clone(),
            primitive_point_indices.clone(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn geometry_report(
    document: &AnnotationDocument,
    canonical_source: &DicomAnnotationContext,
    group: &AnnotationGroup,
    native_dimensions: usize,
    native_coordinates: Vec<f64>,
    primitive_point_indices: Vec<u32>,
    payload: PayloadMode,
) -> Result<GeometryReport, String> {
    let canonical_dimensions = if document.coordinate_type() == "3D" {
        3
    } else {
        2
    };
    match payload {
        PayloadMode::Full => {
            let canonical_coordinates = canonical_coordinate_iter(
                document,
                canonical_source,
                group,
                native_dimensions,
                &native_coordinates,
            )?
            .collect();
            Ok(GeometryReport::Full {
                native_dimensions,
                canonical_dimensions,
                native_coordinates,
                canonical_level0_coordinates: canonical_coordinates,
                primitive_point_indices,
            })
        }
        PayloadMode::Digest => {
            let native_sha256 = digest_f64(native_dimensions, native_coordinates.iter().copied());
            let mut canonical_digest = Sha256::new();
            canonical_digest.update((canonical_dimensions as u64).to_le_bytes());
            let mut canonical_coordinate_count = 0_usize;
            for value in canonical_coordinate_iter(
                document,
                canonical_source,
                group,
                native_dimensions,
                &native_coordinates,
            )? {
                canonical_digest.update(value.to_bits().to_le_bytes());
                canonical_coordinate_count += 1;
            }
            Ok(GeometryReport::Digest {
                native_dimensions,
                canonical_dimensions,
                native_coordinate_count: native_coordinates.len(),
                canonical_coordinate_count,
                native_sha256,
                canonical_level0_sha256: format!("{:x}", canonical_digest.finalize()),
                primitive_point_indices,
            })
        }
    }
}

fn canonical_coordinate_iter(
    document: &AnnotationDocument,
    canonical_source: &DicomAnnotationContext,
    group: &AnnotationGroup,
    native_dimensions: usize,
    native_coordinates: &[f64],
) -> Result<std::vec::IntoIter<f64>, String> {
    let mut values = Vec::new();
    if document.coordinate_type() == "2D" {
        for point in native_coordinates.chunks_exact(2) {
            let canonical = document
                .canonical_level0_pixel(canonical_source, point[0], point[1], None)
                .map_err(|error| format!("coordinate canonicalization failed: {error}"))?;
            values.extend([canonical.x, canonical.y]);
        }
    } else if native_dimensions == 3 {
        for point in native_coordinates.chunks_exact(3) {
            let canonical = document
                .canonical_level0_pixel(canonical_source, point[0], point[1], Some(point[2]))
                .map_err(|error| format!("coordinate canonicalization failed: {error}"))?;
            values.extend([canonical.x, canonical.y, point[2]]);
        }
    } else {
        for point in native_coordinates.chunks_exact(2) {
            for z in group.common_z_coordinates() {
                let canonical = document
                    .canonical_level0_pixel(canonical_source, point[0], point[1], Some(*z))
                    .map_err(|error| format!("coordinate canonicalization failed: {error}"))?;
                values.extend([canonical.x, canonical.y, *z]);
            }
        }
    }
    Ok(values.into_iter())
}

pub(super) fn build_seg_report(
    document: &SegmentationDocument,
    canonical_source: &DicomAnnotationContext,
    payload: PayloadMode,
) -> Result<SegReport, String> {
    let mut segments = document.segments().iter().enumerate().collect::<Vec<_>>();
    segments.sort_by_key(|(index, segment)| {
        segment
            .source_segment_number()
            .unwrap_or_else(|| u16::try_from(index + 1).unwrap_or(u16::MAX))
    });
    let segments = segments
        .into_iter()
        .map(|(index, segment)| segment_report(index, segment))
        .collect();
    let sha256 = document
        .mask_digest()
        .map_err(|error| format!("SEG digest failed: {error}"))?;
    let masks = seg_mask_report(document, payload, sha256)?;
    Ok(SegReport {
        sop_instance_uid: document.sop_instance_uid().to_string(),
        series_instance_uid: document.series_instance_uid().to_string(),
        segmentation_kind: segmentation_kind_label(document.kind()),
        content: ContentReport {
            label: document.content_label().to_string(),
            description: document.content_description().to_string(),
            creator_name: document.content_creator_name().map(str::to_string),
        },
        source: source_report(document.source(), canonical_source),
        segments,
        masks,
    })
}

fn seg_mask_report(
    document: &SegmentationDocument,
    payload: PayloadMode,
    sha256: String,
) -> Result<SegMaskReport, String> {
    if document.kind() == SegmentationKind::Fractional {
        let runs = document
            .fractional_runs()
            .map_err(|error| format!("fractional SEG normalization failed: {error}"))?;
        Ok(match payload {
            PayloadMode::Full => SegMaskReport::FullFractional {
                sha256,
                runs: runs
                    .into_iter()
                    .map(|run| FractionalRunReport {
                        segment_number: run.segment_number(),
                        row: run.row(),
                        column_start: run.column_start(),
                        maximum_fractional_value: run.maximum_fractional_value(),
                        values: run.values().to_vec(),
                    })
                    .collect(),
            },
            PayloadMode::Digest => SegMaskReport::Digest {
                sha256,
                run_count: runs.len(),
            },
        })
    } else {
        let runs = document
            .binary_runs()
            .map_err(|error| format!("binary SEG normalization failed: {error}"))?;
        Ok(match payload {
            PayloadMode::Full => SegMaskReport::FullBinary {
                sha256,
                runs: runs
                    .into_iter()
                    .map(|run| BinaryRunReport {
                        segment_number: run.segment_number(),
                        row: run.row(),
                        column_start: run.column_start(),
                        length: run.length(),
                    })
                    .collect(),
            },
            PayloadMode::Digest => SegMaskReport::Digest {
                sha256,
                run_count: runs.len(),
            },
        })
    }
}

fn segment_report(index: usize, segment: &SegmentationSegment) -> SegmentReport {
    let number = segment
        .source_segment_number()
        .unwrap_or_else(|| u16::try_from(index + 1).unwrap_or(u16::MAX));
    SegmentReport {
        number,
        label: segment.label().to_string(),
        description: segment.description().to_string(),
        generation_type: segment.generation_type().dicom_value(),
        algorithms: if number == 0 {
            Vec::new()
        } else {
            segment.algorithms().iter().map(algorithm_report).collect()
        },
        category: code_report(segment.category()),
        property_type: code_report(segment.property_type()),
        property_type_modifiers: segment
            .property_type_modifiers()
            .iter()
            .map(code_report)
            .collect(),
        tracking_id: segment.tracking_id().map(str::to_string),
        tracking_uid: segment.tracking_uid().map(str::to_string),
        anatomic_regions: segment.anatomic_regions().iter().map(code_report).collect(),
        primary_anatomic_structures: segment
            .primary_anatomic_structures()
            .iter()
            .map(code_report)
            .collect(),
        recommended_display_cielab: segment.recommended_display_cielab(),
    }
}

fn source_report(
    source: &DicomAnnotationContext,
    canonical_source: &DicomAnnotationContext,
) -> SourceReport {
    let (columns, rows) = source.total_pixel_matrix_dimensions();
    let (tile_columns, tile_rows) = source.tile_dimensions();
    let (canonical_columns, canonical_rows) = canonical_source.total_pixel_matrix_dimensions();
    SourceReport {
        sop_class_uid: source.sop_class_uid().to_string(),
        sop_instance_uid: source.sop_instance_uid().to_string(),
        series_instance_uid: source.series_instance_uid().to_string(),
        study_instance_uid: source.study_instance_uid().to_string(),
        frame_of_reference_uid: source.frame_of_reference_uid().map(str::to_string),
        total_pixel_matrix_columns: columns,
        total_pixel_matrix_rows: rows,
        tile_columns,
        tile_rows,
        pixel_spacing: source.pixel_spacing(),
        canonical_total_pixel_matrix_columns: canonical_columns,
        canonical_total_pixel_matrix_rows: canonical_rows,
        canonical_pixel_spacing: canonical_source.pixel_spacing(),
    }
}

fn measurement_report(measurement: &AnnotationMeasurement) -> MeasurementReport {
    MeasurementReport {
        concept: code_report(measurement.concept()),
        units: code_report(measurement.units()),
        values: measurement.values().to_vec(),
        annotation_indices: measurement.annotation_indices().map(<[u32]>::to_vec),
    }
}

fn code_report(code: &DicomCode) -> CodeReport {
    CodeReport {
        value: code.value().to_string(),
        value_kind: match code.value_kind() {
            DicomCodeValueKind::Short => "short",
            DicomCodeValueKind::Long => "long",
            DicomCodeValueKind::Urn => "urn",
        },
        scheme: code.scheme().to_string(),
        coding_scheme_version: code.coding_scheme_version().map(str::to_string),
        meaning: code.meaning().to_string(),
        context_identifier: code.context_identifier().map(str::to_string),
        context_uid: code.context_uid().map(str::to_string),
        mapping_resource: code.mapping_resource().map(str::to_string),
        mapping_resource_uid: code.mapping_resource_uid().map(str::to_string),
        context_group_version: code.context_group_version().map(str::to_string),
        context_group_local_version: code.context_group_local_version().map(str::to_string),
        context_group_extension: code.context_group_extension(),
        context_group_extension_creator_uid: code
            .context_group_extension_creator_uid()
            .map(str::to_string),
    }
}

fn algorithm_report(algorithm: &AlgorithmIdentification) -> AlgorithmReport {
    AlgorithmReport {
        family: code_report(algorithm.family()),
        name_code: algorithm.name_code().map(code_report),
        name: algorithm.name().to_string(),
        version: algorithm.version().to_string(),
        parameters: algorithm.parameters().map(str::to_string),
        source: algorithm.source().map(str::to_string),
    }
}

pub(super) fn diagnostic_reports(
    diagnostics: &[InteroperabilityDiagnostic],
) -> Vec<DiagnosticReport> {
    diagnostics
        .iter()
        .map(|diagnostic| DiagnosticReport {
            code: diagnostic.code().to_string(),
            severity: match diagnostic.severity() {
                DiagnosticSeverity::Info => "info",
                DiagnosticSeverity::Warning => "warning",
                DiagnosticSeverity::Error => "error",
            },
            path: diagnostic.path().to_string(),
            disposition: match diagnostic.disposition() {
                DiagnosticDisposition::Normalized => "normalized",
                DiagnosticDisposition::WouldDrop => "would_drop",
                DiagnosticDisposition::Unsupported => "unsupported",
            },
            message: diagnostic.message().to_string(),
        })
        .collect()
}

pub(super) fn file_report(
    path: &Path,
    bytes: u64,
    sop_instance_uid: &str,
    series_instance_uid: &str,
) -> FileReport {
    FileReport {
        path: path.to_string_lossy().into_owned(),
        bytes,
        sop_instance_uid: sop_instance_uid.to_string(),
        series_instance_uid: series_instance_uid.to_string(),
    }
}

fn segmentation_kind_label(kind: SegmentationKind) -> &'static str {
    match kind {
        SegmentationKind::Binary => "binary",
        SegmentationKind::LabelMap => "labelmap",
        SegmentationKind::Fractional => "fractional",
    }
}

fn digest_f64(dimensions: usize, values: impl IntoIterator<Item = f64>) -> String {
    let mut digest = Sha256::new();
    digest.update((dimensions as u64).to_le_bytes());
    for value in values {
        digest.update(value.to_bits().to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}
