# Tumor Mask Compatibility v1

`org.frames.tumor-mask-compatibility` v1 is an immutable, deliberately narrow
adapter for the viewer's pre-workspace tumor-mask behavior. It is not a model
for creating additional private terminology.

The scheme contains:

- region class `viable-tumor`, with private property code
  `99FRAMES:VIABLE_TUMOR`;
- point class `cell`, retaining the existing SCT cell semantics.

Exclusion is not a palette class. It is expressed by Erase primitives inside
an independently tracked viable-tumor segmentation segment. This avoids a
global “ignore” class and preserves holes, splits, and disconnected mask
components per segment.

## Named compatibility outputs

The adapter is enabled only when the workspace's embedded scheme digest
exactly matches the built-in v1 snapshot.

**CellViT viable-tumor GeoJSON** preserves the prior exact contract:

- one `Feature` per computed viable-tumor component;
- stable export-order names and fragment IDs `F001`, `F002`, …;
- `objectType: "annotation"`;
- `coordinate_space: "level-0_pixels"`;
- `classification.name: "viable_tumor"`;
- polygon interior rings for exclusions;
- no scheme-aware `frames_pathology` metadata.

**Tumor mask compatibility ANN** is a separately named target. It preserves
the private `99FRAMES:VIABLE_TUMOR` group for tumor exteriors and emits
computed holes as private `99FRAMES:EXCLUSION` polygon groups. The viable
group reuses the tracked object's stable UID; exclusion group UIDs are
deterministically derived from that object and remain stable across exports.
This exception does not change general ANN behavior: ordinary ANN export never
converts segmentation content, holes, or subtraction into private semantics.

SEG uses the computed viable-tumor segment with its holes and tracking pair.
The opened WSI is still immutable, and each derived DICOM file receives a new
SOP Instance UID.

Cell points and ruler measurements are not silently omitted from CellViT
GeoJSON; the export flow lists them and requires **Export eligible items
only**. The compatibility adapter never infers these conventions from a
similarly named class or from an imported unknown code.
