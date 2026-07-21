use std::time::Instant;

use super::{
    distribution, safe_ratio, DicomIndexCounters, DicomIndexSamples, Distribution,
    EnabledPipelineStats, Interaction, LifetimeCounters, PIPELINE_SCHEMA_VERSION,
};

pub(super) fn pipeline_json(stats: &EnabledPipelineStats, now: Instant) -> String {
    let window = stats.window.snapshot(now);
    let queue_wait = distribution(&window.queue_wait_ms);
    let visible_queue_wait = distribution(&window.visible_queue_wait_ms);
    let source_read = distribution(&window.source_read_ms);
    let upload = distribution(&window.upload_ms);
    let app_ui_cpu = distribution(&window.app_ui_cpu_ms);
    let eviction = distribution(&window.eviction_ms);
    let level_preparation = distribution(&window.level_preparation_ms);
    let source_read_total_ms = window.source_read_ms.iter().sum::<f64>();
    let obsolete_source_read_ratio =
        safe_ratio(window.obsolete_source_read_ms, source_read_total_ms);
    let app_ui_cpu_total_ms = window.app_ui_cpu_ms.iter().sum::<f64>();
    let eviction_cpu_ratio = safe_ratio(window.eviction_ms.iter().sum(), app_ui_cpu_total_ms);
    format!(
        "{{\"schema_version\":{PIPELINE_SCHEMA_VERSION},\"kind\":\"pipeline_window\",\"elapsed_ms\":{:.3},\"queue_depth\":{{\"visible\":{},\"transition\":{},\"fallback\":{},\"overview\":{},\"prefetch\":{},\"total\":{}}},\"decoding\":{},\"batch_cardinality\":{{\"requested\":{},\"admitted\":{},\"returned\":{}}},\"visible_queue_wait_ms\":{},\"queue_wait_ms\":{},\"source_read_ms\":{},\"obsolete_source_read\":{{\"duration_ms\":{:.3},\"ratio\":{}}},\"upload_ms\":{},\"app_ui_cpu_ms\":{},\"eviction_ms\":{},\"eviction_cpu_ratio\":{},\"timing_scope\":\"app_ui_cpu_excludes_egui_tessellation_gpu_execution_and_presentation\",\"level_preparation_ms\":{},\"dicom_index\":{},\"lifetime\":{{\"requested_tiles\":{},\"admitted_tiles\":{},\"ready_cache_hits\":{},\"in_flight_deduplications\":{},\"enqueued\":{},\"returned_tiles\":{},\"cpu_results\":{},\"metal_results\":{},\"retries\":{},\"failures\":{},\"level_preparations\":{},\"level_preparation_failures\":{},\"cancellation_stage\":{{\"demand\":{},\"source\":{},\"preparation\":{}}},\"obsolete_discards\":{},\"uploads\":{},\"evictions\":{}}},\"resident\":{{\"bytes\":{},\"pinned_bytes\":{}}},\"gpu_submissions\":{}}}",
        now.duration_since(stats.started).as_secs_f64() * 1000.0,
        stats.gauges.visible,
        stats.gauges.transition,
        stats.gauges.fallback,
        stats.gauges.overview,
        stats.gauges.prefetch,
        stats.gauges.queued,
        stats.gauges.decoding,
        window.requested_tiles,
        window.admitted_tiles,
        window.returned_tiles,
        distribution_json(visible_queue_wait),
        distribution_json(queue_wait),
        distribution_json(source_read),
        window.obsolete_source_read_ms,
        optional_ratio_json(source_read_total_ms, obsolete_source_read_ratio),
        distribution_json(upload),
        distribution_json(app_ui_cpu),
        distribution_json(eviction),
        optional_ratio_json(app_ui_cpu_total_ms, eviction_cpu_ratio),
        distribution_json(level_preparation),
        dicom_index_json(&window.dicom_index, stats.lifetime.dicom_index, "lifetime"),
        stats.lifetime.requested_tiles,
        stats.lifetime.admitted_tiles,
        stats.lifetime.ready_cache_hits,
        stats.lifetime.in_flight_deduplications,
        stats.lifetime.enqueued,
        stats.lifetime.returned_tiles,
        stats.lifetime.cpu_results,
        stats.lifetime.metal_results,
        stats.lifetime.retries,
        stats.lifetime.failures,
        stats.lifetime.level_preparations,
        stats.lifetime.level_preparation_failures,
        stats.lifetime.demand_cancellations,
        stats.lifetime.source_cancellations,
        stats.lifetime.preparation_cancellations,
        stats.lifetime.obsolete_discards,
        stats.lifetime.uploads,
        stats.lifetime.evictions,
        stats.gauges.resident_bytes,
        stats.gauges.pinned_bytes,
        stats.gauges.submissions,
    )
}

