use std::collections::{HashMap, HashSet};

use dicom_viewer_core::{LevelIndex, LevelInfo};
use eframe::egui::{self, pos2, Color32, CornerRadius, Rect, Stroke, StrokeKind};

use super::super::theme;
use super::super::viewport::{tile_screen_rect, CanvasView};
use super::upload::{RegisteredTileTexture, TileUploadError, WgpuTileUploader};
use super::{is_stale_job, DecodedTile, TileFailureInfo, TileKey, TileLoadResult, VisibleTile};

pub(super) enum QueueStatus {
    New,
    Reprioritize,
    Ignore,
}

enum TileState {
    Queued,
    Decoding,
    Decoded {
        tile: DecodedTile,
        byte_len: usize,
        last_used: u64,
    },
    Ready {
        texture: ReadyTexture,
        width: u32,
        height: u32,
        byte_len: usize,
        last_used: u64,
    },
    Failed,
}

enum ReadyTexture {
    Wgpu(RegisteredTileTexture),
    #[cfg(test)]
    Fake(egui::TextureId),
}

impl ReadyTexture {
    fn id(&self) -> egui::TextureId {
        match self {
            Self::Wgpu(texture) => texture.id(),
            #[cfg(test)]
            Self::Fake(id) => *id,
        }
    }
}

pub(super) struct UploadBatchOutcome {
    pub(super) uploaded: usize,
    pub(super) cpu_retries: Vec<TileKey>,
}

pub(super) struct TileStore {
    entries: HashMap<TileKey, TileState>,
    pinned: HashSet<TileKey>,
    displayed_level: Option<LevelIndex>,
    max_resident_bytes: usize,
    use_generation: u64,
    failure_count: usize,
    last_failure: Option<TileFailureInfo>,
    cpu_fallback_count: usize,
    last_cpu_fallback: Option<String>,
    cpu_retry_attempted: HashSet<TileKey>,
}

impl TileStore {
    pub(super) fn new(max_resident_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            pinned: HashSet::new(),
            displayed_level: None,
            max_resident_bytes,
            use_generation: 0,
            failure_count: 0,
            last_failure: None,
            cpu_fallback_count: 0,
            last_cpu_fallback: None,
            cpu_retry_attempted: HashSet::new(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.pinned.clear();
        self.displayed_level = None;
        self.use_generation = 0;
        self.failure_count = 0;
        self.last_failure = None;
        self.cpu_fallback_count = 0;
        self.last_cpu_fallback = None;
        self.cpu_retry_attempted.clear();
    }

    pub(super) fn record_failure(&mut self, latest: String) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_failure = Some(TileFailureInfo {
            count: self.failure_count,
            latest,
        });
    }

    pub(super) fn tile_failure(&self) -> Option<&TileFailureInfo> {
        self.last_failure.as_ref()
    }

    pub(super) fn loading_count(&self) -> usize {
        self.entries
            .values()
            .filter(|state| matches!(state, TileState::Queued | TileState::Decoding))
            .count()
    }

    pub(super) fn cpu_fallback(&self) -> Option<(usize, &str)> {
        self.last_cpu_fallback
            .as_deref()
            .map(|reason| (self.cpu_fallback_count, reason))
    }

    pub(super) fn displayed_level(&self) -> Option<LevelIndex> {
        self.displayed_level
    }

    pub(super) fn set_displayed_level(&mut self, level: LevelIndex) {
        self.displayed_level = Some(level);
    }

    pub(super) fn set_pinned(&mut self, pinned: HashSet<TileKey>) {
        self.pinned = pinned;
        self.evict_resident_tiles();
    }

    pub(super) fn queue(&mut self, key: TileKey) -> QueueStatus {
        match self.entries.get(&key) {
            Some(TileState::Queued) => QueueStatus::Reprioritize,
            Some(
                TileState::Decoding
                | TileState::Decoded { .. }
                | TileState::Ready { .. }
                | TileState::Failed,
            ) => QueueStatus::Ignore,
            None => {
                self.entries.insert(key, TileState::Queued);
                QueueStatus::New
            }
        }
    }

    pub(super) fn mark_decoding(&mut self, keys: &[TileKey], active_generation: u64) {
        for &key in keys {
            if key.generation == active_generation
                && matches!(self.entries.get(&key), Some(TileState::Queued))
            {
                self.entries.insert(key, TileState::Decoding);
            }
        }
    }

    pub(super) fn retain_relevant_pending_tiles(&mut self, keep: &HashSet<TileKey>) {
        self.entries
            .retain(|key, state| !matches!(state, TileState::Queued) || keep.contains(key));
    }

