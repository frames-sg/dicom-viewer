use serde::Serialize;

#[derive(Serialize)]
pub(super) struct ImplementationReport {
    pub(super) name: &'static str,
    pub(super) version: &'static str,
}

#[derive(Serialize)]
pub(super) struct FileReport {
    pub(super) path: String,
    pub(super) bytes: u64,
    pub(super) sop_instance_uid: String,
    pub(super) series_instance_uid: String,
}

#[derive(Serialize)]
pub(super) struct RuntimeReport {
    pub(super) parse_ms: f64,
    pub(super) write_ms: f64,
    pub(super) verify_ms: f64,
    pub(super) canonicalize_ms: f64,
    pub(super) peak_tracked_heap_bytes: usize,
}

#[derive(Serialize)]
pub(super) struct SuccessReport {
    pub(super) schema_version: u32,
    pub(super) status: &'static str,
    pub(super) operation: &'static str,
    pub(super) implementation: ImplementationReport,
    pub(super) input: FileReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) output: Option<FileReport>,
    pub(super) payload: &'static str,
    pub(super) semantic: SemanticReport,
    pub(super) diagnostics: Vec<DiagnosticReport>,
    pub(super) runtime: RuntimeReport,
}

#[derive(Serialize)]
pub(super) struct ErrorBody {
    pub(super) code: &'static str,
    pub(super) message: String,
}

#[derive(Serialize)]
pub(super) struct ErrorReport {
    pub(super) schema_version: u32,
    pub(super) status: &'static str,
    pub(super) operation: &'static str,
    pub(super) implementation: ImplementationReport,
    pub(super) error: ErrorBody,
}

#[derive(Serialize)]
#[serde(tag = "object_type", content = "data")]
pub(super) enum SemanticReport {
    Ann(AnnReport),
    Seg(SegReport),
}

#[derive(Serialize)]
pub(super) struct SourceReport {
    pub(super) sop_class_uid: String,
    pub(super) sop_instance_uid: String,
    pub(super) series_instance_uid: String,
    pub(super) study_instance_uid: String,
    pub(super) frame_of_reference_uid: Option<String>,
    pub(super) total_pixel_matrix_columns: u32,
    pub(super) total_pixel_matrix_rows: u32,
    pub(super) tile_columns: u16,
    pub(super) tile_rows: u16,
    pub(super) pixel_spacing: Option<[f64; 2]>,
    pub(super) canonical_total_pixel_matrix_columns: u32,
    pub(super) canonical_total_pixel_matrix_rows: u32,
    pub(super) canonical_pixel_spacing: Option<[f64; 2]>,
}

#[derive(Serialize)]
pub(super) struct ContentReport {
    pub(super) label: String,
    pub(super) description: String,
    pub(super) creator_name: Option<String>,
}

#[derive(Serialize)]
pub(super) struct AnnReport {
    pub(super) sop_instance_uid: String,
    pub(super) series_instance_uid: String,
    pub(super) coordinate_type: String,
    pub(super) pixel_origin_interpretation: Option<String>,
    pub(super) referenced_frame_number: Option<u32>,
    pub(super) content: ContentReport,
    pub(super) source: SourceReport,
    pub(super) groups: Vec<AnnotationGroupReport>,
}

#[derive(Serialize)]
pub(super) struct AnnotationGroupReport {
    pub(super) uid: String,
    pub(super) label: String,
    pub(super) description: String,
    pub(super) generation_type: &'static str,
    pub(super) algorithms: Vec<AlgorithmReport>,
    pub(super) category: CodeReport,
    pub(super) property_type: CodeReport,
    pub(super) property_type_modifiers: Vec<CodeReport>,
    pub(super) anatomic_regions: Vec<CodeReport>,
    pub(super) primary_anatomic_structures: Vec<CodeReport>,
    pub(super) applies_to_all_optical_paths: bool,
    pub(super) referenced_optical_paths: Vec<String>,
    pub(super) applies_to_all_z_planes: bool,
    pub(super) common_z_coordinates_mm: Vec<f64>,
    pub(super) recommended_display_cielab: [u16; 3],
    pub(super) graphic_type: &'static str,
    pub(super) annotation_count: usize,
    pub(super) measurements: Vec<MeasurementReport>,
    pub(super) geometry: GeometryReport,
}

