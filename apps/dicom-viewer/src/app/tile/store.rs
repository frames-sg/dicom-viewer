use std::collections::{HashMap, HashSet};
use std::time::Duration;

use dicom_viewer_core::{LevelIndex, LevelInfo};
use eframe::egui::{self, pos2, Color32, Rect};

use super::super::viewport::{tile_screen_rect, CanvasView};
use super::upload::{
    BudgetedUploadOutcome, RegisteredTileTexture, TileUploadError, TileUploadSink,
};
use super::{
    is_stale_job, loader::TileReadMode, DecodedTile, TileFailureInfo, TileKey, TileLoadOutcome,
    TileLoadResult, VisibleTile,
};

#[cfg(test)]
pub(super) enum QueueStatus {
    New,
    Reprioritize,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TileDemandStatus {
    Queue(TileReadMode),
    Ready,
    Deduplicated,
    Failed,
}

enum TileState {
    Queued {
        read_mode: TileReadMode,
    },
    Decoding {
        read_mode: TileReadMode,
    },
    Decoded {
        tile: DecodedTile,
        read_mode: TileReadMode,
        byte_len: usize,
        last_used: u64,
    },
    Uploading {
        read_mode: TileReadMode,
        source_byte_len: usize,
        texture_byte_len: usize,
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
    pub(super) deferred: usize,
    pub(super) cpu_retries: Vec<TileKey>,
    pub(super) failures: usize,
}

struct UploadBatch {
    keys: Vec<TileKey>,
    tiles: Vec<DecodedTile>,
    deferred: usize,
}

pub(super) struct TileStore {
    entries: HashMap<TileKey, TileState>,
    pinned: HashSet<TileKey>,
    overview_reserved: HashSet<TileKey>,
    max_resident_bytes: usize,
    resident_bytes: usize,
    pinned_bytes: usize,
    measure_eviction: bool,
    pending_eviction_samples: Vec<Duration>,
    use_generation: u64,
    failure_count: usize,
    last_failure: Option<TileFailureInfo>,
    cpu_fallback_count: usize,
    last_cpu_fallback: Option<String>,
}

impl TileStore {
    #[cfg(test)]
    pub(super) fn new(max_resident_bytes: usize) -> Self {
        Self::with_eviction_diagnostics(max_resident_bytes, false)
    }

    pub(super) fn with_eviction_diagnostics(
        max_resident_bytes: usize,
        measure_eviction: bool,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            pinned: HashSet::new(),
            overview_reserved: HashSet::new(),
            max_resident_bytes,
            resident_bytes: 0,
            pinned_bytes: 0,
            measure_eviction,
            pending_eviction_samples: Vec::new(),
            use_generation: 0,
            failure_count: 0,
            last_failure: None,
            cpu_fallback_count: 0,
            last_cpu_fallback: None,
        }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.pinned.clear();
        self.overview_reserved.clear();
        self.resident_bytes = 0;
        self.pinned_bytes = 0;
        self.pending_eviction_samples.clear();
        self.use_generation = 0;
        self.failure_count = 0;
        self.last_failure = None;
        self.cpu_fallback_count = 0;
        self.last_cpu_fallback = None;
    }

    pub(super) fn record_failure(&mut self, latest: String) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_failure = Some(TileFailureInfo {
            count: self.failure_count,
            latest,
        });
    }

    fn record_terminal_failure(&mut self, key: TileKey, message: String) {
        if matches!(self.entries.get(&key), Some(TileState::Failed)) {
            return;
        }
        self.insert_entry(key, TileState::Failed);
        self.record_failure(format!(
            "level {}, tile {},{}: {message}",
            key.level,
            key.coord.col(),
            key.coord.row(),
        ));
    }

    pub(super) fn reject_texture_preflight(
        &mut self,
        key: TileKey,
        texture_bytes: Result<usize, String>,
    ) -> bool {
        if matches!(self.entries.get(&key), Some(TileState::Failed)) {
            return true;
        }
        match texture_bytes {
            Ok(texture_bytes) if texture_bytes <= self.max_resident_bytes => false,
            Ok(texture_bytes) => {
                self.record_terminal_failure(
                    key,
                    format!(
                        "final RGBA texture requires {texture_bytes} bytes, exceeding the {}-byte viewer ceiling",
                        self.max_resident_bytes
                    ),
                );
                true
            }
            Err(error) => {
                self.record_terminal_failure(key, error);
                true
            }
        }
    }

    pub(super) fn tile_failure(&self) -> Option<&TileFailureInfo> {
        self.last_failure.as_ref()
    }

    #[cfg(test)]
    pub(super) fn loading_count(&self) -> usize {
        self.entries
            .values()
            .filter(|state| is_loading_state(state))
            .count()
    }

    pub(super) fn loading_count_for(&self, active: &HashSet<TileKey>) -> usize {
        self.entries
            .iter()
            .filter(|(key, state)| active.contains(key) && is_loading_state(state))
            .count()
    }

    pub(super) fn cpu_fallback(&self) -> Option<(usize, &str)> {
        self.last_cpu_fallback
            .as_deref()
            .map(|reason| (self.cpu_fallback_count, reason))
    }