    pub(super) fn stage_finished(
        &mut self,
        active_generation: u64,
        relevant_tiles: &HashSet<TileKey>,
        result: TileLoadResult,
    ) {
        if is_stale_job(result.key, active_generation) {
            self.entries.remove(&result.key);
            return;
        }
        if result.used_cpu_fallback && result.result.is_ok() {
            self.record_cpu_fallback(
                "wsi-rs returned CPU pixels for a Metal-preferred tile; rendering remains on wgpu"
                    .into(),
            );
        }
        match result.result {
            Ok(tile) => {
                if matches!(self.entries.get(&result.key), Some(TileState::Ready { .. })) {
                    return;
                }
                let byte_len = tile.decoded_byte_len();
                let last_used = self.next_use_generation();
                self.entries.insert(
                    result.key,
                    TileState::Decoded {
                        tile,
                        byte_len,
                        last_used,
                    },
                );
                self.evict_resident_tiles();
            }
            Err(err) => {
                if !relevant_tiles.contains(&result.key) {
                    self.entries.remove(&result.key);
                    return;
                }
                self.entries.insert(result.key, TileState::Failed);
                self.record_failure(format!(
                    "level {}, tile {},{}: {err}",
                    result.key.level,
                    result.key.coord.col(),
                    result.key.coord.row()
                ));
            }
        }
    }

    pub(super) fn is_decoded(&self, key: TileKey) -> bool {
        matches!(self.entries.get(&key), Some(TileState::Decoded { .. }))
    }

    pub(super) fn upload_pending_tiles(
        &mut self,
        uploader: &mut WgpuTileUploader,
        keys: &[TileKey],
    ) -> UploadBatchOutcome {
        let mut pending = Vec::new();
        for &key in keys {
            let Some(state) = self.entries.remove(&key) else {
                continue;
            };
            match state {
                TileState::Decoded { tile, .. } => pending.push((key, tile)),
                other => {
                    self.entries.insert(key, other);
                }
            }
        }
        let (pending_keys, pending_tiles): (Vec<_>, Vec<_>) = pending.into_iter().unzip();
        let uploads = uploader.upload_batch(pending_tiles);
        let mut outcome = UploadBatchOutcome {
            uploaded: 0,
            cpu_retries: Vec::new(),
        };
        for (key, upload) in pending_keys.into_iter().zip(uploads) {
            match upload {
                Ok(texture) => {
                    self.cpu_retry_attempted.remove(&key);
                    let (width, height) = texture.dimensions();
                    let byte_len = (width as usize)
                        .saturating_mul(height as usize)
                        .saturating_mul(4);
                    let last_used = self.next_use_generation();
                    self.entries.insert(
                        key,
                        TileState::Ready {
                            texture: ReadyTexture::Wgpu(texture),
                            width,
                            height,
                            byte_len,
                            last_used,
                        },
                    );
                    outcome.uploaded += 1;
                }
                Err(error) => {
                    self.handle_upload_error(key, error, &mut outcome);
                }
            }
        }
        self.evict_resident_tiles();
        outcome
    }

    fn handle_upload_error(
        &mut self,
        key: TileKey,
        error: TileUploadError,
        outcome: &mut UploadBatchOutcome,
    ) {
        if error.permits_cpu_retry() && self.cpu_retry_attempted.insert(key) {
            self.entries.insert(key, TileState::Queued);
            self.record_cpu_fallback(error.to_string());
            outcome.cpu_retries.push(key);
            return;
        }
        self.entries.insert(key, TileState::Failed);
        self.record_failure(format!(
            "level {}, tile {},{}: {error}",
            key.level,
            key.coord.col(),
            key.coord.row()
        ));
    }

    fn record_cpu_fallback(&mut self, reason: String) {
        self.cpu_fallback_count = self.cpu_fallback_count.saturating_add(1);
        self.last_cpu_fallback = Some(reason);
    }

    pub(super) fn pending_tile_count(
        &self,
        tiles: &[VisibleTile],
        render_level_index: LevelIndex,
    ) -> usize {
        tiles
            .iter()
            .filter(|tile| tile.key.level == render_level_index)
            .filter(|tile| {
                matches!(
                    self.entries.get(&tile.key),
                    None | Some(
                        TileState::Queued | TileState::Decoding | TileState::Decoded { .. }
                    )
                )
            })
            .count()
    }

