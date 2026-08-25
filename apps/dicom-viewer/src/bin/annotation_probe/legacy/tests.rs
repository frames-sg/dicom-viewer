use super::*;
use dicom_core::value::{DataSetSequence, PrimitiveValue, Value};
use dicom_core::{DataElement, Length, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
use wsi_dicom_annotations::{
    AlgorithmIdentification, AnnotationGroup, AnnotationMeasurement, DicomCode, GenerationType,
    Point2, SegmentationDocument, SegmentationSegment,
};

#[test]
fn parses_inspect_defaults() {
    let arguments = parse_arguments(
        ["inspect", "--source", "source.dcm", "annotations.dcm"]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>(),
    )
    .unwrap();

    assert_eq!(arguments.operation, Operation::Inspect);
    assert_eq!(arguments.payload, PayloadMode::Full);
    assert_eq!(arguments.canonical_source, PathBuf::from("source.dcm"));
    assert!(arguments.output.is_none());
}

#[test]
fn roundtrip_requires_a_distinct_output() {
    assert!(parse_arguments(
        ["roundtrip", "--source", "source.dcm", "annotations.dcm"]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
    )
    .is_err());
    assert!(parse_arguments(
        [
            "roundtrip",
            "--source",
            "source.dcm",
            "--output",
            "annotations.dcm",
            "annotations.dcm",
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>()
    )
    .is_err());
}

#[test]
fn parser_rejects_every_ambiguous_or_incomplete_legacy_shape() {
    let parse = |values: &[&str]| {
        parse_arguments(
            values
                .iter()
                .copied()
                .map(OsString::from)
                .collect::<Vec<_>>(),
        )
    };
    for arguments in [
        vec![],
        vec!["convert"],
        vec!["inspect", "--source"],
        vec!["inspect", "--canonical-source"],
        vec!["roundtrip", "--output"],
        vec!["inspect", "--payload"],
        vec!["inspect", "--payload", "brief"],
        vec!["inspect", "--unknown"],
        vec!["inspect", "--source", "source", "one", "two"],
        vec!["inspect", "annotations"],
        vec!["inspect", "--source", "source"],
        vec![
            "inspect",
            "--source",
            "source",
            "--output",
            "out",
            "annotations",
        ],
        vec![
            "inspect",
            "--source",
            "source",
            "--allow-lossy",
            "annotations",
        ],
    ] {
        assert!(parse(&arguments).is_err(), "accepted {arguments:?}");
    }

    let parsed = parse(&[
        "roundtrip",
        "--source",
        "source",
        "--canonical-source",
        "level-zero",
        "--payload",
        "digest",
        "--allow-lossy",
        "--output",
        "out",
        "annotations",
    ])
    .unwrap();
    assert_eq!(parsed.canonical_source, PathBuf::from("level-zero"));
    assert_eq!(parsed.payload, PayloadMode::Digest);
    assert!(parsed.allow_lossy);
}

#[test]
fn operation_failures_emit_machine_readable_json() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit = crate::command::execute(
        [
            "inspect",
            "--source",
            "missing-source.dcm",
            "missing-ann.dcm",
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>(),
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(exit, 1);
    let report: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(report["status"], "error");
    assert_eq!(report["operation"], "inspect");
    assert!(!stderr.is_empty());
}

#[test]
fn inspect_and_roundtrip_emit_stable_ann_and_seg_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    let ann = directory.path().join("annotations.dcm");
    let ann_output = directory.path().join("annotations-roundtrip.dcm");
    let seg = directory.path().join("segmentation.dcm");
    let seg_output = directory.path().join("segmentation-roundtrip.dcm");
    write_source_wsi(&source);
    let context = DicomAnnotationContext::from_source(&source).unwrap();
    let category = code("MORPH", "Morphology");
    let property = code("TUMOR", "Tumor");
    let algorithm = AlgorithmIdentification::new(
        code("AI", "Artificial intelligence"),
        "test-classifier",
        "1.0",
    )
    .unwrap()
    .with_name_code(code("MODEL", "Model"))
    .with_parameters("threshold=0.5")
    .unwrap()
    .with_source("test fixture")
    .unwrap();
    let mut cells = AnnotationGroup::points(
        "Cells",
        category.clone(),
        property.clone(),
        [1, 2, 3],
        vec![Point2::new(1.0, 2.0), Point2::new(3.0, 4.0)],
    )
    .unwrap()
    .with_description("Automated cell detections")
    .unwrap()
    .with_generation(GenerationType::Automatic, vec![algorithm])
    .unwrap()
    .with_property_type_modifiers(vec![code("INV", "Invasive")])
    .with_anatomic_regions(vec![code("BREAST", "Breast")])
    .with_primary_anatomic_structures(vec![code("LOBULE", "Lobule")]);
    cells
        .add_measurement(AnnotationMeasurement::new(
            code("AREA", "Area"),
            DicomCode::new("mm2", "UCUM", "square millimeter").unwrap(),
            vec![2.5, 4.5],
        ))
        .unwrap();
    AnnotationDocument::new(context.clone(), vec![cells])
        .unwrap()
        .write_ann(&ann)
        .unwrap();
    SegmentationDocument::binary(
        context,
        vec![SegmentationSegment::new(
            "Tumor",
            category,
            property,
            [1, 2, 3],
            vec![vec![
                Point2::new(0.0, 0.0),
                Point2::new(4.0, 0.0),
                Point2::new(4.0, 4.0),
                Point2::new(0.0, 4.0),
            ]],
            Vec::new(),
        )
        .unwrap()],
    )
    .unwrap()
    .write_seg(&seg)
    .unwrap();

    let ann_inspect = invoke([
        "inspect",
        "--source",
        source.to_str().unwrap(),
        ann.to_str().unwrap(),
    ]);
    let ann_roundtrip = invoke([
        "roundtrip",
        "--source",
        source.to_str().unwrap(),
        "--payload",
        "digest",
        "--output",
        ann_output.to_str().unwrap(),
        ann.to_str().unwrap(),
    ]);
    assert_eq!(ann_inspect["status"], "ok");
    assert_eq!(ann_roundtrip["status"], "ok");
    assert_eq!(ann_inspect["semantic"]["object_type"], "Ann");
    assert_eq!(
        ann_inspect["semantic"]["data"]["groups"][0]["measurements"][0]["values"],
        serde_json::json!([2.5, 4.5])
    );
    assert_eq!(
        ann_inspect["semantic"]["data"]["groups"][0]["algorithms"][0]["name"],
        "test-classifier"
    );
    assert_ne!(
        ann_roundtrip["input"]["sop_instance_uid"],
        ann_roundtrip["output"]["sop_instance_uid"]
    );
    assert_eq!(
        ann_inspect["semantic"]["data"]["groups"][0]["uid"],
        ann_roundtrip["semantic"]["data"]["groups"][0]["uid"]
    );

    let seg_inspect = invoke([
        "inspect",
        "--source",
        source.to_str().unwrap(),
        seg.to_str().unwrap(),
    ]);
    let seg_roundtrip = invoke([
        "roundtrip",
        "--source",
        source.to_str().unwrap(),
        "--output",
        seg_output.to_str().unwrap(),
        seg.to_str().unwrap(),
    ]);
    assert_eq!(seg_inspect["semantic"]["object_type"], "Seg");
    assert_eq!(
        seg_inspect["semantic"]["data"]["masks"]["sha256"],
        seg_roundtrip["semantic"]["data"]["masks"]["sha256"]
    );
    assert_ne!(
        seg_roundtrip["input"]["sop_instance_uid"],
        seg_roundtrip["output"]["sop_instance_uid"]
    );
}

fn invoke<const N: usize>(arguments: [&str; N]) -> serde_json::Value {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = crate::command::execute(
        arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>(),
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(exit, 0, "{}", String::from_utf8_lossy(&stderr));
    serde_json::from_slice(&stdout).unwrap()
}

fn code(value: &str, meaning: &str) -> DicomCode {
    DicomCode::new(value, "99FRAMES", meaning).unwrap()
}

pub(crate) fn write_source_wsi(path: &Path) {
    const SOP_UID: &str = "1.2.826.0.1.3680043.10.777.501";
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
    let mut object = InMemDicomObject::new_empty();
    for element in [
        DataElement::new(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
        ),
        DataElement::new(tags::SOP_INSTANCE_UID, VR::UI, SOP_UID),
        DataElement::new(tags::STUDY_INSTANCE_UID, VR::UI, "2.25.502"),
        DataElement::new(tags::SERIES_INSTANCE_UID, VR::UI, "2.25.503"),
        DataElement::new(tags::FRAME_OF_REFERENCE_UID, VR::UI, "2.25.504"),
        DataElement::new(tags::PATIENT_NAME, VR::PN, "Research^Slide"),
        DataElement::new(tags::PATIENT_ID, VR::LO, "R-1"),
        DataElement::new(tags::STUDY_DATE, VR::DA, "20260813"),
        DataElement::new(tags::STUDY_TIME, VR::TM, "120000"),
        DataElement::new(tags::STUDY_ID, VR::SH, "STUDY-1"),
        DataElement::new(tags::ACCESSION_NUMBER, VR::SH, ""),
        DataElement::new(tags::ROWS, VR::US, PrimitiveValue::from(4_u16)),
        DataElement::new(tags::COLUMNS, VR::US, PrimitiveValue::from(4_u16)),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_ROWS,
            VR::UL,
            PrimitiveValue::from(8_u32),
        ),
        DataElement::new(
            tags::TOTAL_PIXEL_MATRIX_COLUMNS,
            VR::UL,
            PrimitiveValue::from(8_u32),
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
        PrimitiveValue::from(vec![0_u8; 48]),
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
