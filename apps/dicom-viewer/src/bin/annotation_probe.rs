#![forbid(unsafe_code)]

use peak_alloc::PeakAlloc;

#[path = "annotation_probe/command.rs"]
mod command;
#[path = "annotation_probe/conversion_report.rs"]
mod conversion_report;
#[path = "annotation_probe/convert_geojson.rs"]
mod convert_geojson;
#[path = "annotation_probe/convert_raster.rs"]
mod convert_raster;
#[path = "annotation_probe/legacy/mod.rs"]
mod legacy;
#[path = "annotation_probe/publication.rs"]
mod publication;

#[global_allocator]
pub(crate) static PEAK_ALLOC: PeakAlloc = PeakAlloc;

fn main() {
    let exit_code = command::execute(
        std::env::args_os().skip(1),
        std::io::stdout().lock(),
        std::io::stderr().lock(),
    );
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