#[derive(Serialize)]
#[serde(tag = "mode")]
pub(super) enum GeometryReport {
    Full {
        native_dimensions: usize,
        canonical_dimensions: usize,
        native_coordinates: Vec<f64>,
        canonical_level0_coordinates: Vec<f64>,
        primitive_point_indices: Vec<u32>,
    },
    Digest {
        native_dimensions: usize,
        canonical_dimensions: usize,
        native_coordinate_count: usize,
        canonical_coordinate_count: usize,
        native_sha256: String,
        canonical_level0_sha256: String,
        primitive_point_indices: Vec<u32>,
    },
}

#[derive(Serialize)]
pub(super) struct MeasurementReport {
    pub(super) concept: CodeReport,
    pub(super) units: CodeReport,
    pub(super) values: Vec<f64>,
    pub(super) annotation_indices: Option<Vec<u32>>,
}

#[derive(Serialize)]
pub(super) struct CodeReport {
    pub(super) value: String,
    pub(super) value_kind: &'static str,
    pub(super) scheme: String,
    pub(super) coding_scheme_version: Option<String>,
    pub(super) meaning: String,
    pub(super) context_identifier: Option<String>,
    pub(super) context_uid: Option<String>,
    pub(super) mapping_resource: Option<String>,
    pub(super) mapping_resource_uid: Option<String>,
    pub(super) context_group_version: Option<String>,
    pub(super) context_group_local_version: Option<String>,
    pub(super) context_group_extension: Option<bool>,
    pub(super) context_group_extension_creator_uid: Option<String>,
}

#[derive(Serialize)]
pub(super) struct AlgorithmReport {
    pub(super) family: CodeReport,
    pub(super) name_code: Option<CodeReport>,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) parameters: Option<String>,
    pub(super) source: Option<String>,
}

#[derive(Serialize)]
pub(super) struct SegReport {
    pub(super) sop_instance_uid: String,
    pub(super) series_instance_uid: String,
    pub(super) segmentation_kind: &'static str,
    pub(super) content: ContentReport,
    pub(super) source: SourceReport,
    pub(super) segments: Vec<SegmentReport>,
    pub(super) masks: SegMaskReport,
}

#[derive(Serialize)]
pub(super) struct SegmentReport {
    pub(super) number: u16,
    pub(super) label: String,
    pub(super) description: String,
    pub(super) generation_type: &'static str,
    pub(super) algorithms: Vec<AlgorithmReport>,
    pub(super) category: CodeReport,
    pub(super) property_type: CodeReport,
    pub(super) property_type_modifiers: Vec<CodeReport>,
    pub(super) tracking_id: Option<String>,
    pub(super) tracking_uid: Option<String>,
    pub(super) anatomic_regions: Vec<CodeReport>,
    pub(super) primary_anatomic_structures: Vec<CodeReport>,
    pub(super) recommended_display_cielab: [u16; 3],
}

#[derive(Serialize)]
#[serde(tag = "mode")]
pub(super) enum SegMaskReport {
    FullBinary {
        sha256: String,
        runs: Vec<BinaryRunReport>,
    },
    FullFractional {
        sha256: String,
        runs: Vec<FractionalRunReport>,
    },
    Digest {
        sha256: String,
        run_count: usize,
    },
}

#[derive(Serialize)]
pub(super) struct BinaryRunReport {
    pub(super) segment_number: u16,
    pub(super) row: u32,
    pub(super) column_start: u32,
    pub(super) length: u32,
}

#[derive(Serialize)]
pub(super) struct FractionalRunReport {
    pub(super) segment_number: u16,
    pub(super) row: u32,
    pub(super) column_start: u32,
    pub(super) maximum_fractional_value: u16,
    pub(super) values: Vec<u16>,
}

#[derive(Serialize)]
pub(super) struct DiagnosticReport {
    pub(super) code: String,
    pub(super) severity: &'static str,
    pub(super) path: String,
    pub(super) disposition: &'static str,
    pub(super) message: String,
}
