# `annotation-scheme-v1`

Annotation Schemes are versioned controlled terminology snapshots. They are
not per-project label profiles and are not created as a side effect of import.
Every editable workspace pins and embeds one complete scheme snapshot by
scheme ID, positive version, and content digest.

## JSON shape

The parser accepts UTF-8 JSON up to 4 MiB and rejects duplicate object keys,
unknown fields, invalid codes/colors, more than 512 classes, duplicate class
IDs, and duplicate semantic concept keys. A scheme has 1–512 classes and at
most 128 controlled finding sites.

```json
{
  "schema_version": 1,
  "scheme_id": "org.example.pathology",
  "scheme_version": 1,
  "display_name": "Example Pathology",
  "classes": [
    {
      "id": "neoplasm",
      "label": "Neoplasm",
      "geometry": "REGION",
      "display_color": "#D55E00",
      "category": {
        "code_value": "49755003",
        "coding_scheme_designator": "SCT",
        "code_meaning": "Morphologically Abnormal Structure"
      },
      "property_type": {
        "code_value": "108369006",
        "coding_scheme_designator": "SCT",
        "code_meaning": "Neoplasm"
      },
      "property_type_modifiers": []
    }
  ],
  "finding_sites": [
    {
      "code_value": "76752008",
      "coding_scheme_designator": "SCT",
      "code_meaning": "Breast structure"
    }
  ]
}
```

Exactly one of `code_value`, `long_code_value`, or `urn_code_value` is
required for each code. `coding_scheme_version` and valid DICOM context-group
qualifiers are optional. Context qualifiers describe the source from which a
code was actually selected; they must not be fabricated from a similar code.

Class geometry is `REGION` or `POINT`. Class definitions deliberately exclude
anatomy, report templates, export policy, optical path, plane applicability,
and generation provenance. Finding site, optical path, Z/C/T, source frame,
name, comment, and provenance belong to a finding or layer. This avoids class
cross-products such as `breast_neoplasm` and `colon_neoplasm`.

## Identity and matching

`scheme_content_digest` is SHA-256 over the implementation's deterministic
canonical serialization of every normative field: schema, scheme ID/version
and display name, ordered classes, IDs, labels, default colors, geometry,
codes, ordered modifiers, and ordered finding sites. JSON formatting does not
affect it. Changing any normative field requires a different digest.

`class_concept_key` is a separate semantic matching identity. It includes:

- geometry;
- normalized category code identity;
- normalized property-type code identity;
- the sorted property-modifier code identities.

Each code identity includes its value kind/value, coding scheme designator,
and applicable coding-scheme version. It excludes code meaning, label, color,
JSON formatting, context-group provenance, finding site, optical path, plane,
and source provenance. Therefore wording or color changes do not prevent an
exact semantic import suggestion, while a region can never match a point.

The non-breaking core API exposes `AnnotationScheme::from_json`, `to_json`,
class inspection, `content_digest`, each class's `concept_key`, and the public
`annotation_class_concept_key` constructor. `#RRGGBB` colors use one
standards-based sRGB ↔ DICOM Recommended Display CIELab implementation.

## Library governance

- **General Pathology v1** and **Tumor Mask Compatibility v1** are immutable
  built-ins.
- Routine annotation has no New Class, raw-code entry, local alias, or scheme
  authoring control.
- Settings → Annotation Schemes imports a validated JSON snapshot and exposes
  read-only inspection.
- Identical content deduplicates. The same scheme ID/version with different
  content is rejected. Higher versions install side by side.
- Project schemes persist by content digest. They may be removed only when no
  stored or current workspace references their digest; removal archives the
  library file instead of mutating workspaces.
- A workspace embeds the full snapshot. Restoration does not require a global
  library entry and does not silently reinstall one.
- Visibility, lock, and opacity are presentation state; class colors remain
  controlled by the pinned scheme and do not create workspace-local variants.

Imports never install terminology. Exact imported concepts may be visually
associated with the pinned scheme, but unknown concepts stay in their locked
source layer until explicitly mapped and promoted.

## Migration

Switching an empty workspace is immediate. A populated workspace requires one
complete mapping from every used source class to a geometry-compatible target
class. Exact concept matches are suggestions. Region-to-point mappings and
incomplete mappings are rejected. The validated migration updates findings,
segments, rulers, and external mapping targets as one undoable command; it
never silently reclassifies only part of the document.

## Built-in General Pathology v1

| ID | Label | Geometry | Category | Property type | Color |
| --- | --- | --- | --- | --- | --- |
| `tissue` | Tissue | Region | SCT `85756007` | SCT `85756007` | `#999999` |
| `neoplasm` | Neoplasm | Region | SCT `49755003` | SCT `108369006` | `#D55E00` |
| `necrosis` | Necrosis | Region | SCT `49755003` | SCT `6574001` | `#E69F00` |
| `inflammation` | Inflammation | Region | SCT `49755003` | SCT `409774005` | `#F0E442` |
| `stroma` | Stroma / connective tissue | Region | SCT `85756007` | SCT `21793004` | `#009E73` |
| `unusable-tissue` | Unusable tissue | Region | SCT `263496004` | DCM `131502` | `#CC79A7` |
| `cell` | Cell | Point | SCT `4421005` | SCT `4421005` | `#56B4E9` |
| `nucleus` | Nucleus | Point | SCT `4421005` | SCT `84640000` | `#0072B2` |

`Unusable tissue` is intentionally narrow. Analysis exclusion is a
segmentation/workflow operation, not an artifact/ignore terminology class.
