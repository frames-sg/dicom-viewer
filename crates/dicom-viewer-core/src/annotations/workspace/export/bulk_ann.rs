use std::num::NonZeroU32;

use uuid::Uuid;

use crate::{
    frames_viewer_producer, AlgorithmIdentification, AnnotationDocument, AnnotationGroup,
    DicomAnnotationContext, GenerationType, Point2, Result, ViewerError,
};

use super::super::document::WorkspaceDocument;
use super::super::model::{ControlledFindingSite, VectorFinding, VectorFindingGeometry};
use super::shared::{dicom_label, finding_site_code, validate_ann_source_context};

/// One automatic source object's correspondence to primitives in a bulk ANN group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkAnnotationLocation {
    object_id: Uuid,
    tracking_uid: String,
    group_uid: String,
    first_annotation_index: NonZeroU32,
    annotation_count: NonZeroU32,
}

impl BulkAnnotationLocation {
    #[must_use]
    pub const fn object_id(&self) -> Uuid {
        self.object_id
    }

    #[must_use]
    pub fn tracking_uid(&self) -> &str {
        &self.tracking_uid
    }

    #[must_use]
    pub fn group_uid(&self) -> &str {
        &self.group_uid
    }

    /// Returns the one-based first primitive index in the identified ANN group.
    #[must_use]
    pub const fn first_annotation_index(&self) -> NonZeroU32 {
        self.first_annotation_index
    }

    #[must_use]
    pub const fn annotation_count(&self) -> NonZeroU32 {
        self.annotation_count
    }

    #[must_use]
    pub fn into_parts(self) -> (Uuid, String, String, NonZeroU32, NonZeroU32) {
        (
            self.object_id,
            self.tracking_uid,
            self.group_uid,
            self.first_annotation_index,
            self.annotation_count,
        )
    }
}

/// A bulk ANN document and in-memory source-object correspondence evidence.
///
/// The location evidence is not encoded into the DICOM ANN instance. Callers that persist the
/// document must retain this value separately for the lifetime of the export operation.
#[derive(Debug, Clone, PartialEq)]
pub struct BulkAnnExport {
    document: AnnotationDocument,
    annotation_locations: Vec<BulkAnnotationLocation>,
}

impl BulkAnnExport {
    #[must_use]
    pub fn document(&self) -> &AnnotationDocument {
        &self.document
    }

    #[must_use]
    pub fn annotation_locations(&self) -> &[BulkAnnotationLocation] {
        &self.annotation_locations
    }

