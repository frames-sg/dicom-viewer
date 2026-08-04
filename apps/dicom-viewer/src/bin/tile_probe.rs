#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dicom_viewer_core::{
    LevelIndex, ReadControl, RenderTile, TileCoord, ViewerOpenOptions, ViewerStudy,
};

const DEFAULT_TRIALS: usize = 3;
const DEFAULT_BATCH_SIZE: usize = 8;
const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeApi {
    ControlledRender,
    ControlledRgba,
    UncontrolledRgba,
}

impl ProbeApi {
    const fn label(self) -> &'static str {
        match self {
            Self::ControlledRender => "controlled-render",
            Self::ControlledRgba => "controlled-rgba",
            Self::UncontrolledRgba => "uncontrolled-rgba",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "controlled-render" => Ok(Self::ControlledRender),
            "controlled-rgba" => Ok(Self::ControlledRgba),
            "uncontrolled-rgba" => Ok(Self::UncontrolledRgba),
            _ => {
                Err("--api must be controlled-render, controlled-rgba, or uncontrolled-rgba".into())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestedBackend {
    Auto,
    Cpu,
}

impl RequestedBackend {
    const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            _ => Err("--backend must be auto or cpu".into()),
        }
    }
}

#[derive(Debug)]
struct Arguments {
    path: PathBuf,
    trials: usize,
    json: bool,
    api: ProbeApi,
    backend: RequestedBackend,
    batch_size: usize,
}

#[derive(Debug)]
struct LevelRecord {
    index: LevelIndex,
    width: u64,
    height: u64,
    downsample: f64,
    cols: u64,
    rows: u64,
}

#[derive(Debug)]
struct TrialRecord {
    trial: usize,
    level: LevelIndex,
    tiles: usize,
    open_ms: f64,
    prepare_ms: f64,
    first_batch_ms: f64,
    warm_batch_ms: f64,
    cpu_outputs: usize,
    metal_outputs: usize,
}

#[derive(Debug, Clone, Copy)]
struct Distribution {
    min: f64,
    p50: f64,
    p95: f64,
    max: f64,
}

#[derive(Debug)]
struct ProbeReport {
    path: PathBuf,
    format: String,
    requested_backend: RequestedBackend,
    resolved_backend: String,
    api: ProbeApi,
    batch_size: usize,
    initial_open_ms: f64,
    levels: Vec<LevelRecord>,
    trials: Vec<TrialRecord>,
}

fn main() {
    let arguments = parse_arguments(std::env::args_os().skip(1)).unwrap_or_else(|error| {
        eprintln!(
            "{error}\nusage: tile_probe [--json] [--trials N] [--api controlled-render|controlled-rgba|uncontrolled-rgba] [--backend auto|cpu] [--batch-size 1|2|4|8|16] <wsi-file-or-dicom-folder>"
        );
        std::process::exit(2);
    });
    let report = run_probe(&arguments).unwrap_or_else(|error| {
        eprintln!("probe failed: {error}");
        std::process::exit(1);
    });
    if arguments.json {
        println!("{}", report.to_json());
    } else {
        report.print_human();
    }
}

fn parse_arguments(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Arguments, String> {
    let mut path = None;
    let mut trials = DEFAULT_TRIALS;
    let mut json = false;
    let mut api = ProbeApi::ControlledRender;
    let mut backend = RequestedBackend::Auto;
    let mut batch_size = DEFAULT_BATCH_SIZE;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--json") => json = true,
            Some("--trials") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--trials requires a positive integer".to_string())?;
                trials = value
                    .to_str()
                    .ok_or_else(|| "--trials must be valid UTF-8".to_string())?
                    .parse::<usize>()
                    .map_err(|_| "--trials requires a positive integer".to_string())?;
                if trials == 0 {
                    return Err("--trials requires a positive integer".into());
                }
            }
            Some("--api") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--api requires a value".to_string())?;
                api = ProbeApi::parse(
                    value
                        .to_str()
                        .ok_or_else(|| "--api must be valid UTF-8".to_string())?,
                )?;
            }
            Some("--backend") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--backend requires a value".to_string())?;
                backend = RequestedBackend::parse(
                    value
                        .to_str()
                        .ok_or_else(|| "--backend must be valid UTF-8".to_string())?,
                )?;
            }
            Some("--batch-size") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--batch-size requires a value".to_string())?;
                batch_size = value
                    .to_str()
                    .ok_or_else(|| "--batch-size must be valid UTF-8".to_string())?
                    .parse::<usize>()
                    .map_err(|_| "--batch-size must be 1, 2, 4, 8, or 16".to_string())?;
                if !matches!(batch_size, 1 | 2 | 4 | 8 | 16) {
                    return Err("--batch-size must be 1, 2, 4, 8, or 16".into());
                }
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option: {value}"));
            }
            _ if path.is_none() => path = Some(PathBuf::from(argument)),
            _ => return Err("only one input path may be supplied".into()),
        }
    }
    Ok(Arguments {
        path: path.ok_or_else(|| "an input path is required".to_string())?,
        trials,
        json,
        api,
        backend,
        batch_size,
    })
}

