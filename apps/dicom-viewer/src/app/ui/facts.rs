use std::collections::BTreeSet;
use std::sync::Arc;

use dicom_viewer_core::{DicomInstanceSummary, SourceKind, StudySummary};
use eframe::egui::{
    self, vec2, Color32, CornerRadius, Frame, Label, Margin, Panel, RichText, Sense, Stroke,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageWindow {
    page: usize,
    page_count: usize,
    start: usize,
    end: usize,
}

const INSTANCE_PAGE_SIZE: usize = 50;
const WARNING_PAGE_SIZE: usize = 20;
const TRANSFER_SYNTAX_PAGE_SIZE: usize = 20;

fn page_window(total: usize, requested_page: usize, page_size: usize) -> PageWindow {
    let page_size = page_size.max(1);
    let page_count = total.div_ceil(page_size).max(1);
    let page = requested_page.min(page_count - 1);
    let start = page.saturating_mul(page_size).min(total);
    PageWindow {
        page,
        page_count,
        start,
        end: start.saturating_add(page_size).min(total),
    }
}

#[derive(Clone)]
struct TransferSyntaxCache {
    generation: u64,
    values: Arc<[String]>,
}

fn cached_transfer_syntaxes(
    ui: &egui::Ui,
    generation: u64,
    instances: &[DicomInstanceSummary],
) -> Arc<[String]> {
    let id = ui.make_persistent_id("facts-transfer-syntax-cache");
    ui.data_mut(|data| {
        if let Some(cache) = data.get_temp::<TransferSyntaxCache>(id) {
            if cache.generation == generation {
                return cache.values;
            }
        }
        let values = Arc::<[String]>::from(
            instances
                .iter()
                .map(|instance| instance.transfer_syntax_uid.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
        );
        data.insert_temp(
            id,
            TransferSyntaxCache {
                generation,
                values: Arc::clone(&values),
            },
        );
        values
    })
}

fn paginated_window(ui: &mut egui::Ui, id: egui::Id, total: usize, page_size: usize) -> PageWindow {
    let requested_page = ui.data(|data| data.get_temp::<usize>(id).unwrap_or(0));
    let mut window = page_window(total, requested_page, page_size);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(window.page > 0, egui::Button::new("Previous"))
            .clicked()
        {
            window = page_window(total, window.page - 1, page_size);
        }
        ui.label(format!("Page {} of {}", window.page + 1, window.page_count));
        if ui
            .add_enabled(
                window.page + 1 < window.page_count,
                egui::Button::new("Next"),
            )
            .clicked()
        {
            window = page_window(total, window.page + 1, page_size);
        }
    });
    ui.data_mut(|data| data.insert_temp(id, window.page));
    window
}

use super::super::{format::format_integer, theme};
use super::chrome::chrome_frame;

pub(in crate::app) fn show_facts_sidebar(
    ui: &mut egui::Ui,
    visible: bool,
    study_generation: u64,
    summary: Option<&StudySummary>,
) {
    if !visible {
        return;
    }
    Panel::left("facts")
        .resizable(true)
        .default_size(300.0)
        .size_range(252.0..=460.0)
        .frame(chrome_frame(theme::CHROME, Margin::same(0)))
        .show_inside(ui, |ui| {
            if let Some(summary) = summary {
                facts_panel(ui, study_generation, summary);
            } else {
                empty_facts(ui);
            }
        });
}

fn section_header(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .color(theme::TEXT_DIM)
            .size(10.5)
            .strong(),
    );
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 7.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.top() + 3.0,
        Stroke::new(1.0, theme::HAIRLINE_SOFT),
    );
}

fn kv_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(key).color(theme::TEXT_DIM).size(11.5));
        ui.add(
            Label::new(
                RichText::new(value)
                    .color(theme::TEXT)
                    .monospace()
                    .size(11.5),
            )
            .wrap(),
        );
    });
}

fn optional_kv_row<T: std::fmt::Display>(ui: &mut egui::Ui, key: &str, value: Option<T>) {
    if let Some(value) = value {
        kv_row(ui, key, &value.to_string());
    }
}