    pub(super) fn set_cache_protection(
        &mut self,
        pinned: HashSet<TileKey>,
        overview_reserved: HashSet<TileKey>,
    ) {
        debug_assert!(overview_reserved.is_subset(&pinned));
        let unpinned_bytes = self
            .pinned
            .difference(&pinned)
            .filter_map(|key| self.entries.get(key).and_then(resident_byte_len))
            .fold(0usize, usize::saturating_add);
        let newly_pinned_bytes = pinned
            .difference(&self.pinned)
            .filter_map(|key| self.entries.get(key).and_then(resident_byte_len))
            .fold(0usize, usize::saturating_add);
        self.pinned_bytes = self
            .pinned_bytes
            .saturating_sub(unpinned_bytes)
            .saturating_add(newly_pinned_bytes);
        self.pinned = pinned;
        self.overview_reserved = overview_reserved;
        self.evict_resident_tiles();
    }

    #[cfg(test)]
    pub(super) fn set_pinned(&mut self, pinned: HashSet<TileKey>) {
        self.set_cache_protection(pinned, HashSet::new());
    }

    #[cfg(test)]
    pub(super) fn queue(&mut self, key: TileKey) -> QueueStatus {
        match self.entries.get(&key) {
            Some(TileState::Queued { .. }) => QueueStatus::Reprioritize,
            Some(
                TileState::Decoding { .. }
                | TileState::Decoded { .. }
                | TileState::Uploading { .. }
                | TileState::Ready { .. }
                | TileState::Failed,
            ) => QueueStatus::Ignore,
            None => {
                self.insert_entry(
                    key,
                    TileState::Queued {
                        read_mode: TileReadMode::Preferred,
                    },
                );
                QueueStatus::New
            }
        }
    }

    pub(super) fn queue_for_demand(&mut self, key: TileKey) -> TileDemandStatus {
        match self.entries.get(&key) {
            Some(TileState::Queued { read_mode }) => TileDemandStatus::Queue(*read_mode),
            Some(
                TileState::Decoding { .. }
                | TileState::Decoded { .. }
                | TileState::Uploading { .. },
            ) => TileDemandStatus::Deduplicated,
            Some(TileState::Ready { .. }) => TileDemandStatus::Ready,
            Some(TileState::Failed) => TileDemandStatus::Failed,
            None => {
                self.insert_entry(
                    key,
                    TileState::Queued {
                        read_mode: TileReadMode::Preferred,
                    },
                );
                TileDemandStatus::Queue(TileReadMode::Preferred)
            }
        }
    }

    pub(super) fn mark_decoding(&mut self, keys: &[TileKey], active_generation: u64) {
        for &key in keys {
            if key.generation == active_generation {
                let read_mode = match self.entries.get(&key) {
                    Some(TileState::Queued { read_mode }) => Some(*read_mode),
                    _ => None,
                };
                if let Some(read_mode) = read_mode {
                    self.insert_entry(key, TileState::Decoding { read_mode });
                }
            }
        }
    }

    pub(super) fn retain_relevant_pending_tiles(&mut self, keep: &HashSet<TileKey>) {
        self.entries.retain(|key, state| {
            !matches!(state, TileState::Queued { .. } | TileState::Decoding { .. })
                || keep.contains(key)
        });
    }

    pub(super) fn discard_queued_tiles(&mut self, keys: &[TileKey]) {
        for key in keys {
            if matches!(self.entries.get(key), Some(TileState::Queued { .. })) {
                self.remove_entry(key);
            }
        }
    }

    pub(super) fn stage_finished(
        &mut self,
        active_generation: u64,
        relevant_tiles: &HashSet<TileKey>,
        result: TileLoadResult,
    ) -> bool {
        if is_stale_job(result.key, active_generation) {
            self.remove_entry(&result.key);
            return true;
        }
        if result.used_cpu_fallback && matches!(&result.outcome, TileLoadOutcome::Decoded(_)) {
            self.record_cpu_fallback(
                "wsi-rs returned CPU pixels for a device-preferred tile; rendering remains on wgpu"
                    .into(),
            );
        }
        match result.outcome {
            TileLoadOutcome::Decoded(tile) => {
                if !relevant_tiles.contains(&result.key) && !self.pinned.contains(&result.key) {
                    self.remove_entry(&result.key);
                    return true;
                }
                if matches!(self.entries.get(&result.key), Some(TileState::Ready { .. })) {
                    return false;
                }
                if matches!(self.entries.get(&result.key), Some(TileState::Failed)) {
                    return false;
                }
                let cost = match tile.memory_cost() {
                    Ok(cost) => cost,
                    Err(error) => {
                        self.record_terminal_failure(result.key, error);
                        return false;
                    }
                };
                if cost.decoded_bytes > self.max_resident_bytes {
                    self.record_terminal_failure(
                        result.key,
                        format!(
                            "decoded allocation requires {} bytes, exceeding the {}-byte viewer ceiling",
                            cost.decoded_bytes, self.max_resident_bytes
                        ),
                    );
                    return false;
                }
                if cost.upload_peak_bytes > self.max_resident_bytes {
                    self.record_terminal_failure(
                        result.key,
                        format!(
                            "upload requires {} source-plus-texture bytes, exceeding the {}-byte viewer ceiling",
                            cost.upload_peak_bytes, self.max_resident_bytes
                        ),
                    );
                    return false;
                }
                let byte_len = cost.decoded_bytes;
                let last_used = self.next_use_generation();
                let read_mode = match self.entries.get(&result.key) {
                    Some(TileState::Queued { read_mode } | TileState::Decoding { read_mode }) => {
                        *read_mode
                    }
                    _ => TileReadMode::Preferred,
                };
                self.insert_entry(
                    result.key,
                    TileState::Decoded {
                        tile,
                        read_mode,
                        byte_len,
                        last_used,
                    },
                );
                self.evict_resident_tiles();
            }
            TileLoadOutcome::Cancelled => {
                if matches!(
                    self.entries.get(&result.key),
                    Some(TileState::Queued { .. } | TileState::Decoding { .. })
                ) {
                    self.remove_entry(&result.key);
                }
            }
            TileLoadOutcome::Failed(failure) => {
                if !relevant_tiles.contains(&result.key) {
                    self.remove_entry(&result.key);
                    return false;
                }
                self.insert_entry(result.key, TileState::Failed);
                self.record_failure(format!(
                    "level {}, tile {},{}: {}",
                    result.key.level,
                    result.key.coord.col(),
                    result.key.coord.row(),
                    failure.message,
                ));
            }
        }
        false
    }