fn run_probe(arguments: &Arguments) -> Result<ProbeReport, String> {
    let opened = Instant::now();
    let study = open_study(&arguments.path, arguments.backend)?;
    let initial_open_ms = elapsed_ms(opened);
    let summary = study.summary();
    let format = summary.format_label.clone();
    let resolved_backend = summary.tile_decode_backend.to_string();
    let levels = summary
        .levels
        .iter()
        .filter_map(|level| {
            level
                .tile_layout
                .grid_size()
                .map(|(cols, rows)| LevelRecord {
                    index: level.index,
                    width: level.width,
                    height: level.height,
                    downsample: level.downsample,
                    cols,
                    rows,
                })
        })
        .collect::<Vec<_>>();

    drop(study);

    let mut trials = Vec::new();
    for trial in 0..arguments.trials {
        for level in &levels {
            let block = centered_block(level.cols, level.rows, arguments.batch_size, trial);
            trials.push(run_trial(
                &arguments.path,
                trial + 1,
                level.index,
                &block,
                arguments.api,
                arguments.backend,
            )?);
        }
    }

    Ok(ProbeReport {
        path: arguments.path.clone(),
        format,
        requested_backend: arguments.backend,
        resolved_backend,
        api: arguments.api,
        batch_size: arguments.batch_size,
        initial_open_ms,
        levels,
        trials,
    })
}

