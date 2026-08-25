use super::*;

struct CudaDownloadRetrySource {
    calls: Mutex<Vec<(TileReadMode, Vec<u64>)>>,
}

impl TileSource for CudaDownloadRetrySource {
    fn read_tiles(
        &self,
        requests: &[(LevelIndex, TileCoord)],
        read_mode: TileReadMode,
        _control: &ReadControl,
    ) -> std::result::Result<Vec<DecodedTile>, TileSourceError> {
        self.calls.lock().unwrap().push((
            read_mode,
            requests.iter().map(|(_, coord)| coord.col()).collect(),
        ));
        match read_mode {
            TileReadMode::Preferred => Err(TileSourceError::CudaDownload(
                "synthetic CUDA download failure".into(),
            )),
            TileReadMode::CpuFallback => Ok(requests
                .iter()
                .map(|(_, coord)| {
                    DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                        width: 1,
                        height: 1,
                        rgba: vec![coord.col() as u8, 0, 0, 255],
                    })
                })
                .collect()),
        }
    }

    fn device_was_preferred(&self, read_mode: TileReadMode) -> bool {
        read_mode == TileReadMode::Preferred
    }
}

#[test]
fn cuda_download_failure_gets_exactly_one_ordered_cpu_retry_per_tile() {
    let source = CudaDownloadRetrySource {
        calls: Mutex::new(Vec::new()),
    };
    let keys = vec![key(0), key(1)];
    let requests = vec![
        (LevelIndex::from_u32(0), TileCoord::new(0, 0)),
        (LevelIndex::from_u32(0), TileCoord::new(1, 0)),
    ];

    let decoded = decode_source_requests(
        &source,
        keys.clone(),
        &requests,
        TileReadMode::Preferred,
        &ReadControl::default(),
    );

    assert_eq!(decoded.retries, 2);
    assert_eq!(
        decoded
            .results
            .iter()
            .map(|result| result.key)
            .collect::<Vec<_>>(),
        keys
    );
    assert!(decoded.results.iter().all(|result| {
        result.used_cpu_fallback
            && matches!(
                result.outcome,
                TileLoadOutcome::Decoded(DecodedTile::Cpu(_))
            )
    }));
    assert_eq!(
        *source.calls.lock().unwrap(),
        vec![
            (TileReadMode::Preferred, vec![0, 1]),
            (TileReadMode::CpuFallback, vec![0]),
            (TileReadMode::CpuFallback, vec![1]),
        ]
    );
}

struct BlockingTileSource {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl TileSource for BlockingTileSource {
    fn read_tiles(
        &self,
        requests: &[(LevelIndex, TileCoord)],
        _read_mode: TileReadMode,
        _control: &ReadControl,
    ) -> std::result::Result<Vec<DecodedTile>, TileSourceError> {
        self.entered.wait();
        self.release.wait();
        Ok(requests
            .iter()
            .map(|_| {
                DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 255],
                })
            })
            .collect())
    }

    fn device_was_preferred(&self, _read_mode: TileReadMode) -> bool {
        false
    }
}

