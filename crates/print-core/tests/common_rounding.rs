#[path = "../../layout-core/tests/fixtures/common_rounding.rs"]
mod fixture;
use layout_core::{ResolvedPublication, ResolvedSceneObjectKind};
use print_core::{MissingImagePolicy, ScenePdfOptions, ScenePdfWriter};

#[test]
fn text_table_picture_clip_shadow_and_border_share_the_physical_rounded_outline() {
    let (project, fonts, _) = fixture::fixture();
    let publication =
        ResolvedPublication::resolve_output_with_font_resources(&project, &fonts).unwrap();
    let options = ScenePdfOptions {
        missing_images: MissingImagePolicy::VisiblePlaceholder,
    };
    let serialized = ScenePdfWriter::write(&publication, options).unwrap();
    let repeated = ScenePdfWriter::write(&publication, options).unwrap();
    assert_eq!(serialized.bytes(), repeated.bytes());
    let pdf = String::from_utf8_lossy(serialized.bytes());
    assert_eq!(
        pdf.matches("18 0 m\n54 0 l\n").count(),
        8,
        "Fixed text foreground/shadow, Table clip/shadow/top tangent, Picture placeholder/shadow/border share one physical outline"
    );
    assert!(
        pdf.matches("h\nW\nn\n").count() >= 6,
        "Text, Fit text, Table and Picture are clipped before painting"
    );
    assert_eq!(serialized.evidence().text_object_count, 6);
    assert!(
        serialized.evidence().pages[0]
            .clipped_table_cells
            .is_empty()
    );
    let ResolvedSceneObjectKind::Table { table } = &publication.pages()[0].objects[2].kind else {
        panic!("Table")
    };
    assert_eq!(
        table
            .border_segments
            .iter()
            .flat_map(|edge| &edge.outline)
            .filter(|command| matches!(
                command,
                document_core::shape_geometry::ShapePathCommand::CubicTo(..)
            ))
            .count(),
        4
    );
    assert_eq!(
        serialized.evidence().scene_digest,
        publication.semantic_digest()
    );
}

#[cfg(windows)]
#[test]
fn oriented_alpha_picture_rounding_preserves_profile_crop_flip_and_unclipped_shadow_offset() {
    use print_core::{RasterSourceProfileOrigin, prepare_publication_rasters};
    let (project, fonts, assets) = fixture::fixture();
    let retained = assets.clone();
    let scene = ResolvedPublication::resolve_output_with_font_resources(&project, &fonts).unwrap();
    let rasters = prepare_publication_rasters(
        &scene,
        &assets,
        include_bytes!("fixtures/SWOP2006coated_300_mth.icc"),
        &raster_boundary::TrustedRasterBoundary::resolve_packaged(),
        &color_boundary::TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap();
    assert_eq!(rasters.len(), 1);
    assert!(rasters[0].evidence().has_transparency);
    assert_eq!(
        rasters[0].evidence().source_profile_origin,
        RasterSourceProfileOrigin::Embedded
    );
    let pdf =
        ScenePdfWriter::write_with_rasters(&scene, ScenePdfOptions::default(), &rasters).unwrap();
    let bytes = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(
        bytes.matches("-144 0 0 72 90 -18 cm").count(),
        2,
        "intrinsic crop/flip stays shared by Picture and its alpha shadow"
    );
    let (finished, _) = print_core::PublisherSceneAdapter::lower(
        &scene,
        ScenePdfOptions::default(),
        &rasters,
        None,
        None,
    )
    .unwrap();
    // The adapter composes the page/object matrices. The shadow retains its
    // authored offset relative to the matching foreground viewport group.
    let groups = finished.pages[0]
        .items
        .iter()
        .filter(|item| {
            matches!(
        &item.content, print_core::finished::FinishedContent::Group(group)
        if group.items.iter().any(|child| matches!(&child.content,
            print_core::finished::FinishedContent::Image(_) |
            print_core::finished::FinishedContent::ImageAlpha { .. })))
        })
        .collect::<Vec<_>>();
    assert_eq!(groups.len(), 2);
    assert!((groups[0].transform.0[4] - groups[1].transform.0[4] - 7.2).abs() < 0.001);
    assert!((groups[0].transform.0[5] - groups[1].transform.0[5] + 14.4).abs() < 0.001);
    assert_eq!(bytes.matches("18 0 m\n54 0 l\n").count(), 8);
    let object = &project.document.pages[0].objects[3];
    let document_core::model::ObjectKind::Image { crop, flip, .. } = &object.kind else {
        panic!("Picture")
    };
    let evidence = &pdf.evidence().raster_placements[0];
    assert_eq!(evidence.crop, *crop);
    assert_eq!(evidence.flip, *flip);
    assert_eq!(evidence.transform, object.transform);
    assert_eq!(assets, retained);
}
