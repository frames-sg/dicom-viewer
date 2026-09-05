use super::*;

impl WorkspaceRuntime {
    pub(in crate::app) fn add_external_annotation(
        &mut self,
        name: impl Into<String>,
        source_path: Option<PathBuf>,
        document: AnnotationDocument,
    ) -> ViewerResult<Uuid> {
        let count = document.groups().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::DicomAnn,
            source_path,
            None,
            count,
        )?;
        self.external_payloads
            .insert(id, ExternalLayerPayload::Annotation(Arc::new(document)));
        Ok(id)
    }

    pub(in crate::app) fn add_external_pathology(
        &mut self,
        name: impl Into<String>,
        session: PathologySession,
    ) -> ViewerResult<Uuid> {
        let source_path = Some(session.geojson_path().to_path_buf());
        let source_digest = Some(session.semantic_digest().to_owned());
        let count = session.preview().features().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::ProfiledGeoJson,
            source_path,
            source_digest,
            count,
        )?;
        self.external_payloads
            .insert(id, ExternalLayerPayload::ProfiledGeoJson(Arc::new(session)));
        Ok(id)
    }

    pub(in crate::app) fn add_external_segmentation(
        &mut self,
        name: impl Into<String>,
        source_path: Option<PathBuf>,
        document: SegmentationDocument,
    ) -> ViewerResult<(Uuid, Vec<InteroperabilityDiagnostic>)> {
        let count = document.segments().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::DicomSeg,
            source_path,
            None,
            count,
        )?;
        let (vector_groups, diagnostics) =
            if document.kind() == dicom_viewer_core::SegmentationKind::Fractional {
                (None, Vec::new())
            } else {
                let projection =
                    document.vectorized_annotations(SegToAnnConversionPolicy::AllowLoss)?;
                let diagnostics = projection.diagnostics().to_vec();
                (Some(Arc::from(projection.into_groups())), diagnostics)
            };
        self.external_payloads.insert(
            id,
            ExternalLayerPayload::Segmentation {
                document: Arc::new(document),
                vector_groups,
            },
        );
        Ok((id, diagnostics))
    }

    pub(in crate::app) fn add_external_report(
        &mut self,
        name: impl Into<String>,
        source_path: Option<PathBuf>,
        session: ReportSession,
    ) -> ViewerResult<Uuid> {
        let count = session.document().groups().len() as u64;
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::DicomSr,
            source_path,
            None,
            count,
        )?;
        self.external_payloads
            .insert(id, ExternalLayerPayload::Report(Arc::new(session)));
        Ok(id)
    }

    pub(in crate::app) fn add_external_heatmap(
        &mut self,
        name: impl Into<String>,
        session: RasterSession,
        context: &eframe::egui::Context,
    ) -> ViewerResult<Uuid> {
        let source_path = Some(session.raster_path().to_path_buf());
        let source_digest = Some(session.semantic_digest().to_owned());
        let count = u64::from(session.frame_count())
            .saturating_mul(u64::try_from(session.selected_channel_count()).unwrap_or(u64::MAX));
        let id = self.upsert_external_layer(
            name.into(),
            ExternalLayerKind::Heatmap,
            source_path,
            source_digest,
            count,
        )?;
        let texture = session.load_texture(context);
        self.external_payloads.insert(
            id,
            ExternalLayerPayload::Heatmap {
                session: Arc::new(session),
                texture,
            },
        );
        Ok(id)
    }

    fn upsert_external_layer(
        &mut self,
        name: String,
        kind: ExternalLayerKind,
        source_path: Option<PathBuf>,
        source_digest: Option<String>,
        source_object_count: u64,
    ) -> ViewerResult<Uuid> {
        if let Some(id) = self.matching_external_layer(&kind, source_path.as_deref()) {
            self.edit("Load source layer", |workspace| {
                workspace.hydrate_external_layer(id, source_object_count, source_digest)
            })?;
            return Ok(id);
        }
        let reference = ExternalLayerReference::new(
            name,
            kind,
            source_path,
            source_digest,
            source_object_count,
        );
        let id = reference.id();
        self.edit("Import source layer", |document| {
            document.add_external_layer(reference)
        })?;
        Ok(id)
    }

    pub(in crate::app) fn ensure_discovered_external_stub(
        &mut self,
        name: impl Into<String>,
        kind: ExternalLayerKind,
        source_path: PathBuf,
    ) -> ViewerResult<Uuid> {
        if let Some(id) = self.matching_external_layer(&kind, Some(&source_path)) {
            return Ok(id);
        }
        let reference = ExternalLayerReference::new(name, kind, Some(source_path), None, 0);
        let id = reference.id();
        let mut candidate = (*self.document).clone();
        candidate.add_external_layer(reference)?;
        candidate.validate()?;
        self.document = Arc::new(candidate);
        self.invalidate_spatial_index();
        Ok(id)
    }

    pub(in crate::app) fn remove_external_layer(&mut self, layer_id: Uuid) -> ViewerResult<bool> {
        self.edit("Remove source layer", |document| {
            document.remove_external_layer(layer_id)
        })
    }

    #[must_use]
    pub(in crate::app) fn external_payload(&self, layer_id: Uuid) -> Option<&ExternalLayerPayload> {
        self.external_payloads.get(&layer_id)
    }

    fn matching_external_layer(
        &self,
        kind: &ExternalLayerKind,
        source_path: Option<&Path>,
    ) -> Option<Uuid> {
        let source_path = source_path?;
        self.document
            .external_layers()
            .iter()
            .find(|layer| layer.kind() == kind && layer.source_path() == Some(source_path))
            .map(ExternalLayerReference::id)
    }

    pub(in crate::app) fn external_classes(
        &self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<ExternalClassDescriptor>> {
        let objects = self.prepare_external_objects(layer_id)?;
        let mut classes = BTreeMap::<String, ExternalClassDescriptor>::new();
        for object in objects {
            let Some(geometry) = object.geometry else {
                continue;
            };
            let editable = object.promotion.is_some();
            let entry = classes.entry(object.class_key.clone()).or_insert_with(|| {
                let exact_scheme_class_id = self
                    .document
                    .scheme()
                    .classes()
                    .iter()
                    .find(|class| class.concept_key().as_str() == object.class_key)
                    .map(|class| class.id().to_owned());
                ExternalClassDescriptor {
                    key: object.class_key,
                    label: object.label,
                    geometry,
                    object_count: 0,
                    exact_scheme_class_id,
                    editable,
                }
            });
            entry.object_count = entry.object_count.saturating_add(1);
            entry.editable &= editable;
        }
        Ok(classes.into_values().collect())
    }

    pub(in crate::app) fn external_objects(
        &self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<ExternalObjectDescriptor>> {
        self.prepare_external_objects(layer_id).map(|objects| {
            objects
                .into_iter()
                .map(|object| ExternalObjectDescriptor {
                    promoted: self.source_object_was_promoted(layer_id, &object.source_object_id),
                    promotable: object.promotion.is_some(),
                    promotion_block_reason: object.promotion_block_reason,
                    source_object_id: object.source_object_id,
                    label: object.label,
                    class_key: object.class_key,
                })
                .collect()
        })
    }

    pub(in crate::app) fn set_external_class_mapping(
        &mut self,
        layer_id: Uuid,
        source_class_key: &str,
        target_class_id: &str,
    ) -> ViewerResult<()> {
        let source = self
            .external_classes(layer_id)?
            .into_iter()
            .find(|class| class.key == source_class_key)
            .ok_or_else(|| {
                ViewerError::InvalidInput("the external source class does not exist".into())
            })?;
        let target = self
            .document
            .scheme()
            .class(target_class_id)
            .ok_or_else(|| {
                ViewerError::InvalidInput(
                    "the mapping target is not in the pinned annotation scheme".into(),
                )
            })?;
        if source.geometry != target.geometry() {
            return Err(ViewerError::InvalidInput(
                "external class mappings cannot change point/region geometry".into(),
            ));
        }
        let source_class_key = source_class_key.to_owned();
        let target_class_id = target_class_id.to_owned();
        self.edit("Map external class", move |document| {
            document.set_external_class_mapping(layer_id, &source_class_key, &target_class_id)
        })
    }

    pub(in crate::app) fn promote_external_object(
        &mut self,
        layer_id: Uuid,
        source_object_id: &str,
    ) -> ViewerResult<Uuid> {
        if self.source_object_was_promoted(layer_id, source_object_id) {
            return Err(ViewerError::InvalidInput(
                "the selected source object is already a tracked finding".into(),
            ));
        }
        let object = self
            .prepare_external_objects(layer_id)?
            .into_iter()
            .find(|object| object.source_object_id == source_object_id)
            .ok_or_else(|| {
                ViewerError::InvalidInput("the external source object does not exist".into())
            })?;
        let class_id = self.external_mapping_target(layer_id, &object.class_key)?;
        let promotion = object.promotion.ok_or_else(|| {
            ViewerError::Unsupported(object.promotion_block_reason.unwrap_or_else(|| {
                "the selected external object cannot be represented by an editable workspace geometry"
                    .into()
            }))
        })?;
        let vector_layer = self.active_vector_layer;
        let source_object_id = object.source_object_id;
        let id = self.edit("Promote source object", move |document| {
            apply_external_promotion(
                document,
                vector_layer,
                layer_id,
                source_object_id,
                &class_id,
                promotion,
            )
        })?;
        if self.document.segment(id).is_some() {
            self.active_segmentation_layer = self
                .document
                .segmentation_layers()
                .iter()
                .find(|layer| {
                    layer
                        .segments()
                        .iter()
                        .any(|segment| segment.object_id() == id)
                })
                .map(|layer| layer.id());
        }
        self.select_only(id);
        Ok(id)
    }

    pub(in crate::app) fn make_external_layer_editable(
        &mut self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<Uuid>> {
        let objects = self.prepare_external_objects(layer_id)?;
        if objects.is_empty() {
            return Err(ViewerError::InvalidInput(
                "the external layer contains no convertible objects".into(),
            ));
        }
        if objects
            .iter()
            .any(|object| self.source_object_was_promoted(layer_id, &object.source_object_id))
        {
            return Err(ViewerError::InvalidInput(
                "the external layer already contains promoted objects; promote the remaining objects individually".into(),
            ));
        }
        let prepared = objects
            .into_iter()
            .map(|object| {
                let class_id = self.external_mapping_target(layer_id, &object.class_key)?;
                let promotion = object.promotion.ok_or_else(|| {
                    ViewerError::Unsupported(object.promotion_block_reason.unwrap_or_else(|| {
                        format!(
                            "source object {} cannot be converted losslessly",
                            object.source_object_id
                        )
                    }))
                })?;
                Ok((object.source_object_id, class_id, promotion))
            })
            .collect::<ViewerResult<Vec<_>>>()?;
        let vector_layer = self.active_vector_layer;
        let ids = self.edit("Make external layer editable", move |document| {
            prepared
                .into_iter()
                .map(|(source_object_id, class_id, promotion)| {
                    apply_external_promotion(
                        document,
                        vector_layer,
                        layer_id,
                        source_object_id,
                        &class_id,
                        promotion,
                    )
                })
                .collect::<ViewerResult<Vec<_>>>()
        })?;
        self.selection = ids.iter().copied().collect();
        if ids.iter().any(|id| self.document.segment(*id).is_some()) {
            self.active_segmentation_layer = self
                .document
                .segmentation_layers()
                .iter()
                .find(|layer| {
                    layer
                        .segments()
                        .iter()
                        .any(|segment| ids.contains(&segment.object_id()))
                })
                .map(|layer| layer.id());
        }
        Ok(ids)
    }

    fn external_mapping_target(
        &self,
        layer_id: Uuid,
        source_class_key: &str,
    ) -> ViewerResult<String> {
        self.document
            .external_layers()
            .iter()
            .find(|layer| layer.id() == layer_id)
            .and_then(|layer| layer.class_mappings().get(source_class_key))
            .cloned()
            .ok_or_else(|| {
                ViewerError::InvalidInput(
                    "every source class needs an explicit mapping before promotion".into(),
                )
            })
    }

    fn prepare_external_objects(
        &self,
        layer_id: Uuid,
    ) -> ViewerResult<Vec<PreparedExternalObject>> {
        if !self
            .document
            .external_layers()
            .iter()
            .any(|layer| layer.id() == layer_id)
        {
            return Err(ViewerError::InvalidInput(
                "the external source layer does not exist".into(),
            ));
        }
        match self.external_payloads.get(&layer_id) {
            Some(ExternalLayerPayload::Annotation(document)) => {
                prepare_annotation_objects(document)
            }
            Some(ExternalLayerPayload::ProfiledGeoJson(session)) => session
                .editable_ann()
                .map_or_else(|| Ok(Vec::new()), prepare_annotation_objects),
            Some(ExternalLayerPayload::Segmentation { document, .. }) => {
                prepare_segmentation_objects(document)
            }
            Some(ExternalLayerPayload::Report(session)) => {
                prepare_report_objects(session.document())
            }
            Some(ExternalLayerPayload::Heatmap { .. }) => Ok(Vec::new()),
            None => Ok(Vec::new()),
        }
    }

    fn source_object_was_promoted(&self, layer_id: Uuid, source_object_id: &str) -> bool {
        self.document
            .vector_findings()
            .map(|finding| finding.provenance())
            .chain(self.document.segments().map(|segment| segment.provenance()))
            .chain(
                self.document
                    .measurements()
                    .iter()
                    .map(|measurement| measurement.provenance()),
            )
            .any(|provenance| {
                matches!(
                    provenance,
                    WorkspaceObjectProvenance::Promoted {
                        source_layer_id,
                        source_object_id: promoted_id,
                    } if *source_layer_id == layer_id && promoted_id == source_object_id
                )
            })
    }
}

fn annotation_group_geometry(
    group: &dicom_viewer_core::AnnotationGroup,
) -> Option<AnnotationClassGeometry> {
    match group.geometry() {
        AnnotationGeometry::Points(_) => Some(AnnotationClassGeometry::Point),
        AnnotationGeometry::Polygons(_) => Some(AnnotationClassGeometry::Region),
        AnnotationGeometry::ReadOnly { graphic_type, .. } => match graphic_type {
            dicom_viewer_core::AnnotationGraphicType::Point => Some(AnnotationClassGeometry::Point),
            dicom_viewer_core::AnnotationGraphicType::Polygon => {
                Some(AnnotationClassGeometry::Region)
            }
            dicom_viewer_core::AnnotationGraphicType::Polyline
            | dicom_viewer_core::AnnotationGraphicType::Ellipse
            | dicom_viewer_core::AnnotationGraphicType::Rectangle => None,
        },
    }
}

fn prepare_annotation_objects(
    document: &AnnotationDocument,
) -> ViewerResult<Vec<PreparedExternalObject>> {
    let mut objects = Vec::new();
    for group in document.groups() {
        let Some(geometry) = annotation_group_geometry(group) else {
            continue;
        };
        let class_key = annotation_class_concept_key(
            geometry,
            group.category(),
            group.property_type(),
            group.property_type_modifiers(),
        )
        .to_string();
        let (source_frame, promotion_block_reason) = match SourceFrameContext::from_ann_group(group)
        {
            Ok(source_frame) => (Some(source_frame), None),
            Err(ViewerError::Unsupported(reason) | ViewerError::InvalidInput(reason)) => {
                (None, Some(reason))
            }
            Err(error) => (None, Some(error.to_string())),
        };
        match group.geometry() {
            AnnotationGeometry::Points(points) => {
                for (index, point) in points.iter().copied().enumerate() {
                    let point = document.canonical_level0_pixel(
                        document.source(),
                        point.x,
                        point.y,
                        None,
                    )?;
                    objects.push(PreparedExternalObject {
                        source_object_id: format!("{}:{}", group.uid(), index + 1),
                        label: group.label().to_owned(),
                        class_key: class_key.clone(),
                        geometry: Some(geometry),
                        promotion: source_frame.clone().map(|source_frame| {
                            PreparedExternalPromotion::Vector {
                                geometry: VectorFindingGeometry::Point(point),
                                tracking: None,
                                source_frame,
                            }
                        }),
                        promotion_block_reason: promotion_block_reason.clone(),
                    });
                }
            }
            AnnotationGeometry::Polygons(polygons) => {
                for (index, polygon) in polygons.iter().enumerate() {
                    let polygon = polygon
                        .iter()
                        .map(|point| {
                            document
                                .canonical_level0_pixel(document.source(), point.x, point.y, None)
                                .map_err(dicom_viewer_core::ViewerError::from)
                        })
                        .collect::<ViewerResult<Vec<_>>>()?;
                    objects.push(PreparedExternalObject {
                        source_object_id: format!("{}:{}", group.uid(), index + 1),
                        label: group.label().to_owned(),
                        class_key: class_key.clone(),
                        geometry: Some(geometry),
                        promotion: source_frame.clone().map(|source_frame| {
                            PreparedExternalPromotion::Vector {
                                geometry: VectorFindingGeometry::regions(vec![polygon]),
                                tracking: None,
                                source_frame,
                            }
                        }),
                        promotion_block_reason: promotion_block_reason.clone(),
                    });
                }
            }
            AnnotationGeometry::ReadOnly { .. } => {
                for index in 0..group.annotation_count() {
                    objects.push(PreparedExternalObject {
                        source_object_id: format!("{}:{}", group.uid(), index + 1),
                        label: group.label().to_owned(),
                        class_key: class_key.clone(),
                        geometry: Some(geometry),
                        promotion: None,
                        promotion_block_reason: Some(
                            "the ANN graphic type is viewable but has no editable workspace geometry"
                                .into(),
                        ),
                    });
                }
            }
        }
    }
    Ok(objects)
}

fn prepare_segmentation_objects(
    document: &SegmentationDocument,
) -> ViewerResult<Vec<PreparedExternalObject>> {
    let mut polygons = BTreeMap::<u16, Vec<Vec<Point2>>>::new();
    if document.editable() {
        for run in document.binary_runs()? {
            let x0 = f64::from(run.column_start());
            let x1 = f64::from(run.column_start().saturating_add(run.length()));
            let y0 = f64::from(run.row());
            let y1 = y0 + 1.0;
            polygons.entry(run.segment_number()).or_default().push(vec![
                Point2::new(x0, y0),
                Point2::new(x1, y0),
                Point2::new(x1, y1),
                Point2::new(x0, y1),
            ]);
        }
    }
    Ok(document
        .segments()
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let number = segment
                .source_segment_number()
                .unwrap_or_else(|| u16::try_from(index + 1).unwrap_or(u16::MAX));
            let class_key = annotation_class_concept_key(
                AnnotationClassGeometry::Region,
                segment.category(),
                segment.property_type(),
                segment.property_type_modifiers(),
            )
            .to_string();
            let tracking = segment
                .tracking_id()
                .zip(segment.tracking_uid())
                .and_then(|(id, uid)| TrackingIdentity::new(id, uid).ok());
            let primitives = polygons.remove(&number).map(|polygons| {
                polygons
                    .into_iter()
                    .map(|polygon| SegmentationPrimitive::polygon(SegmentOperation::Add, polygon))
                    .collect::<Vec<_>>()
            });
            PreparedExternalObject {
                source_object_id: format!("segment:{number}"),
                label: segment.label().to_owned(),
                class_key,
                geometry: Some(AnnotationClassGeometry::Region),
                promotion: primitives.filter(|primitives| !primitives.is_empty()).map(
                    |primitives| PreparedExternalPromotion::Segment {
                        primitives,
                        tracking,
                        source_frame: SourceFrameContext::default(),
                    },
                ),
                promotion_block_reason: (!document.editable())
                    .then(|| "the SEG object is viewable but is not losslessly editable".into()),
            }
        })
        .collect())
}

