# `frames-pathology-geojson-v1`

This is the scheme-aware portable geometry export for a pathology
`WorkspaceDocument`. It is distinct from the exact CellViT viable-tumor
compatibility target.

## Collection metadata

The root is a GeoJSON `FeatureCollection` with a `frames_pathology` member:

```json
{
  "type": "FeatureCollection",
  "frames_pathology": {
    "schema_version": "frames-pathology-geojson-v1",
    "coordinate_space": {
      "name": "SLIDE_BASE_PIXEL",
      "origin": "TOP_LEFT",
      "x_direction": "RIGHT",
      "y_direction": "DOWN"
    },
    "source_identity": {},
    "scheme": {
      "scheme_id": "org.frames.general-pathology",
      "scheme_version": 1,
      "content_digest": "sha256-or-hex-digest"
    }
  },
  "features": []
}
```

`source_identity` is the serialized `ViewerSourceIdentity`: dataset identity,
scene/series/plane, and canonical dimensions without a patient name.

## Features

There is exactly one feature per tracked vector finding or segmentation
segment, including a segment or finding with disconnected components. The
GeoJSON feature `id` and `properties.object_id` are that object's UUID.

Properties are:

- `object_id` and monotonic `ordinal`;
- stable `tracking_id` and DICOM-valid `tracking_uid`;
- pinned-scheme `class_id`;
- optional controlled `finding_site`, `name`, and `comment`;
- `source_layer_id` and `source_layer_name`;
- `representation`: `VECTOR_FINDING` or `SEGMENTATION_SEGMENT`;
- structured object `provenance` and `source_frame`.

Vector points use GeoJSON `Point`. One region component uses `Polygon`;
multiple components use `MultiPolygon`. Segmentation exports its computed
Add/Erase result rather than editing primitives, and preserves holes as
interior rings. Rulers are intentionally excluded; export preflight directs
them to DICOM SR and reports the excluded count.

## Canonical output

Export is deterministic for an unchanged workspace:

- features sort by workspace ordinal and then UUID;
- polygon components and holes sort lexicographically by canonical ring;
- every ring is rotated to its lexicographically smallest first coordinate;
- rings are explicitly closed;
- in top-left/Y-down slide coordinates, exteriors use positive shoelace area
  and holes use negative area;
- negative zero is normalized to zero.

Stored geometry is never changed by canonicalization. One feature per tracked
object prevents component or feature-ID collisions and keeps tracking stable
when a segment splits into disconnected pieces.

## Interchange limits

This format does not imply ANN or SEG eligibility. ANN still accepts only
directly representable vector points and independent simple polygons; it does
not invent hole or subtraction semantics. SEG accepts editable segmentation
segments directly and accepts vector findings only after the explicit
**Rasterize vector findings into SEG** choice. Exporter preflight derives
eligibility from actual geometry, representation, and source-frame context.
