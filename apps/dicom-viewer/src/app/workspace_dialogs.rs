use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dicom_viewer_core::{AnnotationClassGeometry, AnnotationScheme};
use eframe::egui::{self, RichText};

use super::bounded_input::read_bounded;
use super::ui::pathology_workspace::PathologyWorkspaceActions;
use super::DicomViewerApp;

const MAX_SCHEME_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(in crate::app) struct PendingSchemeMigration {
    target: AnnotationScheme,
    mappings: BTreeMap<String, String>,
}

impl DicomViewerApp {
    pub(super) fn show_workspace_dialogs(&mut self, ctx: &egui::Context) {
        self.show_restore_dialog(ctx);
        self.show_import_dialog(ctx);
        self.show_export_dialog(ctx);
        self.show_scheme_library_dialog(ctx);
        self.show_scheme_migration_dialog(ctx);
        self.show_storage_dialog(ctx);
    }

    fn show_restore_dialog(&mut self, ctx: &egui::Context) {
        let Some(restored) = self.pending_restore.as_ref() else {
            return;
        };
        let object_count = restored.document().object_count();
        let revision = restored.revision();
        let age = human_age(restored.saved_unix_ms());
        let has_draft = restored.draft().is_some();
        let mut restore = false;
        let mut fresh = false;
        egui::Window::new("Restore pathology workspace")
            .id(egui::Id::new("restore-pathology-workspace"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Revision {revision}, saved {age}, contains {object_count} tracked object(s){}.",
                    if has_draft { " and an unfinished polygon" } else { "" }
                ));
                ui.label(
                    RichText::new("Restore is recommended and does not reinstall the embedded scheme globally.")
                        .small()
                        .weak(),
                );
                ui.horizontal(|ui| {
                    restore = ui.button("Restore").clicked();
                    fresh = ui.button("Start fresh").clicked();
                });
            });
        if restore {
            self.restore_saved_workspace();
        } else if fresh {
            self.start_fresh_workspace();
        }
    }

    fn show_import_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_import_wizard {
            return;
        }
        let mut open = true;
        let mut action = PathologyWorkspaceActions::default();
        let discovered = self
            .study
            .as_ref()
            .map(|study| study.sidecars().to_vec())
            .unwrap_or_default();
        let mut selected_sidecar = None;
        egui::Window::new("Import into Pathology")
            .id(egui::Id::new("pathology-import-wizard"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Imported results stay read-only until they are explicitly mapped and promoted.");
                ui.separator();
                action.import_dicom = ui.button("Discovered or selected DICOM ANN / SEG…").clicked();
                action.import_profiled_geojson =
                    ui.button("Profiled GeoJSON + class mapping…").clicked();
                action.import_sr = ui.button("DICOM SR…").clicked();
                action.import_sr_with_seg = ui.button("DICOM SR with companion SEG…").clicked();
                action.import_raster = ui.button("Raster mask or heatmap…").clicked();
                if !discovered.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Discovered same-folder DICOM sidecars").strong());
                    for sidecar in &discovered {
                        let label = format!(
                            "{:?} · {}",
                            sidecar.kind(),
                            sidecar
                                .path()
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("sidecar.dcm")
                        );
                        if ui.button(label).clicked() {
                            selected_sidecar = Some((sidecar.path().to_path_buf(), sidecar.kind()));
                        }
                    }
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Parametric Map import is not advertised until a PM reader is available.").small().weak());
            });
        let acted = selected_sidecar.is_some()
            || action.import_dicom
            || action.import_profiled_geojson
            || action.import_sr
            || action.import_sr_with_seg
            || action.import_raster;
        self.show_import_wizard = open && !acted;
        if let Some((path, kind)) = selected_sidecar {
            if kind == dicom_viewer_core::SidecarKind::StructuredReport {
                self.start_structured_report_path(path, None, ctx);
            } else {
                self.start_annotation_load(path, Some(kind), ctx);
            }
        } else if acted {
            self.handle_pathology_workspace_actions(action, ctx);
        }
    }

    fn show_export_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_export_wizard {
            return;
        }
        let Some(runtime) = self.workspace.as_ref() else {
            self.show_export_wizard = false;
            return;
        };
        let vectors = runtime.document().vector_findings().count();
        let vector_regions = runtime
            .document()
            .vector_findings()
            .filter(|finding| {
                matches!(
                    finding.geometry(),
                    dicom_viewer_core::VectorFindingGeometry::Regions(_)
                )
            })
            .count();
        let segments = runtime.document().segments().count();
        let rulers = runtime.document().measurements().len();
        let dicom_context = self
            .study
            .as_ref()
            .and_then(|study| study.annotation_context());
        let dicom_available = dicom_context.is_some();
        let reliable_spacing = self.study.as_ref().is_some_and(|study| {
            study
                .summary()
                .mpp
                .is_some_and(|(x, y)| x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0)
        });
        let sr_available = dicom_available
            && rulers > 0
            && reliable_spacing
            && dicom_context.is_some_and(|context| context.frame_of_reference_uid().is_some());
        let heatmap_available = runtime.document().external_layers().iter().any(|layer| {
            matches!(
                runtime.external_payload(layer.id()),
                Some(super::workspace::ExternalLayerPayload::Heatmap { .. })
            )
        });
        let compatibility_available = runtime.document().scheme().content_digest()
            == AnnotationScheme::tumor_mask_compatibility_v1().content_digest();
        let mut open = true;
        let mut action = PathologyWorkspaceActions::default();
        egui::Window::new("Export Pathology")
            .id(egui::Id::new("pathology-export-wizard"))
            .open(&mut open)
            .default_width(430.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Preflight: {vectors} vector finding(s), {segments} segment(s), {rulers} ruler(s)."
                ));
                ui.separator();
                action.export_portable_workspace = ui.button("Portable workspace…").clicked();
                action.export_scheme_geojson = ui.button("Scheme-aware GeoJSON…").clicked();
                action.export_compatibility_geojson = ui
                    .add_enabled(compatibility_available, egui::Button::new("CellViT viable-tumor compatibility GeoJSON…"))
                    .on_disabled_hover_text("Requires the pinned Tumor Mask Compatibility v1 scheme.")
                    .clicked();
                ui.separator();
                action.export_ann = ui
                    .add_enabled(dicom_available && vectors > 0, egui::Button::new("Vector annotations (ANN)…"))
                    .on_hover_text("Exports directly representable points and independent simple polygons only.")
                    .clicked();
                action.export_compatibility_ann = ui
                    .add_enabled(
                        dicom_available && compatibility_available && (vectors > 0 || segments > 0),
                        egui::Button::new("Tumor mask compatibility ANN (VIABLE_TUMOR / EXCLUSION)…"),
                    )
                    .on_disabled_hover_text("Requires Tumor Mask Compatibility v1 and tracked tumor-mask content.")
                    .clicked();
                ui.horizontal(|ui| {
                    action.export_seg = ui
                        .add_enabled(
                            dicom_available && (segments > 0 || (self.seg_rasterize_vectors && vector_regions > 0)),
                            egui::Button::new("Segmentation mask (SEG)…"),
                        )
                        .clicked();
                    ui.checkbox(
                        &mut self.seg_rasterize_vectors,
                        "Rasterize vector findings into SEG",
                    );
                });
                action.export_sr = ui
                    .add_enabled(sr_available, egui::Button::new("Measurement report (SR)…"))
                    .on_disabled_hover_text("Requires rulers, physical spacing, and DICOM frame identity.")
                    .clicked();
                action.export_pm = ui
                    .add_enabled(
                        dicom_available && heatmap_available,
                        egui::Button::new("Existing heatmap source (PM)…"),
                    )
                    .on_disabled_hover_text("Requires a loaded profiled heatmap source.")
                    .clicked();
                if segments > 0 {
                    ui.label(RichText::new("ANN excludes segmentation content; it is never converted silently.").small().weak());
                }
                if rulers > 0 {
                    ui.label(RichText::new("GeoJSON excludes rulers only after explicit eligible-items confirmation.").small().weak());
                }
            });
        let acted = action.export_portable_workspace
            || action.export_scheme_geojson
            || action.export_compatibility_geojson
            || action.export_ann
            || action.export_compatibility_ann
            || action.export_seg
            || action.export_sr
            || action.export_pm;
        self.show_export_wizard = open && !acted;
        if acted {
            self.handle_pathology_workspace_actions(action, ctx);
        }
    }

    fn show_scheme_library_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_scheme_settings {
            return;
        }
        let schemes = self
            .scheme_library
            .entries()
            .map(|(scheme, built_in)| (scheme.clone(), built_in))
            .collect::<Vec<_>>();
        let current_digest = self
            .workspace
            .as_ref()
            .map(|runtime| runtime.document().scheme().content_digest().to_owned());
        let mut open = true;
        let mut install = false;
        let mut switch_to = None;
        let mut remove = None;
        egui::Window::new("Annotation Schemes")
            .id(egui::Id::new("annotation-scheme-library"))
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label("Schemes are versioned controlled terminology. Built-ins are immutable; this screen intentionally has no class editor.");
                if ui.button("Install validated JSON…").clicked() {
                    install = true;
                }
                ui.separator();
                for (scheme, built_in) in &schemes {
                    let pinned = current_digest.as_deref() == Some(scheme.content_digest());
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(scheme.display_name()).strong());
                            ui.label(format!("v{}", scheme.version()));
                            ui.label(
                                RichText::new(if *built_in { "BUILT-IN" } else { "PROJECT" })
                                    .small()
                                    .weak(),
                            );
                            if pinned {
                                ui.label(RichText::new("PINNED").small().strong());
                            } else if ui.button("Use…").clicked() {
                                switch_to = Some(scheme.clone());
                            }
                            if !built_in && ui.button("Remove…").clicked() {
                                remove = Some((scheme.id().to_owned(), scheme.version()));
                            }
                        });
                        ui.monospace(format!("{} · {}", scheme.id(), scheme.content_digest()));
                        ui.collapsing(format!("{} controlled classes", scheme.classes().len()), |ui| {
                            for class in scheme.classes() {
                                ui.label(format!(
                                    "{} · {} · {}:{} / {}:{}",
                                    class.label(),
                                    class.geometry().label(),
                                    class.category().scheme(),
                                    class.category().value(),
                                    class.property_type().scheme(),
                                    class.property_type().value()
                                ));
                            }
                        });
                    });
                }
            });
        self.show_scheme_settings = open;
        if install {
            self.install_annotation_scheme();
        }
        if let Some(target) = switch_to {
            self.begin_scheme_switch(target);
        }
        if let Some((id, version)) = remove {
            let confirmed = rfd::MessageDialog::new()
                .set_title("Remove annotation scheme")
                .set_description("Remove this installed project scheme? Stored workspaces that reference it will block removal.")
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes;
            if confirmed {
                let mut references = self
                    .revision_store
                    .as_ref()
                    .map(|store| store.referenced_scheme_digests())
                    .transpose()
                    .unwrap_or_else(|error| {
                        self.status =
                            format!("Could not inspect stored workspace references: {error}");
                        None
                    })
                    .unwrap_or_default();
                if let Some(runtime) = &self.workspace {
                    references.insert(runtime.document().scheme().content_digest().to_owned());
                }
                self.status =
                    match self
                        .scheme_library
                        .remove_project_scheme(&id, version, &references)
                    {
                        Ok(()) => format!("Removed project annotation scheme {id} v{version}."),
                        Err(error) => error.to_string(),
                    };
            }
        }
    }

    fn install_annotation_scheme(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Annotation scheme v1", &["json"])
            .pick_file()
        else {
            return;
        };
        let bytes = match read_bounded(&path, MAX_SCHEME_BYTES, "annotation scheme") {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        match self.scheme_library.install_json(&bytes) {
            Ok(super::workspace::SchemeInstallOutcome::Installed) => {
                self.status = format!("Installed annotation scheme from {}.", path.display())
            }
            Ok(super::workspace::SchemeInstallOutcome::AlreadyInstalled) => {
                self.status = "Identical annotation scheme content is already installed.".into()
            }
            Err(error) => self.status = format!("Annotation scheme rejected: {error}"),
        }
    }

    fn begin_scheme_switch(&mut self, target: AnnotationScheme) {
        let Some(runtime) = self.workspace.as_mut() else {
            return;
        };
        if runtime.document().object_count() == 0 {
            let result = runtime.migrate_scheme(target, &BTreeMap::new());
            self.status = match result {
                Ok(()) => "Pinned the empty workspace to the selected annotation scheme.".into(),
                Err(error) => error.to_string(),
            };
            return;
        }
        self.pending_scheme_migration = Some(PendingSchemeMigration {
            mappings: runtime.document().suggest_scheme_migration(&target),
            target,
        });
    }

    fn show_scheme_migration_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.pending_scheme_migration.take() else {
            return;
        };
        let Some(runtime) = self.workspace.as_ref() else {
            return;
        };
        let used = used_classes(runtime.document());
        let source_scheme = runtime.document().scheme().clone();
        let mut open = true;
        let mut commit = false;
        egui::Window::new("Map annotation scheme")
            .id(egui::Id::new("annotation-scheme-migration"))
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Map every used class from {} to {}. Exact concept matches are suggested.",
                    source_scheme.display_name(),
                    pending.target.display_name()
                ));
                for (source_id, geometry) in &used {
                    let source_label = source_scheme
                        .class(source_id)
                        .map_or(source_id.as_str(), |class| class.label());
                    ui.horizontal(|ui| {
                        ui.label(format!("{source_label} ({})", geometry.label()));
                        let selected = pending
                            .mappings
                            .get(source_id)
                            .and_then(|id| pending.target.class(id))
                            .map_or("Choose…", |class| class.label());
                        egui::ComboBox::from_id_salt(("migration", source_id))
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for class in pending
                                    .target
                                    .classes()
                                    .iter()
                                    .filter(|class| class.geometry() == *geometry)
                                {
                                    if ui
                                        .selectable_label(
                                            pending.mappings.get(source_id).map(String::as_str)
                                                == Some(class.id()),
                                            class.label(),
                                        )
                                        .clicked()
                                    {
                                        pending
                                            .mappings
                                            .insert(source_id.clone(), class.id().to_owned());
                                    }
                                }
                            });
                    });
                }
                let complete = used.iter().all(|(source, geometry)| {
                    pending
                        .mappings
                        .get(source)
                        .and_then(|target| pending.target.class(target))
                        .is_some_and(|class| class.geometry() == *geometry)
                });
                commit = ui
                    .add_enabled(complete, egui::Button::new("Commit migration"))
                    .clicked();
                ui.label(RichText::new("The complete migration is one undoable command. No data is reclassified until commit.").small().weak());
            });
        if commit {
            let target = pending.target.clone();
            let mappings = pending.mappings.clone();
            let result = self
                .workspace
                .as_mut()
                .expect("workspace existed while rendering migration")
                .migrate_scheme(target, &mappings);
            self.status = match result {
                Ok(()) => "Annotation scheme migration committed.".into(),
                Err(error) => {
                    self.pending_scheme_migration = Some(pending);
                    format!("Scheme migration failed: {error}")
                }
            };
        } else if open {
            self.pending_scheme_migration = Some(pending);
        }
    }

    fn show_storage_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_storage_settings {
            return;
        }
        let usage = self
            .revision_store
            .as_ref()
            .and_then(|store| store.storage_usage_bytes().ok())
            .unwrap_or(0);
        let root = self
            .revision_store
            .as_ref()
            .map(|store| store.root().display().to_string())
            .unwrap_or_else(|| "Unavailable".into());
        let mut open = true;
        let mut export = false;
        let mut purge = false;
        let mut delete_source = false;
        let mut delete_all = false;
        egui::Window::new("Workspace Storage")
            .id(egui::Id::new("workspace-storage"))
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(format!("Usage: {}", format_bytes(usage)));
                ui.monospace(root);
                ui.label(RichText::new("Source keys contain no patient names. Portable workspaces and linked-layer metadata may contain local paths and annotation content.").small().weak());
                export = ui.button("Export current portable workspace…").clicked();
                purge = ui.button("Purge archives older than 30 days").clicked();
                ui.separator();
                delete_source = ui.button("Delete drafts for this source…").clicked();
                delete_all = ui.button("Delete all workspace drafts…").clicked();
            });
        self.show_storage_settings = open;
        if export {
            self.export_portable_workspace(ctx);
        }
        if purge {
            match self.revision_store.as_ref().map(|store| {
                store.purge_archives_older_than(Duration::from_secs(30 * 24 * 60 * 60))
            }) {
                Some(Ok(count)) => {
                    self.status = format!("Purged {count} expired workspace archive(s).")
                }
                Some(Err(error)) => self.status = error.to_string(),
                None => self.status = "Workspace storage is unavailable.".into(),
            }
        }
        if delete_source
            && confirm_delete("Delete every saved revision and archive for this source?")
        {
            let result = self
                .workspace
                .as_ref()
                .zip(self.revision_store.as_ref())
                .map(|(runtime, store)| {
                    store.delete_source_workspace(runtime.document().source_identity())
                });
            self.status = match result {
                Some(Ok(true)) => {
                    "Deleted stored drafts for this source. The open document remains in memory."
                        .into()
                }
                Some(Ok(false)) => "No stored drafts existed for this source.".into(),
                Some(Err(error)) => error.to_string(),
                None => "Workspace storage is unavailable.".into(),
            };
        }
        if delete_all
            && confirm_delete("Delete all saved workspace revisions and archives for every source?")
        {
            self.status = match self
                .revision_store
                .as_ref()
                .map(|store| store.delete_all_workspaces())
            {
                Some(Ok(true)) => {
                    "Deleted all stored workspace drafts. Open documents remain in memory.".into()
                }
                Some(Ok(false)) => "No stored workspace drafts existed.".into(),
                Some(Err(error)) => error.to_string(),
                None => "Workspace storage is unavailable.".into(),
            };
        }
    }
}

