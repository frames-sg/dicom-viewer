use super::*;

#[test]
fn level_and_tile_identifiers_preserve_checked_platform_boundaries() {
    let level = LevelIndex::from_usize(7).expect("small level should fit");
    assert_eq!(level.get(), 7);
    assert_eq!(level.as_usize(), 7);
    assert_eq!(level.to_string(), "7");
    if usize::BITS > u32::BITS {
        assert!(LevelIndex::from_usize(u32::MAX as usize + 1).is_err());
    }

    let coord = TileCoord::new(4, 9);
    assert_eq!((coord.col(), coord.row()), (4, 9));
    assert_eq!(coord.as_wsi_rs_i64().unwrap(), (4, 9));
    assert!(TileCoord::new(u64::MAX, 0).as_wsi_rs_i64().is_err());
    assert!(TileCoord::new(0, u64::MAX).as_wsi_rs_i64().is_err());
}

#[test]
fn public_status_and_backend_labels_cover_every_declared_variant() {
    assert_eq!(ColorManagementStatus::Unprofiled.to_string(), "unprofiled");
    assert_eq!(ColorManagementStatus::Applied.to_string(), "applied");
    assert_eq!(
        ColorManagementStatus::MalformedProfile.to_string(),
        "malformed profile"
    );
    assert_eq!(
        ColorManagementStatus::LutValidationFailed.to_string(),
        "LUT validation failed"
    );
    assert_eq!(ColorManagementMode::Identity.to_string(), "identity");
    assert_eq!(
        ColorManagementMode::CpuLittleCms.to_string(),
        "LittleCMS → sRGB"
    );
    assert_eq!(
        ColorManagementMode::MetalLut65.to_string(),
        "Metal 65³ LUT → sRGB"
    );
    assert_eq!(
        ColorManagementMode::CpuLutValidationFallback.to_string(),
        "CPU LittleCMS fallback"
    );
    assert_eq!(
        ColorManagementMode::UncorrectedMalformedProfile.to_string(),
        "uncorrected"
    );
    assert_eq!(TileDecodeBackend::Cpu.to_string(), "CPU");
    assert_eq!(TileDecodeBackend::Metal.to_string(), "Metal");
    assert_eq!(TileDecodeBackend::Cuda.to_string(), "CUDA");
    assert_eq!(
        ColorManagementSummary::unprofiled(),
        ColorManagementSummary {
            status: ColorManagementStatus::Unprofiled,
            sha256: None,
            byte_size: None,
            provenance: None,
            applied_mode: ColorManagementMode::Identity,
        }
    );
}

#[test]
fn tile_layouts_report_bounded_display_extents_and_grid_semantics() {
    let regular = LevelTileLayout::Regular {
        tile_width: 0,
        tile_height: 256,
        tiles_across: 3,
        tiles_down: 2,
    };
    assert_eq!(regular.display_tile_size(), (1, 256));
    assert_eq!(regular.grid_size(), Some((3, 2)));
    assert!(regular.contains(TileCoord::new(2, 1)));
    assert!(!regular.contains(TileCoord::new(3, 1)));

    let whole = LevelTileLayout::WholeLevel {
        width: 513,
        height: 1025,
        virtual_tile_width: 513,
        virtual_tile_height: 1025,
    };
    assert_eq!(whole.display_tile_size(), (512, 512));
    assert_eq!(whole.grid_size(), Some((2, 3)));

    for (advance, expected) in [
        ((32.1, 64.0), (33, 64)),
        ((f64::NAN, f64::INFINITY), (1, 1)),
        ((-2.0, 0.0), (1, 1)),
        ((f64::MAX, f64::MAX), (u32::MAX, u32::MAX)),
    ] {
        let irregular = LevelTileLayout::Irregular {
            tile_advance: advance,
            tile_count: 1,
        };
        assert_eq!(irregular.display_tile_size(), expected);
        assert_eq!(irregular.grid_size(), None);
        assert!(!irregular.contains(TileCoord::new(0, 0)));
    }
}

#[test]
fn open_options_and_color_luts_validate_explicit_resource_contracts() {
    let budgets = ViewerCacheBudgets::new(3, 2, 1);
    assert_eq!(
        ViewerOpenOptions::cpu_only()
            .with_cache_budgets(budgets)
            .cache_budgets(),
        budgets
    );
    assert_eq!(
        ViewerCacheBudgets::default(),
        ViewerCacheBudgets::balanced()
    );
    assert!(ViewerCacheBudgets::large().viewer_tile_bytes > budgets.viewer_tile_bytes);
    assert!(format!("{:?}", ViewerOpenOptions::default()).contains("Auto"));
    assert!(format!("{:?}", ViewerOpenOptions::cpu_only()).contains("CpuOnly"));
    #[cfg(any(target_os = "macos", feature = "cuda"))]
    assert!(ViewerOpenOptions::cpu_only().requests_cpu_only());
    #[cfg(target_os = "macos")]
    assert!(ViewerOpenOptions::auto().metal_device().is_none());

    assert!(ColorLut3d::from_rgba8(1, vec![0; 4], "small").is_err());
    assert!(ColorLut3d::from_rgba8(u32::MAX, Vec::new(), "overflow").is_err());
    let lut =
        ColorLut3d::from_rgba8(2, vec![7; 32], "digest").expect("2-cube RGBA LUT should be valid");
    assert_eq!(lut.edge(), 2);
    assert_eq!(lut.rgba(), &[7; 32]);
    assert_eq!(lut.profile_sha256(), "digest");
}

#[test]
fn viewer_source_identity_is_stable_path_free_and_plane_specific() {
    let first = ViewerSourceIdentity::new(17, 2, 3, 4, 5, 6, (40_000, 20_000));
    let same = ViewerSourceIdentity::new(17, 2, 3, 4, 5, 6, (40_000, 20_000));
    let other_plane = ViewerSourceIdentity::new(17, 2, 3, 4, 5, 7, (40_000, 20_000));

    assert_eq!(first, same);
    assert_ne!(first, other_plane);
    assert_eq!(first.dataset_id(), 17);
    assert_eq!(first.scene(), 2);
    assert_eq!(first.series(), 3);
    assert_eq!(first.plane(), (4, 5, 6));
    assert_eq!(first.dimensions(), (40_000, 20_000));
    assert_eq!(first.digest().len(), 64);
    assert!(!format!("{first:?}").contains('/'));
}