fn warning_row(ui: &mut egui::Ui, text: &str) {
    Frame::NONE
        .fill(Color32::from_rgb(36, 28, 15))
        .inner_margin(Margin::same(8))
        .corner_radius(CornerRadius::same(5))
        .outer_margin(Margin {
            left: 0,
            right: 0,
            top: 0,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                ui.label(RichText::new("\u{26A0}").color(theme::WARN).size(12.0));
                ui.add(
                    Label::new(
                        RichText::new(text)
                            .color(Color32::from_rgb(214, 178, 120))
                            .size(11.5),
                    )
                    .wrap(),
                );
            });
        });
}

pub(in crate::app) fn empty_facts(ui: &mut egui::Ui) {
    Frame::NONE
        .inner_margin(Margin::symmetric(16, 16))
        .show(ui, |ui| {
            section_header(ui, "No slide loaded");
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Open a whole-slide image file, or a folder of DICOM instances, to inspect its resolution pyramid.",
                )
                .color(theme::TEXT_MUTED)
                .size(12.5),
            );
        });
}

pub(in crate::app) fn facts_panel(
    ui: &mut egui::Ui,
    study_generation: u64,
    summary: &StudySummary,
) {
    Frame::NONE
        .fill(theme::CHROME_RAISED)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.label(
                RichText::new(&summary.format_label)
                    .color(theme::TEXT)
                    .size(14.5)
                    .strong(),
            );
            let name = summary
                .source_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(summary.format_label.as_str());
            ui.add(
                Label::new(
                    RichText::new(name)
                        .color(theme::TEXT_MUTED)
                        .monospace()
                        .size(11.5),
                )
                .wrap(),
            );
        });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(14, 6))
                .show(ui, |ui| {
                    section_header(ui, "Source");
                    kv_row(
                        ui,
                        "Kind",
                        match summary.source_kind {
                            SourceKind::File => "file",
                            SourceKind::Folder => "folder",
                        },
                    );
                    kv_row(ui, "Files", &format_integer(summary.file_count as u64));
                    if summary.dicom_instance_count > 0 {
                        kv_row(ui, "Instances", &summary.dicom_instance_count.to_string());
                    }
                    kv_row(ui, "Tile output", &summary.tile_decode_backend.to_string());

                    section_header(ui, "Color management");
                    kv_row(ui, "Status", &summary.color_management.status.to_string());
                    kv_row(
                        ui,
                        "Mode",
                        &summary.color_management.applied_mode.to_string(),
                    );
                    optional_kv_row(
                        ui,
                        "Profile bytes",
                        summary
                            .color_management
                            .byte_size
                            .map(|bytes| format_integer(bytes as u64)),
                    );
                    optional_kv_row(
                        ui,
                        "Provenance",
                        summary.color_management.provenance.as_deref(),
                    );
                    if let Some(sha256) = &summary.color_management.sha256 {
                        kv_row(ui, "SHA-256", sha256);
                    }

                    if summary.mpp.is_some() || summary.objective_power.is_some() {
                        section_header(ui, "Optics");
                        if let Some((x, y)) = summary.mpp {
                            kv_row(ui, "MPP", &format!("{x:.4} \u{00D7} {y:.4} \u{00B5}m"));
                        }
                        if let Some(power) = summary.objective_power {
                            kv_row(ui, "Objective", &format!("{power:.1}\u{00D7}"));
                        }
                    }

                    if !summary.instances.is_empty() {
                        section_header(ui, "Transfer syntaxes");
                        ui.add_space(4.0);
                        let syntaxes =
                            cached_transfer_syntaxes(ui, study_generation, &summary.instances);
                        let syntax_page = paginated_window(
                            ui,
                            ui.make_persistent_id(("facts-transfer-syntax-page", study_generation)),
                            syntaxes.len(),
                            TRANSFER_SYNTAX_PAGE_SIZE,
                        );
                        for uid in &syntaxes[syntax_page.start..syntax_page.end] {
                            ui.add(
                                Label::new(
                                    RichText::new(uid)
                                        .color(theme::TEXT_MUTED)
                                        .monospace()
                                        .size(11.0),
                                )
                                .wrap(),
                            );
                        }

                        section_header(
                            ui,
                            &format!("Instances \u{00B7} {}", summary.instances.len()),
                        );
                        let page = paginated_window(
                            ui,
                            ui.make_persistent_id(("facts-instance-page", study_generation)),
                            summary.instances.len(),
                            INSTANCE_PAGE_SIZE,
                        );
                        for (index, instance) in
                            summary.instances[page.start..page.end].iter().enumerate()
                        {
                            let name = instance
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("<unnamed>");
                            egui::CollapsingHeader::new(RichText::new(name).monospace().size(11.5))
                                .id_salt((study_generation, page.start + index, &instance.path))
                                .default_open(false)
                                .show(ui, |ui| {
                                    kv_row(ui, "SOP class", &instance.sop_class_uid);
                                    kv_row(ui, "Transfer", &instance.transfer_syntax_uid);
                                    if !instance.image_type.is_empty() {
                                        kv_row(ui, "Image type", &instance.image_type.join(" / "));
                                    }
                                    if let (Some(cols), Some(rows)) = (
                                        instance.total_pixel_matrix_columns,
                                        instance.total_pixel_matrix_rows,
                                    ) {
                                        kv_row(ui, "Matrix", &format!("{cols} \u{00D7} {rows}"));
                                    }
                                    if let (Some(cols), Some(rows)) =
                                        (instance.columns, instance.rows)
                                    {
                                        kv_row(ui, "Frame", &format!("{cols} \u{00D7} {rows}"));
                                    }
                                    if let Some(frames) = instance.number_of_frames {
                                        kv_row(ui, "Frames", &frames.to_string());
                                    }
                                    optional_kv_row(
                                        ui,
                                        "Organization",
                                        instance.dimension_organization_type.as_deref(),
                                    );
                                    if let Some((x, y)) = instance.pixel_spacing {
                                        kv_row(
                                            ui,
                                            "Spacing",
                                            &format!("{x:.6} \u{00D7} {y:.6} mm"),
                                        );
                                    }
                                    optional_kv_row(
                                        ui,
                                        "Photometric",
                                        instance.photometric_interpretation.as_deref(),
                                    );
                                    optional_kv_row(ui, "Samples", instance.samples_per_pixel);
                                    optional_kv_row(ui, "Bits stored", instance.bits_stored);
                                    optional_kv_row(ui, "Bits allocated", instance.bits_allocated);
                                    optional_kv_row(ui, "High bit", instance.high_bit);
                                    optional_kv_row(
                                        ui,
                                        "Pixel repr",
                                        instance.pixel_representation,
                                    );
                                    optional_kv_row(
                                        ui,
                                        "Planar config",
                                        instance.planar_configuration,
                                    );
                                });
                        }
                    }

                    if !summary.warnings.is_empty() {
                        section_header(ui, "Warnings");
                        ui.add_space(4.0);
                        let page = paginated_window(
                            ui,
                            ui.make_persistent_id(("facts-warning-page", study_generation)),
                            summary.warnings.len(),
                            WARNING_PAGE_SIZE,
                        );
                        for warning in &summary.warnings[page.start..page.end] {
                            warning_row(ui, warning);
                        }
                    }

                    ui.add_space(14.0);
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{run_ui, summary};
    use dicom_viewer_core::{
        ColorManagementMode, ColorManagementStatus, DicomInstanceSummary, TileDecodeBackend,
    };

    #[test]
    fn page_window_bounds_per_frame_facts_work_and_clamps_stale_pages() {
        assert_eq!(
            page_window(100_000, 0, TRANSFER_SYNTAX_PAGE_SIZE),
            PageWindow {
                page: 0,
                page_count: 5_000,
                start: 0,
                end: 20,
            }
        );
        assert_eq!(
            page_window(100_000, 0, 50),
            PageWindow {
                page: 0,
                page_count: 2_000,
                start: 0,
                end: 50,
            }
        );
        assert_eq!(
            page_window(101, usize::MAX, 50),
            PageWindow {
                page: 2,
                page_count: 3,
                start: 100,
                end: 101,
            }
        );
    }

    #[test]
    fn transfer_syntax_cache_is_stable_within_one_study_generation() {
        let first = DicomInstanceSummary {
            path: "one.dcm".into(),
            sop_class_uid: "1.2.3".into(),
            series_instance_uid_present: true,
            transfer_syntax_uid: "1.2.840.10008.1.2.1".into(),
            image_type: Vec::new(),
            rows: None,
            columns: None,
            total_pixel_matrix_rows: None,
            total_pixel_matrix_columns: None,
            number_of_frames: None,
            optical_path_count: None,
            focal_plane_count: None,
            concatenation_instance_count: None,
            pixel_spacing: None,
            dimension_organization_type: None,
            samples_per_pixel: None,
            photometric_interpretation: None,
            planar_configuration: None,
            bits_allocated: None,
            bits_stored: None,
            high_bit: None,
            pixel_representation: None,
        };
        let second = DicomInstanceSummary {
            transfer_syntax_uid: "1.2.840.10008.1.2.4.90".into(),
            ..first.clone()
        };

        run_ui(|ui| {
            let cached = cached_transfer_syntaxes(ui, 7, std::slice::from_ref(&first));
            let same_generation = cached_transfer_syntaxes(ui, 7, std::slice::from_ref(&second));
            assert!(Arc::ptr_eq(&cached, &same_generation));
            assert_eq!(
                same_generation.as_ref(),
                std::slice::from_ref(&first.transfer_syntax_uid)
            );

            let replacement = cached_transfer_syntaxes(ui, 8, &[first.clone(), second.clone()]);
            assert_eq!(replacement.len(), 2);
            assert!(!Arc::ptr_eq(&cached, &replacement));
        });
    }

    #[test]
    fn facts_sidebar_renders_empty_and_complete_dicom_summaries() {
        let hidden = run_ui(|ui| show_facts_sidebar(ui, false, 1, None));
        assert!(hidden.shapes.is_empty());

        let empty = run_ui(|ui| show_facts_sidebar(ui, true, 1, None));
        assert!(!empty.shapes.is_empty());

        let mut study = summary();
        study.source_kind = SourceKind::Folder;
        study.source_path = "dicom-study".into();
        study.format_label = "VL Whole Slide Microscopy Image".into();
        study.tile_decode_backend = TileDecodeBackend::Metal;
        study.file_count = 2;
        study.dicom_instance_count = 1;
        study.mpp = Some((0.25, 0.26));
        study.objective_power = Some(40.0);
        study.color_management.status = ColorManagementStatus::Applied;
        study.color_management.applied_mode = ColorManagementMode::MetalLut65;
        study.color_management.byte_size = Some(4096);
        study.color_management.provenance = Some("Optical Path Sequence".into());
        study.color_management.sha256 = Some("abc123".into());
        study.instances.push(DicomInstanceSummary {
            path: "level-0.dcm".into(),
            sop_class_uid: "1.2.840.10008.5.1.4.1.1.77.1.6".into(),
            series_instance_uid_present: true,
            transfer_syntax_uid: "1.2.840.10008.1.2.4.90".into(),
            image_type: vec!["ORIGINAL".into(), "PRIMARY".into(), "VOLUME".into()],
            rows: Some(512),
            columns: Some(512),
            total_pixel_matrix_rows: Some(4096),
            total_pixel_matrix_columns: Some(4096),
            number_of_frames: Some(64),
            optical_path_count: Some(1),
            focal_plane_count: Some(1),
            concatenation_instance_count: Some(1),
            pixel_spacing: Some((0.00025, 0.00026)),
            dimension_organization_type: Some("TILED_FULL".into()),
            samples_per_pixel: Some(3),
            photometric_interpretation: Some("RGB".into()),
            planar_configuration: Some(0),
            bits_allocated: Some(8),
            bits_stored: Some(8),
            high_bit: Some(7),
            pixel_representation: Some(0),
        });
        study.warnings = vec!["synthetic warning".into(), "second warning".into()];

        let output = run_ui(|ui| show_facts_sidebar(ui, true, 9, Some(&study)));
        assert!(output.shapes.len() > empty.shapes.len());
    }
}