fn prepare_report_objects(
    document: &StructuredReportDocument,
) -> ViewerResult<Vec<PreparedExternalObject>> {
    let mut objects = Vec::new();
    for group in document.groups() {
        let class_key = annotation_class_concept_key(
            AnnotationClassGeometry::Region,
            group.finding_category(),
            group.finding_type(),
            &[],
        )
        .to_string();
        let source_tracking = TrackingIdentity::new(group.tracking_id(), group.tracking_uid()).ok();
        for (measurement_index, measurement) in group.measurements().iter().enumerate() {
            for (coordinate_index, coordinates) in measurement.coordinates().iter().enumerate() {
                if coordinates.graphic() != dicom_viewer_core::CoordinateGraphic::Polyline
                    || coordinates.points().len() != 2
                {
                    continue;
                }
                let source_object_id = format!(
                    "{}:{}:{}",
                    group.tracking_uid(),
                    measurement_index + 1,
                    coordinate_index + 1
                );
                let physical_length_mm = measurement_length_mm(measurement);
                let endpoints = coordinates
                    .points()
                    .iter()
                    .map(|point| {
                        document
                            .source()
                            .slide_coordinate_to_pixel3(point.x, point.y, point.z)
                            .map_err(dicom_viewer_core::ViewerError::from)
                    })
                    .collect::<ViewerResult<Vec<_>>>()?;
                let promotion = physical_length_mm.map(|physical_length_mm| {
                    PreparedExternalPromotion::Measurement {
                        endpoints: [endpoints[0], endpoints[1]],
                        physical_length_mm,
                        tracking: source_tracking.clone(),
                        source_frame: SourceFrameContext::default(),
                    }
                });
                objects.push(PreparedExternalObject {
                    source_object_id,
                    label: group.finding_type().meaning().to_owned(),
                    class_key: class_key.clone(),
                    geometry: Some(AnnotationClassGeometry::Region),
                    promotion,
                    promotion_block_reason: physical_length_mm
                        .is_none()
                        .then(|| "the SR measurement has no supported physical length".into()),
                });
            }
        }
    }
    Ok(objects)
}

fn measurement_length_mm(
    measurement: &dicom_viewer_core::StructuredReportMeasurement,
) -> Option<f64> {
    if measurement.concept().scheme() != "SCT"
        || measurement.concept().value() != "410668003"
        || measurement.unit().scheme() != "UCUM"
        || !measurement.value().is_finite()
        || measurement.value() <= 0.0
    {
        return None;
    }
    match measurement.unit().value() {
        "mm" => Some(measurement.value()),
        "um" | "µm" => Some(measurement.value() / 1_000.0),
        _ => None,
    }
}
