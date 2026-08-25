use std::path::Path;

use dicom_core::value::{DataSetSequence, PrimitiveValue, Value};
use dicom_core::{DataElement, Length, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

pub(crate) fn write_source_wsi(
    path: &Path,
    width: u32,
    height: u32,
    tile_width: u16,
    tile_height: u16,
) {
    const SOP_UID: &str = "1.2.826.0.1.3680043.10.777.101";
    const SERIES_UID: &str = "1.2.826.0.1.3680043.10.777.102";
    const STUDY_UID: &str = "1.2.826.0.1.3680043.10.777.103";
    const FOR_UID: &str = "1.2.826.0.1.3680043.10.777.104";
    let mut origin = InMemDicomObject::new_empty();
    origin.put(DataElement::new(
        tags::X_OFFSET_IN_SLIDE_COORDINATE_SYSTEM,
        VR::DS,
        "0",
    ));
    origin.put(DataElement::new(
        tags::Y_OFFSET_IN_SLIDE_COORDINATE_SYSTEM,
        VR::DS,
        "0",
    ));
    let frame_count = width
        .div_ceil(u32::from(tile_width))
        .saturating_mul(height.div_ceil(u32::from(tile_height)));
    let mut object = InMemDicomObject::new_empty();
    for element in [
        DataElement::new(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
        ),
        DataElement::new(tags::SOP_INSTANCE_UID, VR::UI, SOP_UID),
        DataElement::new(tags::STUDY_INSTANCE_UID, VR::UI, STUDY_UID),
        DataElement::new(tags::SERIES_INSTANCE_UID, VR::UI, SERIES_UID),
        DataElement::new(tags::FRAME_OF_REFERENCE_UID, VR::UI, FOR_UID),
        DataElement::new(tags::PATIENT_NAME, VR::PN, "Research^Slide"),
        DataElement::new(tags::PATIENT_ID, VR::LO, "R-1"),
        DataElement::new(tags::STUDY_DATE, VR::DA, "20260804"),
        DataElement::new(tags::STUDY_TIME, VR::TM, "120000"),
        DataElement::new(tags::STUDY_ID, VR::SH, "STUDY-1"),
        DataElement::new(tags::ACCESSION_NUMBER, VR::SH, ""),
        DataElement::new(tags::MANUFACTURER, VR::LO, "Frames"),
        DataElement::new(tags::MANUFACTURER_MODEL_NAME, VR::LO, "Synthetic WSI"),
        DataElement::new(tags::DEVICE_SERIAL_NUMBER, VR::LO, "TEST"),
        DataElement::new(tags::SOFTWARE_VERSIONS, VR::LO, "0.1"),
        DataElement::new(tags::DIMENSION_ORGANIZATION_TYPE, VR::CS, "TILED_FULL"),
        DataElement::new(tags::NUMBER_OF_FRAMES, VR::IS, frame_count.to_string()),
        DataElement::new(
            tags::NUMBER_OF_OPTICAL_PATHS,
            VR::UL,
            PrimitiveValue::from(1_u32),
        ),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
            VR::UL,
            PrimitiveValue::from(1_u32),
        ),
        DataElement::new(tags::ROWS, VR::US, PrimitiveValue::from(tile_height)),
        DataElement::new(tags::COLUMNS, VR::US, PrimitiveValue::from(tile_width)),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_ROWS,
            VR::UL,
            PrimitiveValue::from(height),
        ),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_COLUMNS,
            VR::UL,
            PrimitiveValue::from(width),
        ),
        DataElement::new(tags::IMAGE_ORIENTATION_SLIDE, VR::DS, "1\\0\\0\\0\\1\\0"),
        DataElement::new(tags::PIXEL_SPACING, VR::DS, "0.00025\\0.00025"),
        DataElement::new(tags::SLICE_THICKNESS, VR::DS, "0.001"),
    ] {
        object.put(element);
    }
    object.put(DataElement::new(
        tags::TOTAL_PIXEL_MATRIX_ORIGIN_SEQUENCE,
        VR::SQ,
        Value::from(DataSetSequence::new(vec![origin], Length::UNDEFINED)),
    ));
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::from(vec![
            0_u8;
            usize::from(tile_width) * usize::from(tile_height) * 3
        ]),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .media_storage_sop_class_uid(uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(SOP_UID)
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN),
        )
        .unwrap()
        .write_to_file(path)
        .unwrap();
}