    pub(super) fn is_decoded(&self, key: TileKey) -> bool {
        matches!(self.entries.get(&key), Some(TileState::Decoded { .. }))
    }

    pub(super) fn has_pending_or_missing(&self, tiles: &[VisibleTile]) -> bool {
        tiles.iter().any(|tile| {
            matches!(
                self.entries.get(&tile.key),
                None | Some(
                    TileState::Queued { .. }
                        | TileState::Decoding { .. }
                        | TileState::Decoded { .. }
                        | TileState::Uploading { .. }
                )
            )
        })
    }

    fn begin_upload_batch(&mut self, keys: &[TileKey]) -> UploadBatch {
        let mut batch = UploadBatch {
            keys: Vec::new(),
            tiles: Vec::new(),
            deferred: 0,
        };
        for &key in keys {
            let Some(state) = self.remove_entry(&key) else {
                continue;
            };
            let TileState::Decoded {
                tile,
                read_mode,
                byte_len,
                last_used,
            } = state
            else {
                self.insert_entry(key, state);
                continue;
            };
            let cost = match tile.memory_cost() {
                Ok(cost) => cost,
                Err(error) => {
                    self.record_terminal_failure(key, error);
                    continue;
                }
            };
            if cost.decoded_bytes != byte_len {
                self.record_terminal_failure(
                    key,
                    "decoded allocation changed before upload reservation".into(),
                );
                continue;
            }
            if !self.evict_to_fit(cost.upload_peak_bytes) {
                self.insert_entry(
                    key,
                    TileState::Decoded {
                        tile,
                        read_mode,
                        byte_len,
                        last_used,
                    },
                );
                self.evict_resident_tiles();
                batch.deferred = batch.deferred.saturating_add(1);
                continue;
            }
            self.insert_entry(
                key,
                TileState::Uploading {
                    read_mode,
                    source_byte_len: cost.decoded_bytes,
                    texture_byte_len: cost.texture_bytes,
                    byte_len: cost.upload_peak_bytes,
                    last_used,
                },
            );
            batch.keys.push(key);
            batch.tiles.push(tile);
        }
        batch
    }

