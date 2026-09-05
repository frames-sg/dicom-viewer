# Pathology workspace storage and privacy

The stable application ID is `io.frames.dicom-viewer`. Platform application
data contains versioned Annotation Schemes and recoverable pathology workspace
revisions; it does not contain the source WSI pixels.

## Source identity and layout

`ViewerStudy::source_identity()` derives a non-PHI identity from the opened
dataset identity, scene, series, Z/C/T plane, and canonical dimensions. The
storage key is its SHA-256 digest, not a patient or window-title string.

```text
<application-data>/
  annotation-schemes/
  workspaces/
    <source-identity-digest>/
      revisions/
        rev-<20-digit-revision>-<sha256>.json
      archive/
        <timestamp>-<uuid>/
```

Each revision embeds the complete `WorkspaceDocument`, pinned Annotation
Scheme snapshot, presentation state required for restoration, and an optional
unfinished polygon `DraftInteraction`. GPU resources, loaded external
payloads, spatial indexes, background jobs, and undo/redo history are runtime
state and are not serialized.

## Write and restore rules

- Changes are debounced for one second. At most one writer is in flight and a
  pending request is replaced by the newest snapshot.
- Serialization runs off the UI thread from an immutable document snapshot.
- A revision is written to a unique temporary file, flushed and synchronized,
  then published to a previously nonexistent digest-addressed filename.
- Revisions are limited to 128 MiB. Documents are also limited to 100,000
  editable tracked objects and five million stored coordinate points.
- The five newest valid revisions per source are retained.
- Restore scans newest to oldest. A corrupt, oversized, unsupported,
  digest-mismatched, or source-identity-mismatched file is skipped without
  partially applying it.
- External source payloads are reloaded on demand. Missing linked files remain
  visible as missing locked layers; when a saved semantic digest is available,
  changed content is rejected rather than substituted behind existing class
  mappings.
- Reopening offers Restore by default with revision/object/draft information.
  **Start fresh** moves the current revision set into an archive instead of
  deleting it.
- Closing while work is pending flushes the writer. A failure presents Retry
  and an explicit Quit Without Saving decision.

Settings → Workspace Storage reports usage, exports the current portable
workspace, purges archives older than 30 days, and provides confirmed deletion
for the current source or all workspace drafts. Undo history intentionally
does not survive restart.

## Privacy boundary

Source directory keys do not include patient names. This is still local
annotation storage, not de-identification:

- annotations, comments, controlled finding sites, and provenance are content;
- portable workspaces embed that content and the pinned terminology;
- linked external-layer metadata may contain local filesystem paths;
- paths and content may appear in the UI, exports, screenshots, or backups.

The viewer does not upload workspaces, implement collaborative review, or provide a
PACS/DICOMweb storage workflow.
