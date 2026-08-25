use super::*;

pub(super) trait TileSource: Send + Sync {
    fn read_tiles(
        &self,
        requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
        read_mode: TileReadMode,
        control: &ReadControl,
    ) -> std::result::Result<Vec<DecodedTile>, TileSourceError>;

    fn device_was_preferred(&self, read_mode: TileReadMode) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TileSourceError {
    Cancelled,
    CudaDownload(String),
    Failed(String),
}

impl TileSourceError {
    fn from_viewer(error: dicom_viewer_core::ViewerError) -> Self {
        if error.is_cuda_download_failure() {
            Self::CudaDownload(error.to_string())
        } else if error.is_cancelled() {
            Self::Cancelled
        } else {
            Self::Failed(error.to_string())
        }
    }
}

impl TileSource for ViewerStudy {
    fn read_tiles(
        &self,
        requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
        read_mode: TileReadMode,
        control: &ReadControl,
    ) -> std::result::Result<Vec<DecodedTile>, TileSourceError> {
        match read_mode {
            TileReadMode::Preferred => self
                .read_tiles_for_render_controlled(requests, control)
                .map_err(TileSourceError::from_viewer)?
                .into_iter()
                .map(|tile| DecodedTile::from_render_tile(tile).map_err(TileSourceError::Failed))
                .collect(),
            TileReadMode::CpuFallback => self
                .read_tiles_rgba_controlled(requests, control)
                .map(|tiles| tiles.into_iter().map(DecodedTile::from_rgba_tile).collect())
                .map_err(TileSourceError::from_viewer),
        }
    }

    fn device_was_preferred(&self, read_mode: TileReadMode) -> bool {
        read_mode == TileReadMode::Preferred
            && self.summary().tile_decode_backend != dicom_viewer_core::TileDecodeBackend::Cpu
    }
}

pub(super) fn decode_tile_jobs(jobs: Vec<TileJob>, control: &ReadControl) -> TileDecodeBatch {
    let keys = jobs.iter().map(|job| job.key).collect::<Vec<_>>();
    let requests = jobs
        .iter()
        .map(|job| (job.key.level, job.key.coord))
        .collect::<Vec<_>>();
    let read_mode = jobs[0].read_mode;
    decode_source_requests(jobs[0].study.as_ref(), keys, &requests, read_mode, control)
}

pub(super) struct TileDecodeBatch {
    pub(super) results: Vec<TileLoadResult>,
    pub(super) admitted: usize,
    pub(super) returned: usize,
    pub(super) retries: usize,
    pub(super) source_cancellations: usize,
}

pub(super) fn decode_source_requests(
    source: &dyn TileSource,
    keys: Vec<TileKey>,
    requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
    read_mode: TileReadMode,
    control: &ReadControl,
) -> TileDecodeBatch {
    if control.cancellation().is_cancelled() {
        return TileDecodeBatch {
            results: cancelled_tiles(keys),
            admitted: 0,
            returned: 0,
            retries: 0,
            source_cancellations: 0,
        };
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        source.read_tiles(requests, read_mode, control)
    }));
    let returned = match &outcome {
        Ok(Ok(tiles)) => tiles.len(),
        Ok(Err(_)) | Err(_) => 0,
    };
    let mut retries = 0;
    let mut source_cancellations = 0;
    let mut results = if control.cancellation().is_cancelled() {
        source_cancellations = 1;
        cancelled_tiles(keys)
    } else {
        match outcome {
            Ok(Ok(tiles)) if tiles.len() == requests.len() => successful_tiles(keys, tiles),
            Ok(Ok(tiles)) if requests.len() > 1 => {
                let recovery = retry_tiles_individually(
                    source,
                    keys,
                    requests,
                    read_mode,
                    control,
                    format!(
                        "tile batch returned {} tiles for {} requests",
                        tiles.len(),
                        requests.len()
                    ),
                );
                retries = recovery.retries;
                source_cancellations = recovery.source_cancellations;
                recovery.results
            }
            Ok(Err(TileSourceError::Cancelled)) => {
                source_cancellations = 1;
                cancelled_tiles(keys)
            }
            Ok(Err(TileSourceError::CudaDownload(error))) => {
                let recovery = retry_tiles_individually(
                    source,
                    keys,
                    requests,
                    TileReadMode::CpuFallback,
                    control,
                    error,
                );
                retries = recovery.retries;
                source_cancellations = recovery.source_cancellations;
                recovery.results
            }
            Ok(Err(TileSourceError::Failed(error)))
                if requests.len() > 1 && !control.cancellation().is_cancelled() =>
            {
                let recovery =
                    retry_tiles_individually(source, keys, requests, read_mode, control, error);
                retries = recovery.retries;
                source_cancellations = recovery.source_cancellations;
                recovery.results
            }
            Ok(Ok(tiles)) => batch_failures(
                keys,
                format!(
                    "tile read returned {} tiles for {} requests",
                    tiles.len(),
                    requests.len()
                ),
            ),
            Ok(Err(TileSourceError::Failed(error))) => {
                batch_failures(keys, format!("tile read failed: {error}"))
            }
            Err(_) => batch_failures(keys, "tile decoder panicked".into()),
        }
    };
    if source.device_was_preferred(read_mode) {
        for result in &mut results {
            result.used_cpu_fallback = match &result.outcome {
                TileLoadOutcome::Decoded(tile) => tile.is_cpu(),
                TileLoadOutcome::Cancelled | TileLoadOutcome::Failed(_) => false,
            };
        }
    }
    TileDecodeBatch {
        results,
        admitted: requests.len(),
        returned,
        retries,
        source_cancellations,
    }
}

