# Pathology annotation workflow

The viewer has one native **Pathology** workspace. The frontend is
`eframe`/`egui`; egui is painted through `epaint` and the eframe `wgpu`
renderer. There is no browser view, Svelte runtime, Tauri bridge, or second UI
state model. Svelte would be a reasonable choice for a document-oriented web
application, but it would add a WebView/IPC boundary here without replacing
the native tile renderer or improving the whole-slide interaction hot path.

## Workspace model

The common path is deliberately close to radiology measurement tracking:
choose a controlled class, create an independently identified finding, and
keep its tracking identity stable across exports. Pathology-specific masks and
dense results remain first-class rather than being forced into radiology-style
vectors.

Two editable representations share the same tools and inspector:

- **Vector findings** are independent points or simple polygon components.
  Each click-created point and each finished polygon is a tracked object.
  Vectors do not have boolean erase or holes.
- **Segmentation segments** are independent tracked masks composed from an
  ordered sequence of polygon or brush Add/Erase primitives. Composition is
  per segment, never per class. Disconnected components and holes therefore
  keep the segment's one identity.

Two objects with the same class are never merged merely because their labels
match. Every manual or promoted vector finding, segment, and ruler receives a
UUID, a monotonically increasing non-reused ordinal, a DICOM Tracking ID, and
a `2.25.<UUID integer>` Tracking UID.

## Controls

The left rail owns one active tool: Pan, Select, Polygon, Brush, Point, or
Ruler. The right panel owns the pinned Annotation Scheme and palette, a
virtualized Findings list, Layers, the selection inspector, and the collapsed
Terminology & DICOM expert controls.

| Input | Action |
| --- | --- |
| `Space` + drag | Momentary pan |
| `V`, `P`, `B`, `K`, `R` | Select, Polygon, Brush, Point, Ruler |
| `N` | Start the next independent finding or segment |
| `Enter` or double-click | Finish a polygon |
| `Escape` | Cancel the innermost interaction or remove one recoverable draft step |
| `Delete` | Delete selected tracked objects |
| `Cmd/Ctrl+Z` | Restore the latest Escape-cancelled draft step, then undo document history |
| `Shift+Cmd/Ctrl+Z` or `Ctrl+Y` | Redo |
| `[` / `]` | Decrease / increase brush diameter |
| `Alt` while segmenting | Temporarily reverse Add and Erase |

Polygon acts on the active representation. In a vector layer it creates a new
region finding. In a segmentation layer it edits the selected segment. Brush
activates the existing editable segmentation layer or creates **Manual
Segmentation** on first use. The first Add operation creates a segment when
none is selected; later strokes continue that segment until `N` or **New
segment**. Erase is unavailable without a selected segment, and an erase that
does not intersect it is a visible no-op.

Ruler creates a tracked measurement with region-class semantics. Its base
pixel endpoints are authoritative; physical length is stored when reliable
anisotropic spacing is available and displayed automatically as µm or mm.
SR export is disabled when reliable frame identity or spacing is unavailable.

Unfinished polygons survive autosave. Changing tool, class, layer, or slide
while a draft exists requires an explicit Resume, Finish, or Discard decision.
Lost pointer capture cancels an in-progress drag or brush stroke without
committing partial geometry.

## Findings and source results

The Findings list contains manual findings, explicitly promoted source
objects, segmentation segments, and rulers. Selection is synchronized with
the canvas; double-click or the jump control centers an object. Shift enables
object-level multi-selection. The inspector supports controlled
reclassification, finding site, optional object name/comment, visibility,
delete, and handle-based geometry edits.

Imported ANN, SEG, SR, profiled GeoJSON, mask, report, and heatmap results
start as locked source layers. Unknown source concepts remain isolated there:
they never create classes or mutate the palette.

For a losslessly editable source layer:

1. Map every source class to a geometry-compatible class in the pinned scheme.
   Exact concept-key matches are offered, not silently applied.
2. Use **Promote** to convert one source object into an independent tracked
   finding, or **Make editable** to convert the complete layer atomically.
3. A valid, non-conflicting source Tracking ID/UID pair is preserved;
   otherwise a new pair is assigned once.

Fractional masks, unsupported ANN geometry, and report content that is not a
compatible two-point length stay read-only. A two-point SR length can be
promoted after mapping. A complete layer cannot become editable until every
source class has an explicit mapping.

## Import, export, and compatibility

**Import** is the entry point for discovered DICOM ANN/SEG/SR sidecars,
profiled GeoJSON plus mapping, raster masks, and heatmap sources. Sidecars are
registered as lightweight stubs before their large coordinate or pixel
payload is loaded. The viewer does not claim DICOM Parametric Map import.

**Export** preflights the selected layers for portable workspace,
scheme-aware GeoJSON, CellViT viable-tumor GeoJSON, ANN, SEG, SR, or PM. It
lists every exclusion and blocks incompatible selected content unless the
user explicitly chooses **Export eligible items only**. Vector-to-SEG
rasterization is a separate explicit option. No target performs a silent lossy
conversion.

Exports run in cancellable background jobs and publish from temporary
destinations. Existing output requires Replace, Choose Another, or Cancel;
the opened source is never an output target. Cancellation or failure leaves an
existing destination unchanged.

The private `VIABLE_TUMOR` / `EXCLUSION` and CellViT++ behavior is available
only through the named Tumor Mask Compatibility scheme and adapter. It is not
generalized into common annotation semantics.

See [Annotation Schemes](ANNOTATION_SCHEME_V1.md), [workspace storage](WORKSPACE_STORAGE.md),
[scheme-aware GeoJSON](FRAMES_PATHOLOGY_GEOJSON_V1.md), and [the compatibility
contract](TUMOR_MASK_COMPATIBILITY.md).
