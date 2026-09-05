mod ann;
mod bulk_ann;
mod seg;
pub(super) mod shared;

pub use bulk_ann::{BulkAnnExport, BulkAnnotationLocation};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VectorSegmentationPolicy {
    #[default]
    Exclude,
    Rasterize,
}
