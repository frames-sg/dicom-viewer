use super::decode::decode_tile_jobs;
use super::queue::{finish_batch, take_index_diagnostics, wait_for_batch};
use super::*;

pub(super) fn spawn_worker(
    worker_index: usize,
    shared: Arc<LoaderShared>,
    sender: SyncSender<TileLoaderMessage>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("dicom-viewer-tile-{worker_index}"))
        .spawn(move || run_worker(&shared, &sender))
}

fn run_worker(shared: &LoaderShared, sender: &SyncSender<TileLoaderMessage>) {
    loop {
        let Some(batch) = wait_for_batch(shared) else {
            return;
        };
        let keys = batch.jobs.iter().map(|job| job.key).collect::<Vec<_>>();
        if sender
            .send(TileLoaderMessage::Started {
                batch_id: batch.id,
                keys: keys.clone(),
            })
            .is_err()
        {
            finish_batch(shared, batch.id, &keys);
            return;
        }

        let queue_waits = batch
            .jobs
            .first()
            .and_then(|job| job.enqueued_at)
            .map_or_else(Vec::new, |_| {
                queue_wait_samples(&batch.jobs, Instant::now())
            });
        let source_read_started = batch
            .jobs
            .first()
            .and_then(|job| job.enqueued_at.map(|_| Instant::now()));
        let requested = batch.jobs.len();
        let decoded = decode_tile_jobs(batch.jobs, &batch.control);
        let source_read_ms =
            source_read_started.map(|started| started.elapsed().as_secs_f64() * 1000.0);
        let cpu_results = decoded
            .results
            .iter()
            .filter(
                |result| matches!(&result.outcome, TileLoadOutcome::Decoded(tile) if tile.is_cpu()),
            )
            .count();
        let metal_results = decoded
            .results
            .iter()
            .filter(|result| matches!(&result.outcome, TileLoadOutcome::Decoded(tile) if !tile.is_cpu()))
            .count();
        let failures = decoded
            .results
            .iter()
            .filter(|result| matches!(&result.outcome, TileLoadOutcome::Failed(_)))
            .count();
        let metrics = TileBatchMetrics {
            queue_waits,
            source_read_ms,
            requested,
            admitted: decoded.admitted,
            returned: decoded.returned,
            cpu_results,
            metal_results,
            retries: decoded.retries,
            failures,
            source_cancellations: decoded.source_cancellations,
        };
        let index_diagnostics = take_index_diagnostics(&batch.index_diagnostics);
        if sender
            .send(TileLoaderMessage::Finished {
                batch_id: batch.id,
                results: decoded.results,
                metrics,
                index_diagnostics,
            })
            .is_err()
        {
            finish_batch(shared, batch.id, &keys);
            return;
        }
    }
}

pub(super) fn queue_wait_samples(jobs: &[TileJob], observed_at: Instant) -> Vec<TileQueueWait> {
    jobs.iter()
        .filter_map(|job| {
            job.enqueued_at.map(|enqueued_at| TileQueueWait {
                lane: job.priority.lane,
                milliseconds: observed_at
                    .saturating_duration_since(enqueued_at)
                    .as_secs_f64()
                    * 1_000.0,
            })
        })
        .collect()
}
