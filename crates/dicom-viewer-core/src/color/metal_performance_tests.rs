use super::tests::gamma_rgb_profile;
use super::*;

#[test]
#[ignore = "release ICC resolution investigation; requires DICOM_VIEWER_WSI_FIXTURE"]
fn metal_lut_resolution_characterization() {
    if cfg!(debug_assertions) {
        panic!("run with --release");
    }
    let path = std::env::var_os("DICOM_VIEWER_WSI_FIXTURE").expect("trusted local fixture");
    let study =
        crate::ViewerStudy::open_path_with_options(path, crate::ViewerOpenOptions::cpu_only())
            .unwrap();
    let transform = study
        .color_management
        .transform
        .as_deref()
        .expect("profiled fixture");
    let profiles = std::iter::once(("real fixture".to_string(), None)).chain(
        [0.8, 1.0, 1.2, 1.4, 1.6, 2.4, 2.8]
            .into_iter()
            .map(|gamma| {
                (
                    format!("gamma {gamma}"),
                    Some(IccTransform::new(&gamma_rgb_profile(gamma)).unwrap()),
                )
            }),
    );
    for (name, candidate) in profiles {
        let transform = candidate.as_ref().unwrap_or(transform);
        for edge in [65usize, 86, 129] {
            let mut rgba = Vec::with_capacity(edge * edge * edge * 4);
            for blue in 0..edge {
                for green in 0..edge {
                    for red in 0..edge {
                        rgba.extend_from_slice(
                            &[red, green, blue]
                                .map(|v| ((v * 255 + (edge - 1) / 2) / (edge - 1)) as u8),
                        );
                        rgba.push(255);
                    }
                }
            }
            transform.transform_rgba(&mut rgba);
            let lut = ColorLut3d::new(edge as u32, rgba, "resolution comparison".into());
            if edge == LUT_EDGE as usize {
                assert_eq!(lut.rgba(), generate_lut(transform, "reference").rgba());
            }
            let started = std::time::Instant::now();
            let result = validate_lut(transform, &lut);
            eprintln!(
                "profile={name:?} edge={edge} validation={result:?} elapsed_ms={}",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}