#[test]
fn cancellation_during_a_source_read_returns_only_silent_cancelled_outcomes() {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let source = Arc::new(BlockingTileSource {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let token = ReadCancellationToken::default();
    let control = ReadControl::new(token.clone());
    let thread = std::thread::spawn(move || {
        let requests = vec![(LevelIndex::from_u32(0), TileCoord::new(0, 0))];
        decode_source_requests(
            source.as_ref(),
            vec![key(0)],
            &requests,
            TileReadMode::Preferred,
            &control,
        )
    });
    entered.wait();
    token.cancel();
    release.wait();

    let results = thread.join().unwrap().results;
    assert!(results
        .iter()
        .all(|result| matches!(&result.outcome, TileLoadOutcome::Cancelled)));
}

#[derive(Clone, Copy)]
enum FakeReadBehavior {
    FailTile(u64),
    WrongFirstCardinality,
    Cancel,
    Panic,
}

struct FakeTileSource {
    behavior: FakeReadBehavior,
    calls: Mutex<Vec<Vec<TileCoord>>>,
}

impl FakeTileSource {
    fn new(behavior: FakeReadBehavior) -> Self {
        Self {
            behavior,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl TileSource for FakeTileSource {
    fn read_tiles(
        &self,
        requests: &[(LevelIndex, TileCoord)],
        _read_mode: TileReadMode,
        _control: &ReadControl,
    ) -> std::result::Result<Vec<DecodedTile>, TileSourceError> {
        let mut calls = self.calls.lock().unwrap();
        calls.push(requests.iter().map(|(_, coord)| *coord).collect());
        let call_number = calls.len();
        drop(calls);
        match self.behavior {
            FakeReadBehavior::Panic => panic!("synthetic decoder panic"),
            FakeReadBehavior::Cancel => Err(TileSourceError::Cancelled),
            FakeReadBehavior::WrongFirstCardinality if call_number == 1 && requests.len() > 1 => {
                Ok(Vec::new())
            }
            FakeReadBehavior::FailTile(col)
                if requests.iter().any(|(_, coord)| coord.col() == col) =>
            {
                Err(TileSourceError::Failed(format!("corrupt tile {col}")))
            }
            _ => Ok(requests
                .iter()
                .map(|_| {
                    DecodedTile::Cpu(dicom_viewer_core::RgbaTile {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 0, 255],
                    })
                })
                .collect()),
        }
    }

    fn device_was_preferred(&self, _read_mode: TileReadMode) -> bool {
        false
    }
}

fn decode_with_fake(source: &dyn TileSource, columns: std::ops::Range<u64>) -> Vec<TileLoadResult> {
    let keys = columns.clone().map(key).collect::<Vec<_>>();
    let requests = columns
        .map(|col| (LevelIndex::from_u32(0), TileCoord::new(col, 0)))
        .collect::<Vec<_>>();
    decode_source_requests(
        source,
        keys,
        &requests,
        TileReadMode::Preferred,
        &ReadControl::default(),
    )
    .results
}

#[test]
fn failed_batch_retries_each_tile_once_and_preserves_seven_valid_tiles() {
    let source = FakeTileSource::new(FakeReadBehavior::FailTile(3));

    let results = decode_with_fake(&source, 0..8);

    assert_eq!(source.call_count(), 9);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(&result.outcome, TileLoadOutcome::Decoded(_)))
            .count(),
        7
    );
    assert!(matches!(&results[3].outcome, TileLoadOutcome::Failed(_)));
}

#[test]
fn wrong_batch_cardinality_isolated_by_single_tile_retries() {
    let source = FakeTileSource::new(FakeReadBehavior::WrongFirstCardinality);

    let results = decode_with_fake(&source, 0..3);

    assert_eq!(source.call_count(), 4);
    assert!(results
        .iter()
        .all(|result| matches!(&result.outcome, TileLoadOutcome::Decoded(_))));
}

#[test]
fn decode_report_preserves_initial_cardinality_and_retry_counts() {
    let source = FakeTileSource::new(FakeReadBehavior::WrongFirstCardinality);
    let keys = (0..3).map(key).collect::<Vec<_>>();
    let requests = (0..3)
        .map(|col| (LevelIndex::from_u32(0), TileCoord::new(col, 0)))
        .collect::<Vec<_>>();

    let report = decode_source_requests(
        &source,
        keys,
        &requests,
        TileReadMode::Preferred,
        &ReadControl::default(),
    );

    assert_eq!(report.admitted, 3);
    assert_eq!(report.returned, 0);
    assert_eq!(report.retries, 3);
    assert_eq!(report.source_cancellations, 0);
    assert_eq!(report.results.len(), 3);
}

#[test]
fn single_tile_failure_is_not_retried() {
    let source = FakeTileSource::new(FakeReadBehavior::FailTile(0));

    let results = decode_with_fake(&source, 0..1);

    assert_eq!(source.call_count(), 1);
    assert!(matches!(&results[0].outcome, TileLoadOutcome::Failed(_)));
}

#[test]
fn typed_source_cancellation_is_never_retried_or_recorded_as_failure() {
    let source = FakeTileSource::new(FakeReadBehavior::Cancel);

    let results = decode_with_fake(&source, 0..8);

    assert_eq!(source.call_count(), 1);
    assert!(results
        .iter()
        .all(|result| matches!(&result.outcome, TileLoadOutcome::Cancelled)));
}

#[test]
fn decoder_panic_does_not_create_a_retry_storm() {
    let source = FakeTileSource::new(FakeReadBehavior::Panic);

    let results = decode_with_fake(&source, 0..8);

    assert_eq!(source.call_count(), 1);
    assert!(results.iter().all(|result| {
            matches!(&result.outcome, TileLoadOutcome::Failed(failure) if failure.message.contains("panicked"))
        }));
}
