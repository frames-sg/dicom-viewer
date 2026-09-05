# DICOM-native pathology conversion

The separately versioned `wsi-dicom-annotations` library owns two
conversion boundaries in Rust, while `annotation_probe` exposes them through a
deterministic CLI/report contract:

```text
profiled pathology GeoJSON -> ANN / SEG / Comprehensive 3D SR
raster-valued model output -> Parametric Map
```

Python is not part of either production path. The sibling interoperability
harness uses highdicom and pydicom only as an independent file-level oracle.

The implementation targets DICOM 2026c. It preserves the source Study and
Frame of Reference, creates new Series and SOP Instance identities, and writes
an explicit source derivation reference. A common Frame of Reference means only
that the derived object uses the source slide coordinate system; it does not
claim spatial registration between pathology and radiology.

New outputs identify Manufacturer `Frames`. Desktop exports use Manufacturer
Model Name `DICOM Viewer`; the standalone `wsi-annotation-probe` package in
`wsi-dicom-annotations` uses `Annotation Probe`. ANN, SEG, SR, and PM use
Series Numbers 9101, 9201, 9301, and 9401 respectively, with format-specific
Series Descriptions. Round-tripping an imported object instead retains its
imported equipment identity.

## Commands

```text
annotation_probe convert-geojson \
  --source source-wsi.dcm \
  [--canonical-source level0-wsi.dcm] \
  --mapping pathology-mapping-v1.json \
  --coordinate-space level0-pixels|source-pixels|slide-mm \
  --target ann [--target seg] [--target sr] \
  (--output object.dcm | --output-dir bundle) \
  [--allow-lossy] \
  annotations.geojson
```

One target requires `--output`. Multiple targets require `--output-dir`, which
uses the deterministic names `ann.dcm`, `seg.dcm`, `sr.dcm`, and
`manifest.json`. Every requested geometric target must cover every source
feature; the converter never filters incompatible geometry into a partial
object.

```text
annotation_probe convert-raster \
  --source source-wsi.dcm \
  [--canonical-source level0-wsi.dcm] \
  --profile raster-profile-v1.json \
  [--channel zero-based-index-or-exact-name | --all-channels] \
  (--output map.dcm | --output-dir bundle) \
  [--max-instance-bytes bytes] \
  raster-input
```

A single-channel raster forbids a channel option. A multichannel raster
requires one exact channel selection or `--all-channels`; all channels become
one multi-parameter PM with quantity as an explicit dimension. The default
maximum encoded instance size is exactly 2,000,000,000 bytes. Directory output
splits only at frame boundaries and uses `pm-0001.dcm`, ... plus
`manifest.json`.

Both commands write one `conversion-report-v1` JSON object to stdout on
success or failure, including usage failures for a recognized conversion
command. Human-readable diagnostics go to stderr. Exit status is 0 for
success, 1 for input/conversion failure, and 2 for invalid command usage.

## Desktop viewer workflow

The native viewer exposes the same Rust-owned conversion boundaries through
the unified **Pathology** workspace without making users leave the WSI canvas:

```sh
cargo run -p dicom-viewer -- /path/to/source-wsi.dcm
```

Use **Import** for discovered ANN/SEG/SR sidecars, Profiled GeoJSON, and raster
masks or heatmaps. Discovered DICOM objects first appear in Layers as unloaded
locked stubs. Profiled GeoJSON requires an explicit coordinate space and
mapping profile; its bounded background import registers a read-only source
layer without extending the workspace's pinned Annotation Scheme. Map every
source class before using **Promote** or **Make editable**.

Editable SEG projection uses the explicit `AllowLoss` policy. The viewer keeps
the resulting typed diagnostics and lists every blocking projection-loss code
in status instead of silently discarding identity or applicability losses.

Comprehensive 3D SR import displays its coded report title, procedures,
findings, sites, measurements, qualitative evaluations, and algorithm
identity. Direct SCOORD3D regions are registered to the open slide. If a
report references segmentation frames, use **DICOM SR with companion SEG…**
and choose the companion explicitly; the viewer never guesses a SEG or claims
a relationship based only on filenames.

Raster import requires its profile before the input, so TIFF, NPY,
tiled-manifest files, and local Zarr directories use the correct picker and
adapter. The viewer uses the profile reader's automatic channel selection.
Scanning and PM export are background jobs, while the workspace owns one
bounded, registered heatmap preview. Missing samples remain transparent. The viewer
can export existing heatmap sources as PM using the same 2,000,000,000-byte
default, frame-boundary concatenation, semantic reread, and atomic publication
rules as the CLI. It does not claim PM import until a PM reader exists.

The Export wizard preflights ANN, SEG, SR, and PM eligibility and requires
explicit confirmation before omitting incompatible selected content.
`annotation_probe` remains the deterministic JSON-reporting boundary for
automation, controlled defects, and independent-oracle testing.

## Profiled GeoJSON

Only a GeoJSON `FeatureCollection` is accepted. Coordinates are always `[x,y]`.
The CLI declaration and any feature-level `coordinate_space` must agree.
QuPath full-resolution pixels use `level0-pixels`; `source-pixels` addresses
the selected DICOM pyramid level, and `slide-mm` uses the WSI slide coordinate
system. No coordinate field or axis order is inferred.