struct TileRecovery {
    results: Vec<TileLoadResult>,
    retries: usize,
    source_cancellations: usize,
}

fn retry_tiles_individually(
    source: &dyn TileSource,
    keys: Vec<TileKey>,
    requests: &[(dicom_viewer_core::LevelIndex, dicom_viewer_core::TileCoord)],
    read_mode: TileReadMode,
    control: &ReadControl,
    batch_error: String,
) -> TileRecovery {
    let mut cancellation_seen = false;
    let mut retries = 0;
    let mut source_cancellations = 0;
    let results = keys
        .into_iter()
        .zip(requests)
        .map(|(key, request)| {
            if cancellation_seen || control.cancellation().is_cancelled() {
                return TileLoadResult {
                    key,
                    outcome: TileLoadOutcome::Cancelled,
                    used_cpu_fallback: false,
                };
            }
            retries += 1;
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                source.read_tiles(std::slice::from_ref(request), read_mode, control)
            }));
            if control.cancellation().is_cancelled() {
                cancellation_seen = true;
                source_cancellations += 1;
                return TileLoadResult {
                    key,
                    outcome: TileLoadOutcome::Cancelled,
                    used_cpu_fallback: false,
                };
            }
            let result = match outcome {
                Ok(Ok(mut tiles)) if tiles.len() == 1 => Ok(tiles.remove(0)),
                Ok(Ok(tiles)) => Err(format!(
                    "tile batch recovery after {batch_error} returned {} tiles for one request",
                    tiles.len()
                )),
                Ok(Err(TileSourceError::Cancelled)) => {
                    cancellation_seen = true;
                    source_cancellations += 1;
                    return TileLoadResult {
                        key,
                        outcome: TileLoadOutcome::Cancelled,
                        used_cpu_fallback: false,
                    };
                }
                Ok(Err(TileSourceError::CudaDownload(error))) => Err(format!(
                    "tile batch recovery after {batch_error} hit another CUDA download failure: {error}"
                )),
                Ok(Err(TileSourceError::Failed(error))) => Err(format!(
                    "tile batch recovery after {batch_error} failed: {error}"
                )),
                Err(_) => Err(format!(
                    "tile decoder panicked during recovery after {batch_error}"
                )),
            };
            TileLoadResult {
                key,
                outcome: match result {
                    Ok(tile) => TileLoadOutcome::Decoded(tile),
                    Err(error) => TileLoadOutcome::Failed(TileFailure::new(error)),
                },
                used_cpu_fallback: false,
            }
        })
        .collect();
    TileRecovery {
        results,
        retries,
        source_cancellations,
    }
}

fn successful_tiles(keys: Vec<TileKey>, tiles: Vec<DecodedTile>) -> Vec<TileLoadResult> {
    keys.into_iter()
        .zip(tiles)
        .map(|(key, tile)| TileLoadResult {
            key,
            outcome: TileLoadOutcome::Decoded(tile),
            used_cpu_fallback: false,
        })
        .collect()
}

fn batch_failures(keys: Vec<TileKey>, error: String) -> Vec<TileLoadResult> {
    keys.into_iter()
        .map(|key| TileLoadResult {
            key,
            outcome: TileLoadOutcome::Failed(TileFailure::new(error.clone())),
            used_cpu_fallback: false,
        })
        .collect()
}

fn cancelled_tiles(keys: Vec<TileKey>) -> Vec<TileLoadResult> {
    keys.into_iter()
        .map(|key| TileLoadResult {
            key,
            outcome: TileLoadOutcome::Cancelled,
            used_cpu_fallback: false,
        })
        .collect()
}
