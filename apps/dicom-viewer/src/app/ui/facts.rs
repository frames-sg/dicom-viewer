use dicom_viewer_core::{SourceKind, StudySummary};
use eframe::egui::{
    self, vec2, Color32, CornerRadius, Frame, Label, Margin, Panel, RichText, Sense, Stroke,
};

use super::super::{format::format_integer, theme};
use super::chrome::chrome_frame;

pub(in crate::app) fn show_facts_sidebar(
    ui: &mut egui::Ui,
    visible: bool,
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
                facts_panel(ui, summary);
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

pub(in crate::app) fn facts_panel(ui: &mut egui::Ui, summary: &StudySummary) {
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
                        let mut syntaxes = summary
                            .instances
                            .iter()
                            .map(|instance| instance.transfer_syntax_uid.as_str())
                            .collect::<Vec<_>>();
                        syntaxes.sort_unstable();
                        syntaxes.dedup();
                        for uid in syntaxes {
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
                        for instance in &summary.instances {
                            let name = instance
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("<unnamed>");
                            egui::CollapsingHeader::new(RichText::new(name).monospace().size(11.5))
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
                        for warning in &summary.warnings {
                            warning_row(ui, warning);
                        }
                    }

                    ui.add_space(14.0);
                });
        });
}