Supported geometry is Point, MultiPoint, LineString, MultiLineString, Polygon,
and MultiPolygon. Geometry collections, malformed nesting, nonfinite values,
unclosed linear rings, invalid polygon topology, and out-of-bounds coordinates
are rejected. `objectType` and QuPath `object_type` are aliases and must agree.
Classification accepts `classification.name` and explicit hierarchical
`classification.names`.

Mapping keys are exact. Labels provide coded category/property semantics,
generation and algorithm identity, applicability, display color, and segment
label. Measurements map exact source names to coded concepts and units.
Qualitative evaluations map exact metadata keys and source values to explicit
DICOM codes. Arbitrary strings never become clinical codes or conclusions.

Feature identities have no guessing path. A valid DICOM UID remains unchanged;
a UUID maps reversibly to its `2.25` decimal UID. Any other supplied identity is
rejected. ANN uses the resulting annotation-group UID. SEG and SR also retain
the exact source text as Tracking ID.

| Input geometry or semantics | ANN | SEG | SR |
| --- | --- | --- | --- |
| Point | `POINT` | rejected | TID 1410 `SCOORD3D` |
| Line / MultiLine | `POLYLINE` | rejected | TID 1501/TID 300 when a numeric measurement exists |
| MultiPoint | `POINT` primitives | rejected | TID 1501/TID 300 when a numeric measurement exists |
| Simple polygon | `POLYGON` | sparse binary segment | TID 1410 `SCOORD3D` |
| Polygon with holes | rejected | sparse binary segment | referenced SEG segment and frames |
| Disconnected MultiPolygon | `POLYGON` primitives when hole-free | sparse binary segment | referenced SEG segment and frames |
| Numeric measurement | ANN Measurements Sequence when one primitive; otherwise SR | requires companion SR | coded NUM content |
| Coded qualitative evaluation | requires companion SR | requires companion SR | coded evaluation |

ANN removes the repeated GeoJSON closure and normalizes polygon winding.
SEG uses one segment per source feature and preserves holes, disconnected
components, and overlaps without allocating a full-slide mask. SR is
Comprehensive 3D SR using TID 1500 measurement reports and marks reports
`COMPLETE`, `UNVERIFIED`, and `PRELIMINARY`.

`--allow-lossy` applies only to explicitly reported, nonstructural metadata.
It never permits malformed geometry, invalid identity or codes, lost source
references, unsafe paths, or geometry that a selected target cannot represent.

## Raster profiles and Parametric Maps

`raster-profile-v1` declares the input format (`tiff`, `npy`, `zarr`, or
`tiled-manifest`), dtype, axes, grid origin, sample spacing, coordinate space,
ordered channels, quantity and unit codes, and one algorithm identity. Zarr
also requires a local array path.

Float32 and float64 remain OF and OD respectively. Integer input requires an
explicit slope, intercept, missing sentinel, and output precision. NaNs are
missing values and are canonicalized to one quiet-NaN payload with matching
padding attributes; infinities are rejected. TIFF input rejects lossy
compression, unsupported orientation, and multiple pages. NPY rejects pickle,
object, structured, and complex dtypes. Zarr is restricted to a local
filesystem store, a regular chunk grid, and the compiled common codecs; ZIP,
remote stores, symlinked array entries, and runtime codec registries are not
accepted.

Tiled manifests use relative TIFF or NPY tile paths and explicit sample-grid
origins. Overlap defaults to rejection. The only other policies are
`valid-region-crop`, `mean`, `max`, and `manifest-order-last-write`, and every
applied normalization appears in the conversion report. Rotation, shear, and
raster resampling are outside the profile.

Complete single-quantity grids use `TILED_FULL`. A grid with omitted all-missing
tiles, or a multi-quantity map that needs an explicit quantity dimension, uses
`TILED_SPARSE`. Real World Value Mapping carries the coded unit and Quantity
Definition, and the source WSI supplies slide position, orientation, specimen
context when present, and derivation identity.

The writer plans exact encoded part sizes before creating output. Concatenated
parts share one PM Series UID, Concatenation UID, notional concatenation-source
SOP UID, total, sequential part number, and frame offsets. Pixel writing adds
only the streamed OF/OD value body to the existing DICOM encoder.

## Publication and reports

Destinations must not exist. The CLI rejects aliases and containment with
protected source/input paths, parent traversal, Zarr output containment, and
symlink escapes at the local array and tiled-manifest boundaries. Each DICOM
object is built beside its destination, reread for semantic verification,
fsynced, and published without replacing an existing path. A bundle is exposed
only after every object and its manifest have passed verification. Failure
leaves no published partial output.

`conversion-report-v1` records the consumed inputs, ordered outputs, DICOM
identities, encoded and pixel checksums, target coverage, normalizations and
losses, semantic digest, timing, and tracked peak heap. Profile and GeoJSON
checksums are computed from the exact bounded byte buffers consumed by the
parser. Timing and memory never enter the semantic digest.

This infrastructure does not add model execution,
geographic CRS handling, ontology inference, fractional SEG generation,
radiology/pathology spatial registration, or PACS transport.