fn run_trial(
    path: &Path,
    trial: usize,
    level: LevelIndex,
    block: &[TileCoord],
    api: ProbeApi,
    backend: RequestedBackend,
) -> Result<TrialRecord, String> {
    let opened = Instant::now();
    let study = open_study(path, backend)?;
    let open_ms = elapsed_ms(opened);
    let control = ReadControl::default();
    let preparing = Instant::now();
    study
        .prepare_level_controlled(level, &control)
        .map_err(|error| format!("level {level} preparation failed: {error}"))?;
    let prepare_ms = elapsed_ms(preparing);

    let started = Instant::now();
    let first_outputs = read_block(&study, level, block, api, &control)?;
    let first_batch_ms = elapsed_ms(started);

    let started = Instant::now();
    let warm_outputs = read_block(&study, level, block, api, &control)?;
    let warm_batch_ms = elapsed_ms(started);
    if first_outputs != warm_outputs {
        return Err(format!(
            "level {level} output residency changed between identical reads"
        ));
    }

    Ok(TrialRecord {
        trial,
        level,
        tiles: block.len(),
        open_ms,
        prepare_ms,
        first_batch_ms,
        warm_batch_ms,
        cpu_outputs: first_outputs.cpu,
        metal_outputs: first_outputs.metal,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct OutputCounts {
    cpu: usize,
    metal: usize,
}

fn read_block(
    study: &ViewerStudy,
    level: LevelIndex,
    block: &[TileCoord],
    api: ProbeApi,
    control: &ReadControl,
) -> Result<OutputCounts, String> {
    let requests = block
        .iter()
        .map(|&coord| (level, coord))
        .collect::<Vec<_>>();
    match api {
        ProbeApi::ControlledRender => {
            let tiles = study
                .read_tiles_for_render_controlled(&requests, control)
                .map_err(|error| {
                    format!("controlled render read failed at level {level}: {error}")
                })?;
            count_render_outputs(level, requests.len(), tiles)
        }
        ProbeApi::ControlledRgba => {
            let tiles = study
                .read_tiles_rgba_controlled(&requests, control)
                .map_err(|error| {
                    format!("controlled RGBA read failed at level {level}: {error}")
                })?;
            expect_count(level, requests.len(), tiles.len())?;
            Ok(OutputCounts {
                cpu: tiles.len(),
                metal: 0,
            })
        }
        ProbeApi::UncontrolledRgba => {
            let tiles = study.read_tiles_rgba(&requests).map_err(|error| {
                format!("uncontrolled RGBA read failed at level {level}: {error}")
            })?;
            expect_count(level, requests.len(), tiles.len())?;
            Ok(OutputCounts {
                cpu: tiles.len(),
                metal: 0,
            })
        }
    }
}

fn count_render_outputs(
    level: LevelIndex,
    expected: usize,
    tiles: Vec<RenderTile>,
) -> Result<OutputCounts, String> {
    expect_count(level, expected, tiles.len())?;
    let mut counts = OutputCounts::default();
    for tile in tiles {
        match tile {
            RenderTile::Cpu(_) => counts.cpu += 1,
            #[cfg(target_os = "macos")]
            RenderTile::Metal(_) => counts.metal += 1,
            #[allow(unreachable_patterns)]
            _ => return Err("renderer returned an unsupported output type".into()),
        }
    }
    Ok(counts)
}

fn expect_count(level: LevelIndex, expected: usize, actual: usize) -> Result<(), String> {
    if expected == actual {
        Ok(())
    } else {
        Err(format!(
            "batch at level {level} returned {actual} tiles for {expected} requests"
        ))
    }
}

fn open_study(path: &Path, backend: RequestedBackend) -> Result<ViewerStudy, String> {
    let options = viewer_options(backend);
    ViewerStudy::open_path_with_options(path, options).map_err(|error| error.to_string())
}

fn viewer_options(backend: RequestedBackend) -> ViewerOpenOptions {
    match backend {
        RequestedBackend::Cpu => ViewerOpenOptions::cpu_only(),
        RequestedBackend::Auto => {
            let options = ViewerOpenOptions::auto();
            #[cfg(target_os = "macos")]
            if let Some(device) = metal::Device::system_default() {
                return options.with_metal_device(device);
            }
            options
        }
    }
}

fn centered_block(cols: u64, rows: u64, requested: usize, trial: usize) -> Vec<TileCoord> {
    if cols == 0 || rows == 0 || requested == 0 {
        return Vec::new();
    }
    let shifts = [(0_i64, 0_i64), (-1, 0), (1, 0), (0, -1), (0, 1)];
    let (shift_col, shift_row) = shifts[trial % shifts.len()];
    let center_col = (cols / 2)
        .saturating_add_signed(shift_col)
        .min(cols.saturating_sub(1));
    let center_row = (rows / 2)
        .saturating_add_signed(shift_row)
        .min(rows.saturating_sub(1));
    let target = requested.min(usize::try_from(cols.saturating_mul(rows)).unwrap_or(usize::MAX));
    let mut radius = 0_u64;
    loop {
        let start_col = center_col.saturating_sub(radius);
        let end_col = center_col.saturating_add(radius).min(cols - 1);
        let start_row = center_row.saturating_sub(radius);
        let end_row = center_row.saturating_add(radius).min(rows - 1);
        let available = end_col
            .saturating_sub(start_col)
            .saturating_add(1)
            .saturating_mul(end_row.saturating_sub(start_row).saturating_add(1));
        if available >= target as u64
            || (start_col == 0 && end_col + 1 == cols && start_row == 0 && end_row + 1 == rows)
        {
            let mut tiles = (start_row..=end_row)
                .flat_map(|row| (start_col..=end_col).map(move |col| TileCoord::new(col, row)))
                .collect::<Vec<_>>();
            tiles.sort_by_key(|coord| {
                (
                    u128::from(coord.col().abs_diff(center_col)).pow(2)
                        + u128::from(coord.row().abs_diff(center_row)).pow(2),
                    coord.row(),
                    coord.col(),
                )
            });
            tiles.truncate(target);
            return tiles;
        }
        radius = radius.saturating_add(1);
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn distribution(values: impl IntoIterator<Item = f64>) -> Distribution {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return Distribution {
            min: 0.0,
            p50: 0.0,
            p95: 0.0,
            max: 0.0,
        };
    }
    Distribution {
        min: percentile(&values, 0.0),
        p50: percentile(&values, 0.5),
        p95: percentile(&values, 0.95),
        max: percentile(&values, 1.0),
    }
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (quantile.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

impl ProbeReport {
    fn print_human(&self) {
        println!("path={}", self.path.display());
        println!("open_ms={:.3}", self.initial_open_ms);
        println!(
            "schema={} format={} api={} requested_backend={} resolved_backend={} batch_size={}",
            REPORT_SCHEMA_VERSION,
            self.format,
            self.api.label(),
            self.requested_backend.label(),
            self.resolved_backend,
            self.batch_size,
        );
        for level in &self.levels {
            println!(
                "level={} size={}x{} downsample={:.3} grid={}x{}",
                level.index, level.width, level.height, level.downsample, level.cols, level.rows
            );
        }
        for trial in &self.trials {
            println!(
                "trial={} level={} tiles={} open_ms={:.3} prepare_ms={:.3} first_batch_ms={:.3} warm_batch_ms={:.3} cpu={} metal={}",
                trial.trial,
                trial.level,
                trial.tiles,
                trial.open_ms,
                trial.prepare_ms,
                trial.first_batch_ms,
                trial.warm_batch_ms,
                trial.cpu_outputs,
                trial.metal_outputs,
            );
        }
        for (name, values) in [
            (
                "open",
                distribution(self.trials.iter().map(|trial| trial.open_ms)),
            ),
            (
                "prepare",
                distribution(self.trials.iter().map(|trial| trial.prepare_ms)),
            ),
            (
                "first_batch",
                distribution(self.trials.iter().map(|trial| trial.first_batch_ms)),
            ),
            (
                "warm_batch",
                distribution(self.trials.iter().map(|trial| trial.warm_batch_ms)),
            ),
        ] {
            println!(
                "stats={name} min={:.3} p50={:.3} p95={:.3} max={:.3}",
                values.min, values.p50, values.p95, values.max
            );
        }
    }

    fn to_json(&self) -> String {
        let mut output = String::new();
        write!(
            output,
            "{{\"schema_version\":{},\"path\":{},\"format\":{},\"api\":{},\"requested_backend\":{},\"resolved_backend\":{},\"batch_size\":{},\"initial_open_ms\":{:.6},\"levels\":[",
            REPORT_SCHEMA_VERSION,
            json_string(&self.path.to_string_lossy()),
            json_string(&self.format),
            json_string(self.api.label()),
            json_string(self.requested_backend.label()),
            json_string(&self.resolved_backend),
            self.batch_size,
            self.initial_open_ms
        )
        .expect("writing to a String cannot fail");
        for (index, level) in self.levels.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"index\":{},\"width\":{},\"height\":{},\"downsample\":{:.9},\"cols\":{},\"rows\":{}}}",
                level.index, level.width, level.height, level.downsample, level.cols, level.rows
            )
            .expect("writing to a String cannot fail");
        }
        output.push_str("],\"trials\":[");
        for (index, trial) in self.trials.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"trial\":{},\"level\":{},\"tiles\":{},\"open_ms\":{:.6},\"prepare_ms\":{:.6},\"first_batch_ms\":{:.6},\"warm_batch_ms\":{:.6},\"cpu_outputs\":{},\"metal_outputs\":{}}}",
                trial.trial,
                trial.level,
                trial.tiles,
                trial.open_ms,
                trial.prepare_ms,
                trial.first_batch_ms,
                trial.warm_batch_ms,
                trial.cpu_outputs,
                trial.metal_outputs,
            )
            .expect("writing to a String cannot fail");
        }
        output.push_str("],\"statistics\":{");
        for (index, (name, values)) in [
            (
                "open_ms",
                distribution(self.trials.iter().map(|trial| trial.open_ms)),
            ),
            (
                "prepare_ms",
                distribution(self.trials.iter().map(|trial| trial.prepare_ms)),
            ),
            (
                "first_batch_ms",
                distribution(self.trials.iter().map(|trial| trial.first_batch_ms)),
            ),
            (
                "warm_batch_ms",
                distribution(self.trials.iter().map(|trial| trial.warm_batch_ms)),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{}:{{\"min\":{:.6},\"p50\":{:.6},\"p95\":{:.6},\"max\":{:.6}}}",
                json_string(name),
                values.min,
                values.p50,
                values.p95,
                values.max,
            )
            .expect("writing to a String cannot fail");
        }
        output.push_str("}}");
        output
    }
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{centered_block, parse_arguments, percentile, ProbeApi, RequestedBackend};

    #[test]
    fn arguments_enable_json_and_repeated_trials() {
        let arguments = parse_arguments([
            OsString::from("--json"),
            OsString::from("--trials"),
            OsString::from("5"),
            OsString::from("--api"),
            OsString::from("controlled-render"),
            OsString::from("--backend"),
            OsString::from("cpu"),
            OsString::from("--batch-size"),
            OsString::from("8"),
            OsString::from("slide.svs"),
        ])
        .expect("arguments should parse");

        assert!(arguments.json);
        assert_eq!(arguments.trials, 5);
        assert_eq!(arguments.api, ProbeApi::ControlledRender);
        assert_eq!(arguments.backend, RequestedBackend::Cpu);
        assert_eq!(arguments.batch_size, 8);
        assert_eq!(arguments.path, std::path::Path::new("slide.svs"));
    }

    #[test]
    fn arguments_reject_unsupported_batch_size() {
        let error = parse_arguments([
            OsString::from("--batch-size"),
            OsString::from("3"),
            OsString::from("slide.svs"),
        ])
        .expect_err("unsupported batch size should fail");

        assert!(error.contains("1, 2, 4, 8, or 16"));
    }

    #[test]
    fn percentile_uses_nearest_rank_without_hiding_raw_samples() {
        let values = [40.0, 10.0, 30.0, 20.0, 50.0];

        assert_eq!(percentile(&values, 0.0), 10.0);
        assert_eq!(percentile(&values, 0.5), 30.0);
        assert_eq!(percentile(&values, 0.95), 50.0);
        assert_eq!(percentile(&[], 0.95), 0.0);
    }

    #[test]
    fn centered_blocks_rotate_but_keep_requested_cardinality() {
        let first = centered_block(20, 20, 8, 0);
        let second = centered_block(20, 20, 8, 1);

        assert_eq!(first.len(), 8);
        assert_eq!(second.len(), 8);
        assert_ne!(first, second);
    }
}