    pub(super) fn draw_ready_tile(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        level: &LevelInfo,
        tile: &VisibleTile,
        center_base: eframe::egui::Vec2,
        zoom: f32,
    ) -> bool {
        let ready = match self.entries.get(&tile.key) {
            Some(TileState::Ready {
                texture,
                width,
                height,
                ..
            }) => Some((texture.id(), *width, *height)),
            Some(TileState::Failed) => {
                let (tile_width, tile_height) = level.tile_layout.display_tile_size();
                let failed_rect = tile_screen_rect(
                    CanvasView {
                        rect,
                        center_base,
                        zoom,
                    },
                    level,
                    tile.key.coord,
                    tile_width,
                    tile_height,
                );
                painter.rect_filled(
                    failed_rect,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(42, 28, 14, 150),
                );
                painter.rect_stroke(
                    failed_rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, theme::WARN),
                    StrokeKind::Inside,
                );
                None
            }
            _ => None,
        };
        let Some((texture_id, width, height)) = ready else {
            return false;
        };

        self.touch(tile.key);
        painter.image(
            texture_id,
            tile_screen_rect(
                CanvasView {
                    rect,
                    center_base,
                    zoom,
                },
                level,
                tile.key.coord,
                width,
                height,
            ),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        true
    }

    fn touch(&mut self, key: TileKey) {
        let generation = self.next_use_generation();
        if let Some(TileState::Ready { last_used, .. }) = self.entries.get_mut(&key) {
            *last_used = generation;
        }
    }

    fn next_use_generation(&mut self) -> u64 {
        let generation = self.use_generation;
        self.use_generation = self.use_generation.saturating_add(1);
        generation
    }

