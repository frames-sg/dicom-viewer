use eframe::egui;

use super::pathology::PathologyEvent;
use super::DicomViewerApp;

impl DicomViewerApp {
    pub(super) fn poll_pathology_job(&mut self) {
        let Some(event) = self.pathology.poll() else {
            return;
        };
        self.status = match event {
            PathologyEvent::Loaded(session) => {
                let feature_count = session.preview().features().len();
                let detail = format!(
                    "Mapping: {}. {} diagnostic(s). {}",
                    session.mapping_path().display(),
                    session.diagnostic_count(),
                    if session.editable_ann().is_some() {
                        "Lossless vector objects are available for explicit scheme mapping."
                    } else {
                        "No lossless editable vector representation is available."
                    }
                );
                let name = session
                    .geojson_path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Profiled GeoJSON")
                    .to_owned();
                match self
                    .workspace
                    .as_mut()
                    .map(|runtime| runtime.add_external_pathology(name, *session))
                {
                    Some(Ok(_)) => format!(
                        "Loaded and preflighted {feature_count} profiled GeoJSON feature(s). {detail}"
                    ),
                    Some(Err(error)) => {
                        format!("Loaded GeoJSON but could not register its source layer: {error}")
                    }
                    None => "The pathology workspace is unavailable.".into(),
                }
            }
            PathologyEvent::Failed(error) => format!("Failed to import profiled GeoJSON: {error}"),
            PathologyEvent::Disconnected => {
                "Pathology conversion worker exited without returning a result.".into()
            }
        };
    }

    pub(super) fn pick_profiled_geojson(&mut self, ctx: &egui::Context) {
        let Some(context) = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context())
            .cloned()
        else {
            self.status = "Profiled GeoJSON import requires an open VL WSI DICOM source.".into();
            return;
        };
        let directory = context.source_path().parent();
        let mut geojson_dialog = rfd::FileDialog::new().add_filter("GeoJSON", &["geojson", "json"]);
        if let Some(directory) = directory {
            geojson_dialog = geojson_dialog.set_directory(directory);
        }
        let Some(geojson_path) = geojson_dialog.pick_file() else {
            return;
        };
        let mut mapping_dialog = rfd::FileDialog::new()
            .add_filter("DICOM mapping profile", &["json"])
            .set_file_name("pathology-mapping-v1.json");
        if let Some(parent) = geojson_path.parent() {
            mapping_dialog = mapping_dialog.set_directory(parent);
        }
        let Some(mapping_path) = mapping_dialog.pick_file() else {
            self.status = "Profiled GeoJSON import cancelled; no mapping profile selected.".into();
            return;
        };
        match self
            .pathology
            .start_load(context, geojson_path.clone(), mapping_path, ctx)
        {
            Ok(()) => {
                self.status = format!("Validating profiled GeoJSON {}…", geojson_path.display());
            }
            Err(error) => self.status = error,
        }
    }
}