    #[must_use]
    pub fn into_parts(self) -> (AnnotationDocument, Vec<BulkAnnotationLocation>) {
        (self.document, self.annotation_locations)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkGraphicType {
    Point,
    Polygon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BulkGroupKey {
    class_id: String,
    graphic_type: BulkGraphicType,
    finding_site: Option<ControlledFindingSite>,
    optical_path: Option<String>,
}

impl BulkGroupKey {
    fn from_finding(finding: &VectorFinding) -> Self {
        Self {
            class_id: finding.class_id().to_owned(),
            graphic_type: match finding.geometry() {
                VectorFindingGeometry::Point(_) => BulkGraphicType::Point,
                VectorFindingGeometry::Regions(_) => BulkGraphicType::Polygon,
            },
            finding_site: finding.finding_site().cloned(),
            optical_path: finding.source_frame().optical_path().map(str::to_owned),
        }
    }
}

struct BulkGroupBucket {
    key: BulkGroupKey,
    points: Vec<Point2>,
    polygons: Vec<Vec<Point2>>,
}

impl BulkGroupBucket {
    fn new(key: BulkGroupKey) -> Self {
        Self {
            key,
            points: Vec::new(),
            polygons: Vec::new(),
        }
    }

    fn annotation_count(&self) -> usize {
        match self.key.graphic_type {
            BulkGraphicType::Point => self.points.len(),
            BulkGraphicType::Polygon => self.polygons.len(),
        }
    }
}

struct PendingBulkAnnotationLocation {
    object_id: Uuid,
    tracking_uid: String,
    bucket_index: usize,
    first_annotation_index: NonZeroU32,
    annotation_count: NonZeroU32,
}

impl WorkspaceDocument {
    /// Exports automatic vector findings in shared ANN groups.
    ///
    /// Unlike [`Self::export_ann`], this path groups compatible findings and returns a one-based
    /// in-memory source-object-to-primitive mapping. Per-object names and comments are rejected
    /// because ANN can represent those values only at group scope.
    pub fn export_automatic_bulk_ann(
        &self,
        context: &DicomAnnotationContext,
        algorithm: AlgorithmIdentification,
    ) -> Result<BulkAnnExport> {
        let mut findings = self.vector_findings().collect::<Vec<_>>();
        findings.sort_by_key(|finding| finding.ordinal());

        let mut buckets = Vec::<BulkGroupBucket>::new();
        let mut pending_locations = Vec::with_capacity(findings.len());
        for finding in findings {
            validate_ann_source_context(
                &format!("vector finding #{}", finding.ordinal()),
                finding.source_frame(),
                context,
            )?;
            if finding.name().is_some() || finding.comment().is_some() {
                return Err(ViewerError::InvalidInput(
                    "bulk ANN cannot preserve per-annotation name or comment".into(),
                ));
            }
            if self.scheme().class(finding.class_id()).is_none() {
                return Err(ViewerError::InvalidInput(
                    "finding references an unknown annotation class".into(),
                ));
            }

            let key = BulkGroupKey::from_finding(finding);
            let bucket_index = buckets
                .iter()
                .position(|bucket| bucket.key == key)
                .unwrap_or_else(|| {
                    buckets.push(BulkGroupBucket::new(key));
                    buckets.len() - 1
                });
            let bucket = &mut buckets[bucket_index];
            let first_annotation_index = bucket
                .annotation_count()
                .checked_add(1)
                .and_then(|index| u32::try_from(index).ok())
                .and_then(NonZeroU32::new)
                .ok_or_else(|| {
                    ViewerError::InvalidInput(
                        "bulk ANN annotation index exceeds DICOM UL range".into(),
                    )
                })?;
            let annotation_count = match finding.geometry() {
                VectorFindingGeometry::Point(point) => {
                    bucket.points.push(*point);
                    NonZeroU32::MIN
                }
                VectorFindingGeometry::Regions(components) => {
                    let count = u32::try_from(components.len())
                        .ok()
                        .and_then(NonZeroU32::new)
                        .ok_or_else(|| {
                            ViewerError::InvalidInput(
                                "bulk ANN annotation count must fit a nonzero DICOM UL".into(),
                            )
                        })?;
                    bucket
                        .polygons
                        .extend(components.iter().map(|component| component.to_vec()));
                    count
                }
            };
            pending_locations.push(PendingBulkAnnotationLocation {
                object_id: finding.object_id(),
                tracking_uid: finding.tracking().uid().to_owned(),
                bucket_index,
                first_annotation_index,
                annotation_count,
            });
        }

        if buckets.is_empty() {
            return Err(ViewerError::InvalidInput(
                "bulk ANN export has no directly representable vector findings".into(),
            ));
        }

        let mut groups = Vec::with_capacity(buckets.len());
        let mut group_uids = Vec::with_capacity(buckets.len());
        for bucket in buckets {
            let class = self.scheme().class(&bucket.key.class_id).ok_or_else(|| {
                ViewerError::InvalidInput("finding references an unknown annotation class".into())
            })?;
            let site_code = bucket
                .key
                .finding_site
                .as_ref()
                .map(|site| finding_site_code(self, site))
                .transpose()?;
            let mut group = match bucket.key.graphic_type {
                BulkGraphicType::Point => AnnotationGroup::points(
                    dicom_label(class.label())?,
                    class.category().clone(),
                    class.property_type().clone(),
                    class.recommended_display_cielab(),
                    bucket.points,
                )?,
                BulkGraphicType::Polygon => AnnotationGroup::polygons(
                    dicom_label(class.label())?,
                    class.category().clone(),
                    class.property_type().clone(),
                    class.recommended_display_cielab(),
                    bucket.polygons,
                )?,
            }
            .with_property_type_modifiers(class.property_type_modifiers().to_vec())
            .with_generation(GenerationType::Automatic, vec![algorithm.clone()])?;
            if let Some(site) = site_code {
                group = group.with_anatomic_regions(vec![site]);
            }
            if let Some(optical_path) = bucket.key.optical_path {
                group = group.with_referenced_optical_paths(vec![optical_path])?;
            }
            group = group.with_deterministic_uid(
                "frames-dicom-viewer:automatic-bulk-ann:v1",
                context.sop_instance_uid(),
                &bucket.key.class_id,
            )?;
            group_uids.push(group.uid().to_owned());
            groups.push(group);
        }

        let annotation_locations = pending_locations
            .into_iter()
            .map(|location| BulkAnnotationLocation {
                object_id: location.object_id,
                tracking_uid: location.tracking_uid,
                group_uid: group_uids[location.bucket_index].clone(),
                first_annotation_index: location.first_annotation_index,
                annotation_count: location.annotation_count,
            })
            .collect();
        let document = AnnotationDocument::new(context.clone(), groups)?
            .with_producer(frames_viewer_producer(9101, "WSI annotations")?);
        Ok(BulkAnnExport {
            document,
            annotation_locations,
        })
    }
}
