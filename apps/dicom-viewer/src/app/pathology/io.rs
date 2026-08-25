use dicom_viewer_core::{PathologyAnnotationSet, PathologyCoordinateSpace};

use super::{PathologyInput, PathologySession};
use crate::app::bounded_input::read_bounded;

const MAX_GEOJSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MAPPING_BYTES: u64 = 4 * 1024 * 1024;

pub(super) fn load_pathology(input: PathologyInput) -> Result<PathologySession, String> {
    let annotations = read_annotations(&input)?;
    let semantic_digest = annotations.semantic_sha256();
    let diagnostic_count = annotations.diagnostics().len();
    let editable_ann = annotations.to_ann_with_companion_sr().ok();
    let preview = annotations.into_preview();
    Ok(PathologySession {
        geojson_path: input.geojson_path,
        mapping_path: input.mapping_path,
        semantic_digest,
        preview,
        diagnostic_count,
        editable_ann,
    })
}

fn read_annotations(input: &PathologyInput) -> Result<PathologyAnnotationSet, String> {
    let geojson = read_bounded(&input.geojson_path, MAX_GEOJSON_BYTES, "GeoJSON input")?;
    let mapping = read_bounded(&input.mapping_path, MAX_MAPPING_BYTES, "mapping profile")?;
    PathologyAnnotationSet::from_json(
        &geojson,
        &mapping,
        &input.source,
        &input.source,
        PathologyCoordinateSpace::Level0Pixels,
        false,
    )
    .map_err(|error| error.to_string())
}