fn used_classes(
    document: &dicom_viewer_core::WorkspaceDocument,
) -> BTreeMap<String, AnnotationClassGeometry> {
    let mut used = BTreeMap::new();
    for finding in document.vector_findings() {
        let geometry = match finding.geometry() {
            dicom_viewer_core::VectorFindingGeometry::Point(_) => AnnotationClassGeometry::Point,
            dicom_viewer_core::VectorFindingGeometry::Regions(_) => AnnotationClassGeometry::Region,
        };
        used.insert(finding.class_id().to_owned(), geometry);
    }
    for segment in document.segments() {
        used.insert(
            segment.class_id().to_owned(),
            AnnotationClassGeometry::Region,
        );
    }
    for measurement in document.measurements() {
        used.insert(
            measurement.class_id().to_owned(),
            AnnotationClassGeometry::Region,
        );
    }
    used
}

fn human_age(saved_unix_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let seconds = now.saturating_sub(saved_unix_ms) / 1_000;
    match seconds {
        0..=59 => "less than a minute ago".into(),
        60..=3_599 => format!("{} minute(s) ago", seconds / 60),
        3_600..=86_399 => format!("{} hour(s) ago", seconds / 3_600),
        _ => format!("{} day(s) ago", seconds / 86_400),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{bytes} B")
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1} KiB", bytes as f64 / 1_024.0)
    } else if bytes < 1_024 * 1_024 * 1_024 {
        format!("{:.1} MiB", bytes as f64 / (1_024.0 * 1_024.0))
    } else {
        format!("{:.2} GiB", bytes as f64 / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

fn confirm_delete(message: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title("Delete workspace drafts")
        .set_description(message)
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_labels_are_stable_and_human_readable() {
        assert_eq!(format_bytes(1_024), "1.0 KiB");
        assert_eq!(format_bytes(2 * 1_024 * 1_024), "2.0 MiB");
    }
}
