mod dataset;
mod dicom;

pub(crate) use dataset::summarize_slide;
pub(crate) use dicom::inspect_input;
#[cfg(test)]
pub(crate) use dicom::open_metadata_object;

#[cfg(test)]
pub(crate) use dataset::{
    canonical_canvas_dimensions, select_primary_view, summarize_renderable_levels,
};
#[cfg(test)]
pub(crate) use dicom::{
    build_fact_warnings, candidate_paths_with_limit, MAX_METADATA_ELEMENT_BYTES,
    MAX_METADATA_SEQUENCE_DEPTH, MAX_METADATA_VALUE_BYTES,
};