    fn resident_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(|state| match state {
                TileState::Decoded { byte_len, .. } | TileState::Ready { byte_len, .. } => {
                    Some(*byte_len)
                }
                _ => None,
            })
            .fold(0usize, usize::saturating_add)
    }

    fn evict_resident_tiles(&mut self) {
        while self.resident_bytes() > self.max_resident_bytes {
            let candidate =
                self.entries
                    .iter()
                    .filter(|(key, _)| !self.pinned.contains(key))
                    .filter_map(|(key, state)| match state {
                        TileState::Decoded { last_used, .. }
                        | TileState::Ready { last_used, .. } => Some((*last_used, *key)),
                        _ => None,
                    })
                    .min_by_key(|(last_used, key)| (*last_used, *key))
                    .map(|(_, key)| key);
            let Some(candidate) = candidate else {
                break;
            };
            self.entries.remove(&candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use dicom_viewer_core::{LevelIndex, TileCoord};

    use super::*;

    fn key(generation: u64, col: u64) -> TileKey {
        TileKey {
            generation,
            level: LevelIndex::from_u32(0),
            coord: TileCoord::new(col, 0),
        }
    }

    fn decoded(key: TileKey, width: usize, height: usize) -> TileLoadResult {
        TileLoadResult {
            key,
            result: Ok(DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                width: width as u32,
                height: height as u32,
                rgba: vec![255; width * height * 4],
            })),
            used_cpu_fallback: false,
        }
    }

    fn make_ready(store: &mut TileStore, key: TileKey) {
        assert!(matches!(store.queue(key), QueueStatus::New));
        store.stage_finished(key.generation, &HashSet::from([key]), decoded(key, 1, 1));
        let Some(TileState::Decoded { tile, .. }) = store.entries.remove(&key) else {
            panic!("test tile should be decoded");
        };
        let (width, height) = tile.dimensions();
        let last_used = store.next_use_generation();
        store.entries.insert(
            key,
            TileState::Ready {
                texture: ReadyTexture::Fake(egui::TextureId::User(key.coord.col())),
                width,
                height,
                byte_len: width as usize * height as usize * 4,
                last_used,
            },
        );
        store.evict_resident_tiles();
    }

    #[test]
    fn pending_count_is_derived_from_tile_states() {
        let mut store = TileStore::new(64);
        let queued = key(1, 0);
        let decoding = key(1, 1);
        let ready = key(1, 2);
        store.queue(queued);
        store.queue(decoding);
        store.mark_decoding(&[decoding], 1);
        make_ready(&mut store, ready);

        assert_eq!(store.loading_count(), 2);
        assert_eq!(
            store.pending_tile_count(
                &[
                    VisibleTile {
                        key: queued,
                        distance2: 0,
                    },
                    VisibleTile {
                        key: decoding,
                        distance2: 0,
                    },
                    VisibleTile {
                        key: ready,
                        distance2: 0,
                    },
                ],
                LevelIndex::from_u32(0),
            ),
            2
        );
    }

    #[test]
    fn byte_budget_evicts_least_recently_used_unpinned_texture() {
        let mut store = TileStore::new(8);
        let first = key(1, 0);
        let second = key(1, 1);
        let third = key(1, 2);
        make_ready(&mut store, first);
        make_ready(&mut store, second);
        store.touch(first);
        make_ready(&mut store, third);

        assert!(matches!(
            store.entries.get(&first),
            Some(TileState::Ready { .. })
        ));
        assert!(!store.entries.contains_key(&second));
        assert!(matches!(
            store.entries.get(&third),
            Some(TileState::Ready { .. })
        ));
        assert_eq!(store.resident_bytes(), 8);
    }

    #[test]
    fn pinned_texture_survives_byte_budget_eviction() {
        let mut store = TileStore::new(4);
        let pinned = key(1, 0);
        let expendable = key(1, 1);
        store.set_pinned(HashSet::from([pinned, expendable]));
        make_ready(&mut store, pinned);
        make_ready(&mut store, expendable);
        store.set_pinned(HashSet::from([pinned]));

        assert!(matches!(
            store.entries.get(&pinned),
            Some(TileState::Ready { .. })
        ));
        assert!(!store.entries.contains_key(&expendable));
    }

    #[test]
    fn stale_generation_result_is_disposed() {
        let mut store = TileStore::new(64);
        let stale = key(1, 0);
        store.queue(stale);

        store.stage_finished(2, &HashSet::from([stale]), decoded(stale, 1, 1));

        assert!(!store.entries.contains_key(&stale));
        assert_eq!(store.loading_count(), 0);
    }

    #[test]
    fn completed_offscreen_tile_is_cached_for_reuse() {
        let mut store = TileStore::new(64);
        let offscreen = key(1, 0);
        store.queue(offscreen);
        store.mark_decoding(&[offscreen], 1);

        store.stage_finished(1, &HashSet::new(), decoded(offscreen, 1, 1));

        assert!(matches!(
            store.entries.get(&offscreen),
            Some(TileState::Decoded { .. })
        ));
        assert_eq!(store.loading_count(), 0);
    }

    #[test]
    fn cpu_output_from_a_device_preferred_read_is_reported() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        store.queue(tile);
        let mut result = decoded(tile, 1, 1);
        result.used_cpu_fallback = true;

        store.stage_finished(1, &HashSet::from([tile]), result);

        let (count, reason) = store.cpu_fallback().unwrap();
        assert_eq!(count, 1);
        assert!(reason.contains("wsi-rs returned CPU pixels"));
    }

    #[test]
    fn decoded_tiles_share_the_resident_byte_budget() {
        let mut store = TileStore::new(8);
        let first = key(1, 0);
        let second = key(1, 1);
        let third = key(1, 2);

        for tile in [first, second, third] {
            store.queue(tile);
            store.stage_finished(1, &HashSet::new(), decoded(tile, 1, 1));
        }

        assert!(!store.entries.contains_key(&first));
        assert!(matches!(
            store.entries.get(&second),
            Some(TileState::Decoded { .. })
        ));
        assert!(matches!(
            store.entries.get(&third),
            Some(TileState::Decoded { .. })
        ));
    }

    #[test]
    fn pruning_drops_queued_work_but_keeps_inflight_and_decoded_cache() {
        let mut store = TileStore::new(64);
        let queued = key(1, 0);
        let decoding = key(1, 1);
        let decoded_tile = key(1, 2);
        store.queue(queued);
        store.queue(decoding);
        store.mark_decoding(&[decoding], 1);
        store.queue(decoded_tile);
        store.stage_finished(
            1,
            &HashSet::from([decoded_tile]),
            decoded(decoded_tile, 1, 1),
        );

        store.retain_relevant_pending_tiles(&HashSet::new());

        assert!(!store.entries.contains_key(&queued));
        assert!(matches!(
            store.entries.get(&decoding),
            Some(TileState::Decoding)
        ));
        assert!(matches!(
            store.entries.get(&decoded_tile),
            Some(TileState::Decoded { .. })
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metal_upload_failure_schedules_exactly_one_cpu_retry() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        let mut first = UploadBatchOutcome {
            uploaded: 0,
            cpu_retries: Vec::new(),
        };

        store.handle_upload_error(
            tile,
            TileUploadError::MetalTile("synthetic import failure".into()),
            &mut first,
        );

        assert_eq!(first.cpu_retries, vec![tile]);
        assert!(matches!(store.entries.get(&tile), Some(TileState::Queued)));

        let mut second = UploadBatchOutcome {
            uploaded: 0,
            cpu_retries: Vec::new(),
        };
        store.handle_upload_error(
            tile,
            TileUploadError::MetalTile("synthetic repeated failure".into()),
            &mut second,
        );

        assert!(second.cpu_retries.is_empty());
        assert!(matches!(store.entries.get(&tile), Some(TileState::Failed)));
    }
}