pub(super) fn interaction_json(
    stats: &EnabledPipelineStats,
    interaction: Interaction,
    now: Instant,
) -> String {
    let delta = lifetime_delta(stats.lifetime, interaction.counters_at_start);
    let source_read_total_ms = interaction.samples.source_read_ms.iter().sum::<f64>();
    let obsolete_source_read_ratio = safe_ratio(
        interaction.samples.obsolete_source_read_ms,
        source_read_total_ms,
    );
    let app_ui_cpu_total_ms = interaction.samples.app_ui_cpu_ms.iter().sum::<f64>();
    let eviction_cpu_ratio = safe_ratio(
        interaction.samples.eviction_ms.iter().sum(),
        app_ui_cpu_total_ms,
    );
    let target_level = interaction
        .target_level
        .map_or_else(|| "null".to_string(), |level| level.get().to_string());
    format!(
        "{{\"schema_version\":{PIPELINE_SCHEMA_VERSION},\"kind\":\"interaction_summary\",\"elapsed_ms\":{:.3},\"duration_ms\":{:.3},\"first_sharp_target_ms\":{},\"first_sharp_origin\":\"continuous_gesture_start\",\"full_target_coverage_ms\":{},\"full_coverage_origin\":\"last_zoom_input\",\"target_coverage\":{{\"level\":{},\"ready\":{},\"pending\":{},\"failed\":{},\"missing\":{}}},\"batch_cardinality\":{{\"requested\":{},\"admitted\":{},\"returned\":{}}},\"visible_queue_wait_ms\":{},\"queue_wait_ms\":{},\"source_read_ms\":{},\"obsolete_source_read\":{{\"duration_ms\":{:.3},\"ratio\":{}}},\"upload_ms\":{},\"app_ui_cpu_ms\":{},\"eviction_ms\":{},\"eviction_cpu_ratio\":{},\"timing_scope\":\"app_ui_cpu_excludes_egui_tessellation_gpu_execution_and_presentation\",\"level_preparation_ms\":{},\"dicom_index\":{},\"ready_cache_hits\":{},\"in_flight_deduplications\":{},\"cpu_results\":{},\"metal_results\":{},\"retries\":{},\"failures\":{},\"level_preparations\":{},\"level_preparation_failures\":{},\"cancellation_stage\":{{\"demand\":{},\"source\":{},\"preparation\":{}}},\"obsolete_discards\":{},\"uploads\":{},\"evictions\":{},\"resident\":{{\"bytes\":{},\"pinned_bytes\":{}}}}}",
        now.duration_since(stats.started).as_secs_f64() * 1000.0,
        now.duration_since(interaction.started).as_secs_f64() * 1000.0,
        optional_milliseconds_json(interaction.first_sharp_target_ms),
        optional_milliseconds_json(interaction.full_target_coverage_ms),
        target_level,
        interaction.target_coverage.ready,
        interaction.target_coverage.pending,
        interaction.target_coverage.failed,
        interaction.target_coverage.missing,
        interaction.samples.requested_tiles,
        interaction.samples.admitted_tiles,
        interaction.samples.returned_tiles,
        distribution_json(distribution(
            &interaction.samples.visible_queue_wait_ms,
        )),
        distribution_json(distribution(&interaction.samples.queue_wait_ms)),
        distribution_json(distribution(&interaction.samples.source_read_ms)),
        interaction.samples.obsolete_source_read_ms,
        optional_ratio_json(source_read_total_ms, obsolete_source_read_ratio),
        distribution_json(distribution(&interaction.samples.upload_ms)),
        distribution_json(distribution(&interaction.samples.app_ui_cpu_ms)),
        distribution_json(distribution(&interaction.samples.eviction_ms)),
        optional_ratio_json(app_ui_cpu_total_ms, eviction_cpu_ratio),
        distribution_json(distribution(&interaction.samples.level_preparation_ms)),
        dicom_index_json(&interaction.samples.dicom_index, delta.dicom_index, "interaction"),
        delta.ready_cache_hits,
        delta.in_flight_deduplications,
        delta.cpu_results,
        delta.metal_results,
        delta.retries,
        delta.failures,
        delta.level_preparations,
        delta.level_preparation_failures,
        delta.demand_cancellations,
        delta.source_cancellations,
        delta.preparation_cancellations,
        delta.obsolete_discards,
        delta.uploads,
        delta.evictions,
        stats.gauges.resident_bytes,
        stats.gauges.pinned_bytes,
    )
}