    pub(super) fn upload_pending_tiles<U: TileUploadSink>(
        &mut self,
        uploader: &mut U,
        keys: &[TileKey],
        cpu_budget: Duration,
    ) -> UploadBatchOutcome {
        let batch = self.begin_upload_batch(keys);
        let expected_uploads = batch.keys.len();
        let uploads = uploader.upload_batch_budgeted(batch.tiles, cpu_budget);
        let mut outcome = UploadBatchOutcome {
            uploaded: 0,
            deferred: batch.deferred,
            cpu_retries: Vec::new(),
            failures: 0,
        };
        let actual_uploads = uploads.len();
        let mut uploads = uploads.into_iter();
        for key in batch.keys {
            let Some(upload) = uploads.next() else {
                self.remove_entry(&key);
                self.record_terminal_failure(
                    key,
                    format!(
                        "uploader returned {actual_uploads} outcomes for {expected_uploads} owned inputs"
                    ),
                );
                outcome.failures = outcome.failures.saturating_add(1);
                continue;
            };
            let Some(TileState::Uploading {
                read_mode,
                source_byte_len,
                texture_byte_len,
                last_used,
                ..
            }) = self.remove_entry(&key)
            else {
                self.record_terminal_failure(
                    key,
                    "upload ownership state disappeared before reconciliation".into(),
                );
                outcome.failures = outcome.failures.saturating_add(1);
                continue;
            };
            match upload {
                BudgetedUploadOutcome::Ready(texture) => {
                    let (width, height) = texture.dimensions();
                    let byte_len = usize::try_from(width)
                        .ok()
                        .and_then(|width| {
                            usize::try_from(height)
                                .ok()
                                .and_then(|height| width.checked_mul(height))
                        })
                        .and_then(|pixels| pixels.checked_mul(4));
                    let Some(byte_len) = byte_len else {
                        self.record_terminal_failure(
                            key,
                            format!("uploaded texture dimensions {width}x{height} overflow bytes"),
                        );
                        outcome.failures = outcome.failures.saturating_add(1);
                        continue;
                    };
                    if byte_len != texture_byte_len || byte_len > self.max_resident_bytes {
                        self.record_terminal_failure(
                            key,
                            format!(
                                "uploader produced {byte_len} texture bytes after reserving {texture_byte_len}"
                            ),
                        );
                        outcome.failures = outcome.failures.saturating_add(1);
                        continue;
                    }
                    let last_used = self.next_use_generation();
                    self.insert_entry(
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
                BudgetedUploadOutcome::Failed(error) => {
                    self.handle_upload_error(key, read_mode, error, &mut outcome);
                }
                BudgetedUploadOutcome::Deferred(tile) => {
                    let cost = match tile.memory_cost() {
                        Ok(cost)
                            if cost.decoded_bytes == source_byte_len
                                && cost.texture_bytes == texture_byte_len =>
                        {
                            cost
                        }
                        Ok(_) => {
                            self.record_terminal_failure(
                                key,
                                "uploader returned a different decoded allocation for a deferred tile"
                                    .into(),
                            );
                            outcome.failures = outcome.failures.saturating_add(1);
                            continue;
                        }
                        Err(error) => {
                            self.record_terminal_failure(key, error);
                            outcome.failures = outcome.failures.saturating_add(1);
                            continue;
                        }
                    };
                    self.insert_entry(
                        key,
                        TileState::Decoded {
                            tile,
                            read_mode,
                            byte_len: cost.decoded_bytes,
                            last_used,
                        },
                    );
                    outcome.deferred = outcome.deferred.saturating_add(1);
                }
            }
        }
        let surplus = uploads.count();
        if surplus > 0 {
            outcome.failures = outcome.failures.saturating_add(1);
            self.record_failure(format!(
                "uploader returned {actual_uploads} outcomes for {expected_uploads} owned inputs; discarded {surplus} surplus outcome(s)"
            ));
        }
        self.evict_resident_tiles();
        outcome
    }

    fn handle_upload_error(
        &mut self,
        key: TileKey,
        read_mode: TileReadMode,
        error: TileUploadError,
        outcome: &mut UploadBatchOutcome,
    ) {
        if error.permits_cpu_retry() && read_mode == TileReadMode::Preferred {
            self.insert_entry(
                key,
                TileState::Queued {
                    read_mode: TileReadMode::CpuFallback,
                },
            );
            self.record_cpu_fallback(error.to_string());
            outcome.cpu_retries.push(key);
            return;
        }
        self.insert_entry(key, TileState::Failed);
        outcome.failures = outcome.failures.saturating_add(1);
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

    pub(super) fn uncovered_tile_count(
        &self,
        tiles: &[VisibleTile],
        render_level_index: LevelIndex,
    ) -> usize {
        let coverage = self.coverage(tiles, render_level_index);
        coverage.pending + coverage.failed + coverage.missing
    }

    pub(super) fn coverage(
        &self,
        tiles: &[VisibleTile],
        render_level_index: LevelIndex,
    ) -> TileCoverage {
        let mut coverage = TileCoverage::default();
        for tile in tiles
            .iter()
            .filter(|tile| tile.key.level == render_level_index)
        {
            match self.entries.get(&tile.key) {
                Some(TileState::Ready { .. }) => coverage.ready += 1,
                Some(
                    TileState::Queued { .. }
                    | TileState::Decoding { .. }
                    | TileState::Decoded { .. }
                    | TileState::Uploading { .. },
                ) => coverage.pending += 1,
                Some(TileState::Failed) => coverage.failed += 1,
                None => coverage.missing += 1,
            }
        }
        coverage
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
        let ready = self.ready_draw_info(tile.key);
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

    fn ready_draw_info(&self, key: TileKey) -> Option<(egui::TextureId, u32, u32)> {
        match self.entries.get(&key) {
            Some(TileState::Ready {
                texture,
                width,
                height,
                ..
            }) => Some((texture.id(), *width, *height)),
            _ => None,
        }
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

    pub(super) fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    pub(super) fn pinned_bytes(&self) -> usize {
        self.pinned_bytes
    }

    pub(super) fn take_eviction_samples(&mut self) -> Vec<Duration> {
        std::mem::take(&mut self.pending_eviction_samples)
    }

    fn insert_entry(&mut self, key: TileKey, state: TileState) -> Option<TileState> {
        let previous = self.remove_entry(&key);
        if let Some(byte_len) = resident_byte_len(&state) {
            self.resident_bytes = self.resident_bytes.saturating_add(byte_len);
            if self.pinned.contains(&key) {
                self.pinned_bytes = self.pinned_bytes.saturating_add(byte_len);
            }
        }
        self.entries.insert(key, state);
        previous
    }

    fn remove_entry(&mut self, key: &TileKey) -> Option<TileState> {
        let removed = self.entries.remove(key)?;
        if let Some(byte_len) = resident_byte_len(&removed) {
            self.resident_bytes = self.resident_bytes.saturating_sub(byte_len);
            if self.pinned.contains(key) {
                self.pinned_bytes = self.pinned_bytes.saturating_sub(byte_len);
            }
        }
        Some(removed)
    }

    fn evict_to_fit(&mut self, additional_bytes: usize) -> bool {
        let Some(required) = self.resident_bytes.checked_add(additional_bytes) else {
            return false;
        };
        if required <= self.max_resident_bytes {
            return true;
        }
        let started = self.measure_eviction.then(std::time::Instant::now);
        let mut candidates = self
            .entries
            .iter()
            .filter_map(|(key, state)| match state {
                TileState::Decoded { last_used, .. } | TileState::Ready { last_used, .. } => {
                    let protection = if self.overview_reserved.contains(key) {
                        2
                    } else if self.pinned.contains(key) {
                        1
                    } else {
                        0
                    };
                    Some((protection, *last_used, *key))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        for (_, _, key) in candidates {
            if self
                .resident_bytes
                .checked_add(additional_bytes)
                .is_some_and(|required| required <= self.max_resident_bytes)
            {
                break;
            }
            self.remove_entry(&key);
        }
        if let Some(started) = started {
            self.pending_eviction_samples.push(started.elapsed());
        }
        self.resident_bytes
            .checked_add(additional_bytes)
            .is_some_and(|required| required <= self.max_resident_bytes)
    }

    fn evict_resident_tiles(&mut self) {
        let fits = self.evict_to_fit(0);
        debug_assert!(fits);
    }
}

fn resident_byte_len(state: &TileState) -> Option<usize> {
    match state {
        TileState::Decoded { byte_len, .. }
        | TileState::Uploading { byte_len, .. }
        | TileState::Ready { byte_len, .. } => Some(*byte_len),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::app) struct TileCoverage {
    pub(in crate::app) ready: usize,
    pub(in crate::app) pending: usize,
    pub(in crate::app) failed: usize,
    pub(in crate::app) missing: usize,
}

fn is_loading_state(state: &TileState) -> bool {
    matches!(
        state,
        TileState::Queued { .. }
            | TileState::Decoding { .. }
            | TileState::Decoded { .. }
            | TileState::Uploading { .. }
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dicom_viewer_core::{LevelIndex, TileCoord};

    use super::super::upload::{BudgetedUploadOutcome, TileUploadSink};
    use super::*;

    #[derive(Default)]
    struct DeferringUploader {
        calls: usize,
        batch_sizes: Vec<usize>,
        budgets: Vec<Duration>,
    }

    struct MissingOutcomeUploader;

    impl TileUploadSink for MissingOutcomeUploader {
        fn upload_batch_budgeted(
            &mut self,
            _tiles: Vec<DecodedTile>,
            _cpu_budget: Duration,
        ) -> Vec<BudgetedUploadOutcome> {
            Vec::new()
        }
    }

    struct SurplusOutcomeUploader;

    impl TileUploadSink for SurplusOutcomeUploader {
        fn upload_batch_budgeted(
            &mut self,
            mut tiles: Vec<DecodedTile>,
            _cpu_budget: Duration,
        ) -> Vec<BudgetedUploadOutcome> {
            let owned = tiles.pop().expect("one planned tile");
            vec![
                BudgetedUploadOutcome::Deferred(owned),
                BudgetedUploadOutcome::Deferred(DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 255],
                })),
            ]
        }
    }

    impl TileUploadSink for DeferringUploader {
        fn upload_batch_budgeted(
            &mut self,
            tiles: Vec<DecodedTile>,
            cpu_budget: Duration,
        ) -> Vec<BudgetedUploadOutcome> {
            self.calls += 1;
            self.batch_sizes.push(tiles.len());
            self.budgets.push(cpu_budget);
            tiles
                .into_iter()
                .map(BudgetedUploadOutcome::Deferred)
                .collect()
        }
    }

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
            outcome: TileLoadOutcome::Decoded(DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                width: width as u32,
                height: height as u32,
                rgba: vec![255; width * height * 4],
            })),
            used_cpu_fallback: false,
        }
    }

    fn make_ready(store: &mut TileStore, key: TileKey) {
        assert!(matches!(store.queue(key), QueueStatus::New));
        let last_used = store.next_use_generation();
        store.insert_entry(
            key,
            TileState::Ready {
                texture: ReadyTexture::Fake(egui::TextureId::User(key.coord.col())),
                width: 1,
                height: 1,
                byte_len: 4,
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
            store.uncovered_tile_count(
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
        store.set_cache_protection(HashSet::from([pinned, expendable]), HashSet::from([pinned]));
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
    fn over_budget_pinned_demand_keeps_the_overview_without_exceeding_the_cache_ceiling() {
        let mut store = TileStore::new(8);
        let overview = key(1, 0);
        let foreground_near = key(1, 1);
        let foreground_far = key(1, 2);
        store.set_cache_protection(
            HashSet::from([overview, foreground_near, foreground_far]),
            HashSet::from([overview]),
        );

        make_ready(&mut store, overview);
        make_ready(&mut store, foreground_near);
        make_ready(&mut store, foreground_far);

        assert!(
            store.resident_bytes() <= 8,
            "foreground demand protection must not turn the byte budget into a soft limit"
        );
        assert!(matches!(
            store.entries.get(&overview),
            Some(TileState::Ready { .. })
        ));
        assert_eq!(
            [foreground_near, foreground_far]
                .into_iter()
                .filter(|key| store.entries.contains_key(key))
                .count(),
            1,
            "one foreground tile should remain alongside the reserved overview"
        );
    }

    #[test]
    fn pinned_overview_is_reused_after_deep_zoom_without_a_new_decode() {
        let mut store = TileStore::new(8);
        let overview = key(1, 0);
        let close_up = key(1, 1);
        store.set_cache_protection(HashSet::from([overview]), HashSet::from([overview]));
        make_ready(&mut store, overview);
        make_ready(&mut store, close_up);
        store.set_pinned(HashSet::from([overview]));

        assert!(matches!(
            store.entries.get(&overview),
            Some(TileState::Ready { .. })
        ));
        assert!(matches!(store.queue(overview), QueueStatus::Ignore));
    }

    #[test]
    fn demand_classifies_ready_hits_separately_from_queued_and_inflight_work() {
        let mut store = TileStore::new(64);
        let ready = key(1, 0);
        let queued = key(1, 1);
        let decoding = key(1, 2);
        make_ready(&mut store, ready);
        store.queue(queued);
        store.queue(decoding);
        store.mark_decoding(&[decoding], 1);

        assert_eq!(store.queue_for_demand(ready), TileDemandStatus::Ready);
        assert_eq!(
            store.queue_for_demand(queued),
            TileDemandStatus::Queue(TileReadMode::Preferred)
        );
        assert_eq!(
            store.queue_for_demand(decoding),
            TileDemandStatus::Deduplicated
        );
    }

    #[test]
    fn pinned_bytes_count_only_resident_pinned_tiles() {
        let mut store = TileStore::new(64);
        let pinned = key(1, 0);
        let unpinned = key(1, 1);
        make_ready(&mut store, pinned);
        make_ready(&mut store, unpinned);
        store.set_pinned(HashSet::from([pinned]));

        assert_eq!(store.resident_bytes(), 8);
        assert_eq!(store.pinned_bytes(), 4);
    }

    #[test]
    fn viewer_budget_keeps_a_256_mib_resident_working_set() {
        let mut store = TileStore::new(crate::app::canvas::TILE_RESIDENT_CACHE_BYTES);
        let first = key(1, 0);
        let second = key(1, 1);
        for tile in [first, second] {
            let last_used = store.next_use_generation();
            store.insert_entry(
                tile,
                TileState::Ready {
                    texture: ReadyTexture::Fake(egui::TextureId::User(tile.coord.col())),
                    width: 1,
                    height: 1,
                    byte_len: 128 * 1024 * 1024,
                    last_used,
                },
            );
            store.evict_resident_tiles();
        }

        assert!(matches!(
            store.entries.get(&first),
            Some(TileState::Ready { .. })
        ));
        assert!(matches!(
            store.entries.get(&second),
            Some(TileState::Ready { .. })
        ));
        assert_eq!(store.resident_bytes(), 256 * 1024 * 1024);
    }

    #[test]
    fn eviction_diagnostics_measure_only_real_over_budget_eviction() {
        let mut store = TileStore::with_eviction_diagnostics(4, true);
        make_ready(&mut store, key(1, 0));
        assert!(store.take_eviction_samples().is_empty());

        make_ready(&mut store, key(1, 1));

        assert_eq!(store.take_eviction_samples().len(), 1);
        assert_eq!(store.resident_bytes(), 4);
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
    fn completed_obsolete_tile_is_discarded_instead_of_consuming_cache() {
        let mut store = TileStore::new(64);
        let offscreen = key(1, 0);
        store.queue(offscreen);
        store.mark_decoding(&[offscreen], 1);

        store.stage_finished(1, &HashSet::new(), decoded(offscreen, 1, 1));

        assert!(!store.entries.contains_key(&offscreen));
        assert_eq!(store.loading_count(), 0);
    }

    #[test]
    fn decoded_awaiting_upload_counts_as_loading() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        store.queue(tile);
        store.mark_decoding(&[tile], 1);
        store.stage_finished(1, &HashSet::from([tile]), decoded(tile, 1, 1));

        assert_eq!(store.loading_count(), 1);
    }

    #[test]
    fn decoded_or_upload_peak_over_budget_is_terminal_and_never_requeues() {
        let mut store = TileStore::new(7);
        let tile = key(1, 0);
        store.queue(tile);

        store.stage_finished(1, &HashSet::from([tile]), decoded(tile, 1, 1));

        assert!(matches!(store.entries.get(&tile), Some(TileState::Failed)));
        assert_eq!(store.resident_bytes(), 0);
        assert_eq!(store.queue_for_demand(tile), TileDemandStatus::Failed);
        assert_eq!(store.tile_failure().map(|failure| failure.count), Some(1));
    }

    #[test]
    fn impossible_texture_is_rejected_once_before_decode() {
        let mut store = TileStore::new(8);
        let exact = key(1, 0);
        let oversized = key(1, 1);

        assert!(!store.reject_texture_preflight(exact, Ok(8)));
        assert_eq!(
            store.queue_for_demand(exact),
            TileDemandStatus::Queue(TileReadMode::Preferred)
        );
        assert!(store.reject_texture_preflight(oversized, Ok(9)));
        assert!(store.reject_texture_preflight(oversized, Ok(9)));

        assert_eq!(store.queue_for_demand(oversized), TileDemandStatus::Failed);
        assert_eq!(store.tile_failure().map(|failure| failure.count), Some(1));
    }

    #[test]
    fn malformed_decoded_dimensions_become_one_terminal_failure() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        store.queue(tile);
        let result = TileLoadResult {
            key: tile,
            outcome: TileLoadOutcome::Decoded(DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                width: u32::MAX,
                height: u32::MAX,
                rgba: Vec::new(),
            })),
            used_cpu_fallback: false,
        };

        store.stage_finished(1, &HashSet::from([tile]), result);
        store.stage_finished(1, &HashSet::from([tile]), decoded(tile, 1, 1));

        assert!(matches!(store.entries.get(&tile), Some(TileState::Failed)));
        assert_eq!(store.tile_failure().map(|failure| failure.count), Some(1));
    }

    #[test]
    fn uploading_reserves_source_plus_destination_and_cannot_be_evicted() {
        let mut store = TileStore::new(8);
        let tile = key(1, 0);
        store.queue(tile);
        store.stage_finished(1, &HashSet::from([tile]), decoded(tile, 1, 1));

        let batch = store.begin_upload_batch(&[tile]);

        assert_eq!(batch.keys, vec![tile]);
        assert_eq!(batch.tiles.len(), 1);
        assert_eq!(store.resident_bytes(), 8);
        assert!(matches!(
            store.entries.get(&tile),
            Some(TileState::Uploading { .. })
        ));
        store.evict_resident_tiles();
        assert!(matches!(
            store.entries.get(&tile),
            Some(TileState::Uploading { .. })
        ));
        assert_eq!(store.resident_bytes(), 8);
    }

    #[test]
    fn upload_reservations_never_exceed_the_ceiling() {
        let mut store = TileStore::new(8);
        let first = key(1, 0);
        let second = key(1, 1);
        let relevant = HashSet::from([first, second]);
        for tile in [first, second] {
            store.queue(tile);
            store.stage_finished(1, &relevant, decoded(tile, 1, 1));
        }

        let batch = store.begin_upload_batch(&[first, second]);

        assert_eq!(batch.tiles.len(), 1);
        assert!(store.resident_bytes() <= 8);
        assert_eq!(
            store
                .entries
                .values()
                .filter(|state| matches!(state, TileState::Uploading { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn missing_upload_outcome_releases_accounting_and_fails_the_tile() {
        let mut store = TileStore::new(8);
        let tile = key(1, 0);
        store.queue(tile);
        store.stage_finished(1, &HashSet::from([tile]), decoded(tile, 1, 1));

        let outcome = store.upload_pending_tiles(
            &mut MissingOutcomeUploader,
            &[tile],
            Duration::from_millis(1),
        );

        assert_eq!(outcome.failures, 1);
        assert!(matches!(store.entries.get(&tile), Some(TileState::Failed)));
        assert_eq!(store.resident_bytes(), 0);
        assert_eq!(store.loading_count(), 0);
    }

    #[test]
    fn surplus_upload_outcome_is_dropped_and_cannot_strand_accounting() {
        let mut store = TileStore::new(8);
        let tile = key(1, 0);
        store.queue(tile);
        store.stage_finished(1, &HashSet::from([tile]), decoded(tile, 1, 1));

        let outcome = store.upload_pending_tiles(
            &mut SurplusOutcomeUploader,
            &[tile],
            Duration::from_millis(1),
        );

        assert_eq!(outcome.failures, 1);
        assert!(matches!(
            store.entries.get(&tile),
            Some(TileState::Decoded { .. })
        ));
        assert_eq!(store.resident_bytes(), 4);
        assert_eq!(store.loading_count(), 1);
    }

    #[test]
    fn one_budgeted_sink_call_preserves_exact_deferred_decoded_state() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        store.queue(tile);
        store.stage_finished(1, &HashSet::from([tile]), decoded(tile, 1, 1));
        let original_last_used = match store.entries.get(&tile) {
            Some(TileState::Decoded { last_used, .. }) => *last_used,
            _ => panic!("test tile should be decoded before upload"),
        };
        let mut uploader = DeferringUploader::default();

        let outcome = store.upload_pending_tiles(&mut uploader, &[tile], Duration::from_millis(6));

        assert_eq!(uploader.calls, 1);
        assert_eq!(uploader.batch_sizes, vec![1]);
        assert_eq!(uploader.budgets, vec![Duration::from_millis(6)]);
        assert_eq!(outcome.uploaded, 0);
        assert_eq!(outcome.deferred, 1);
        assert!(matches!(
            store.entries.get(&tile),
            Some(TileState::Decoded {
                tile: DecodedTile::Cpu(dicom_viewer_core::RgbaTile { rgba, .. }),
                read_mode: TileReadMode::Preferred,
                byte_len: 4,
                last_used,
            }) if rgba == &vec![255; 4] && *last_used == original_last_used
        ));
    }

    #[test]
    fn empty_upload_plan_still_invokes_the_sink_once() {
        let mut store = TileStore::new(64);
        let mut uploader = DeferringUploader::default();

        let outcome = store.upload_pending_tiles(&mut uploader, &[], Duration::from_millis(1));

        assert_eq!(uploader.calls, 1);
        assert_eq!(uploader.batch_sizes, vec![0]);
        assert_eq!(outcome.uploaded, 0);
        assert_eq!(outcome.deferred, 0);
    }

    #[test]
    fn multiple_planned_tiles_are_sent_in_one_sink_call() {
        let mut store = TileStore::new(64);
        let first = key(1, 0);
        let second = key(1, 1);
        let relevant = HashSet::from([first, second]);
        for tile in [first, second] {
            store.queue(tile);
            store.stage_finished(1, &relevant, decoded(tile, 1, 1));
        }
        let mut uploader = DeferringUploader::default();

        let outcome =
            store.upload_pending_tiles(&mut uploader, &[first, second], Duration::from_millis(6));

        assert_eq!(uploader.calls, 1);
        assert_eq!(uploader.batch_sizes, vec![2]);
        assert_eq!(outcome.deferred, 2);
    }

    #[test]
    fn cancelled_tile_clears_pending_state_without_recording_a_failure() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        store.queue(tile);
        store.mark_decoding(&[tile], 1);

        store.stage_finished(
            1,
            &HashSet::from([tile]),
            TileLoadResult {
                key: tile,
                outcome: TileLoadOutcome::Cancelled,
                used_cpu_fallback: false,
            },
        );

        assert!(!store.entries.contains_key(&tile));
        assert!(store.tile_failure().is_none());
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
        let relevant = HashSet::from([first, second, third]);

        for tile in [first, second, third] {
            store.queue(tile);
            store.stage_finished(1, &relevant, decoded(tile, 1, 1));
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
    fn pruning_drops_inactive_pending_work_but_keeps_decoded_cache() {
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
        assert!(!store.entries.contains_key(&decoding));
        assert!(matches!(
            store.entries.get(&decoded_tile),
            Some(TileState::Decoded { .. })
        ));
    }

    #[test]
    fn pruning_inactive_decoding_allows_rapid_a_b_a_to_queue_replacement() {
        let mut store = TileStore::new(64);
        let a = key(1, 0);
        store.queue(a);
        store.mark_decoding(&[a], 1);

        store.retain_relevant_pending_tiles(&HashSet::new());

        assert!(matches!(store.queue(a), QueueStatus::New));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metal_upload_failure_schedules_exactly_one_cpu_retry() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        let mut first = UploadBatchOutcome {
            uploaded: 0,
            deferred: 0,
            cpu_retries: Vec::new(),
            failures: 0,
        };

        store.handle_upload_error(
            tile,
            TileReadMode::Preferred,
            TileUploadError::MetalTile("synthetic import failure".into()),
            &mut first,
        );

        assert_eq!(first.cpu_retries, vec![tile]);
        assert!(matches!(
            store.entries.get(&tile),
            Some(TileState::Queued {
                read_mode: TileReadMode::CpuFallback
            })
        ));
        assert_eq!(
            store.queue_for_demand(tile),
            TileDemandStatus::Queue(TileReadMode::CpuFallback),
            "the next atomic demand must publish the queued CPU fallback"
        );

        let mut second = UploadBatchOutcome {
            uploaded: 0,
            deferred: 0,
            cpu_retries: Vec::new(),
            failures: 0,
        };
        store.handle_upload_error(
            tile,
            TileReadMode::CpuFallback,
            TileUploadError::MetalTile("synthetic repeated failure".into()),
            &mut second,
        );

        assert!(second.cpu_retries.is_empty());
        assert!(matches!(store.entries.get(&tile), Some(TileState::Failed)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pruning_a_queued_cpu_retry_clears_its_retry_state() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        let mut first = UploadBatchOutcome {
            uploaded: 0,
            deferred: 0,
            cpu_retries: Vec::new(),
            failures: 0,
        };
        store.handle_upload_error(
            tile,
            TileReadMode::Preferred,
            TileUploadError::MetalTile("first import failure".into()),
            &mut first,
        );
        assert_eq!(first.cpu_retries, vec![tile]);

        store.retain_relevant_pending_tiles(&HashSet::new());
        assert!(!store.entries.contains_key(&tile));

        let mut revisited = UploadBatchOutcome {
            uploaded: 0,
            deferred: 0,
            cpu_retries: Vec::new(),
            failures: 0,
        };
        store.handle_upload_error(
            tile,
            TileReadMode::Preferred,
            TileUploadError::MetalTile("revisited import failure".into()),
            &mut revisited,
        );

        assert_eq!(revisited.cpu_retries, vec![tile]);
        assert!(matches!(
            store.entries.get(&tile),
            Some(TileState::Queued {
                read_mode: TileReadMode::CpuFallback
            })
        ));
    }

    #[test]
    fn failed_tile_stops_loading_but_remains_uncovered() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        store.queue(tile);
        store.stage_finished(
            1,
            &HashSet::from([tile]),
            TileLoadResult {
                key: tile,
                outcome: TileLoadOutcome::Failed(super::super::TileFailure::new(
                    "synthetic decode failure",
                )),
                used_cpu_fallback: false,
            },
        );
        let visible = [VisibleTile {
            key: tile,
            distance2: 0,
        }];

        assert_eq!(store.loading_count(), 0);
        assert_eq!(
            store.uncovered_tile_count(&visible, LevelIndex::from_u32(0)),
            1
        );
        assert_eq!(
            store.coverage(&visible, LevelIndex::from_u32(0)),
            TileCoverage {
                ready: 0,
                pending: 0,
                failed: 1,
                missing: 0,
            }
        );
        assert!(
            !store.has_pending_or_missing(&visible),
            "a terminal failed tile must not prevent idle background uploads"
        );
    }

    #[test]
    fn failed_target_draws_no_placeholder_over_the_coarser_fallback() {
        let mut store = TileStore::new(64);
        let tile = key(1, 0);
        store.queue(tile);
        store.stage_finished(
            1,
            &HashSet::from([tile]),
            TileLoadResult {
                key: tile,
                outcome: TileLoadOutcome::Failed(super::super::TileFailure::new(
                    "synthetic decode failure",
                )),
                used_cpu_fallback: false,
            },
        );
        assert!(store.ready_draw_info(tile).is_none());
    }
}
