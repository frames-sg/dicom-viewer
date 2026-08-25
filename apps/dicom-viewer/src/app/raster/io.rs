use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use dicom_viewer_core::{
    frames_viewer_producer, DicomBundlePublication, ParametricMapDocument, ParametricMapPlan,
    RasterChannelSelection, RasterProfile,
};
use serde::Serialize;

use super::{RasterInput, RasterSession, DEFAULT_MAX_INSTANCE_BYTES};
use crate::app::bounded_input::read_bounded;

const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PREVIEW_PIXELS: usize = 512 * 512;

pub(super) fn load_raster(input: RasterInput) -> Result<RasterSession, String> {
    let document = open_document(&input)?;
    let preview = document
        .preview(0, MAX_PREVIEW_PIXELS)
        .map_err(|error| error.to_string())?;
    Ok(RasterSession {
        input,
        semantic_digest: document.semantic_digest().to_string(),
        preview,
        selected_channel_count: document.selected_channel_count(),
        frame_count: document.frame_count(),
    })
}

fn open_document(input: &RasterInput) -> Result<ParametricMapDocument, String> {
    let profile_bytes = read_bounded(&input.profile_path, MAX_PROFILE_BYTES, "raster profile")?;
    let profile = RasterProfile::from_json(&profile_bytes).map_err(|error| error.to_string())?;
    let document = ParametricMapDocument::open(
        input.source.clone(),
        input.source.clone(),
        profile,
        &input.raster_path,
        RasterChannelSelection::Auto,
    )
    .map_err(|error| error.to_string())?;
    Ok(document.with_producer(
        frames_viewer_producer(9401, "WSI parametric maps").map_err(|error| error.to_string())?,
    ))
}

pub(in crate::app) fn export_raster_cancellable(
    session: &RasterSession,
    destination: &Path,
    cancellation: &AtomicBool,
) -> Result<(PathBuf, usize), String> {
    ensure_not_cancelled(cancellation)?;
    let input = &session.input;
    let document = open_document(input)?;
    ensure_not_cancelled(cancellation)?;
    if document.semantic_digest() != session.semantic_digest() {
        return Err(
            "Raster or profile changed after preview; import it again before exporting.".into(),
        );
    }
    let plan = document
        .plan(DEFAULT_MAX_INSTANCE_BYTES)
        .map_err(|error| error.to_string())?;
    ensure_not_cancelled(cancellation)?;
    let protected = [
        input.source.source_path(),
        input.profile_path.as_path(),
        input.raster_path.as_path(),
    ];
    let publication =
        DicomBundlePublication::new(destination, &protected).map_err(|error| error.to_string())?;
    let names = (1..=plan.parts().len())
        .map(|number| format!("pm-{number:04}.dcm"))
        .collect::<Vec<_>>();
    let staged = names
        .iter()
        .map(|name| publication.staging_path().join(name))
        .collect::<Vec<_>>();
    let instances = document
        .write_planned_parts(&plan, &staged)
        .map_err(|error| error.to_string())?;
    ensure_not_cancelled(cancellation)?;
    if instances.len() != plan.parts().len() {
        return Err("Parametric Map writer returned an unexpected part count".into());
    }
    let manifest =
        ParametricMapManifest::new(input, session.semantic_digest(), &plan, &names, &instances);
    let manifest_path = publication.staging_path().join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("could not encode PM bundle manifest: {error}"))?,
    )
    .map_err(|error| format!("could not write {}: {error}", manifest_path.display()))?;
    publication
        .sync_staged_file(&manifest_path)
        .map_err(|error| error.to_string())?;
    ensure_not_cancelled(cancellation)?;
    publication
        .publish()
        .map(|path| (path, instances.len()))
        .map_err(|error| error.to_string())
}

fn ensure_not_cancelled(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("Parametric Map export cancelled; the destination was not changed.".into())
    } else {
        Ok(())
    }
}

#[derive(Serialize)]
struct ParametricMapManifest<'a> {
    schema: &'static str,
    operation: &'static str,
    source_sop_instance_uid: &'a str,
    semantic_digest: &'a str,
    series_instance_uid: &'a str,
    concatenation_uid: Option<&'a str>,
    outputs: Vec<ParametricMapManifestOutput<'a>>,
}

#[derive(Serialize)]
struct ParametricMapManifestOutput<'a> {
    path: &'a str,
    sop_instance_uid: &'a str,
    frame_offset: u32,
    frame_count: u32,
    pixel_sha256: &'a str,
}

impl<'a> ParametricMapManifest<'a> {
    fn new(
        input: &'a RasterInput,
        semantic_digest: &'a str,
        plan: &'a ParametricMapPlan,
        names: &'a [String],
        instances: &'a [dicom_viewer_core::ParametricMapInstance],
    ) -> Self {
        let outputs = names
            .iter()
            .zip(instances)
            .zip(plan.parts())
            .map(|((name, instance), part)| ParametricMapManifestOutput {
                path: name,
                sop_instance_uid: instance.sop_instance_uid(),
                frame_offset: instance.frame_offset(),
                frame_count: instance.frame_count(),
                pixel_sha256: part.pixel_sha256(),
            })
            .collect();
        Self {
            schema: "viewer-pm-bundle-v1",
            operation: "convert-raster",
            source_sop_instance_uid: input.source.sop_instance_uid(),
            semantic_digest,
            series_instance_uid: plan.series_instance_uid(),
            concatenation_uid: plan.concatenation_uid(),
            outputs,
        }
    }
}