fn lifetime_delta(current: LifetimeCounters, start: LifetimeCounters) -> LifetimeCounters {
    LifetimeCounters {
        requested_tiles: current
            .requested_tiles
            .saturating_sub(start.requested_tiles),
        admitted_tiles: current.admitted_tiles.saturating_sub(start.admitted_tiles),
        ready_cache_hits: current
            .ready_cache_hits
            .saturating_sub(start.ready_cache_hits),
        in_flight_deduplications: current
            .in_flight_deduplications
            .saturating_sub(start.in_flight_deduplications),
        enqueued: current.enqueued.saturating_sub(start.enqueued),
        returned_tiles: current.returned_tiles.saturating_sub(start.returned_tiles),
        cpu_results: current.cpu_results.saturating_sub(start.cpu_results),
        metal_results: current.metal_results.saturating_sub(start.metal_results),
        retries: current.retries.saturating_sub(start.retries),
        failures: current.failures.saturating_sub(start.failures),
        demand_cancellations: current
            .demand_cancellations
            .saturating_sub(start.demand_cancellations),
        source_cancellations: current
            .source_cancellations
            .saturating_sub(start.source_cancellations),
        preparation_cancellations: current
            .preparation_cancellations
            .saturating_sub(start.preparation_cancellations),
        level_preparations: current
            .level_preparations
            .saturating_sub(start.level_preparations),
        level_preparation_failures: current
            .level_preparation_failures
            .saturating_sub(start.level_preparation_failures),
        obsolete_discards: current
            .obsolete_discards
            .saturating_sub(start.obsolete_discards),
        uploads: current.uploads.saturating_sub(start.uploads),
        evictions: current.evictions.saturating_sub(start.evictions),
        dicom_index: current.dicom_index.delta(start.dicom_index),
    }
}

fn dicom_index_json(
    samples: &DicomIndexSamples,
    counters: DicomIndexCounters,
    counter_scope: &str,
) -> String {
    format!(
        "{{\"built_fast_ms\":{},\"fast_path_fallback_ms\":{},\"token_fallback_ms\":{},\"reused_ms\":{},\"{counter_scope}\":{{\"source\":{{\"preparation\":{},\"read\":{}}},\"built_fast\":{},\"fast_path_fallback\":{},\"token_fallback\":{},\"reused\":{},\"mapping\":{{\"extended_offset_table_direct\":{},\"extended_offset_table_items\":{},\"basic_offset_table_items\":{},\"single_frame_items\":{},\"one_fragment_per_frame\":{}}}}}}}",
        distribution_json(distribution(&samples.built_fast_ms)),
        distribution_json(distribution(&samples.fast_path_fallback_ms)),
        distribution_json(distribution(&samples.token_fallback_ms)),
        distribution_json(distribution(&samples.reused_ms)),
        counters.preparation_events,
        counters.read_events,
        counters.built_fast,
        counters.fast_path_fallback,
        counters.token_fallback,
        counters.reused,
        counters.extended_offset_table_direct,
        counters.extended_offset_table_items,
        counters.basic_offset_table_items,
        counters.single_frame_items,
        counters.one_fragment_per_frame,
    )
}

pub(super) fn distribution_json(distribution: Distribution) -> String {
    if distribution.count == 0 {
        return "{\"count\":0,\"p50\":null,\"p95\":null,\"p99\":null}".to_string();
    }
    format!(
        "{{\"count\":{},\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3}}}",
        distribution.count, distribution.p50, distribution.p95, distribution.p99
    )
}

fn optional_ratio_json(denominator: f64, ratio: f64) -> String {
    if denominator > 0.0 {
        format!("{ratio:.6}")
    } else {
        "null".to_string()
    }
}

fn optional_milliseconds_json(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| format!("{value:.3}"))
}
