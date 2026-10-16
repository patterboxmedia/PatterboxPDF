use color_boundary::TrustedColorBoundary;
use document_core::model::{
    Color, DocumentId, ImageContentKind, Insets, ObjectDraft, ObjectEffects, ObjectKind,
    PhysicalSize, Revision, ShapeStroke, Transform, UntaggedRasterPolicy, WrapMode,
};
use document_core::{
    AssetHash, AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot,
    RasterColorProfile, RasterInspection, Transaction, inspect_raster,
};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use layout_core::ResolvedPublication;
use print_core::{
    FixtureContract, FontBoundary, PreflightBasis, ProductionRaster, ProductionRasterError,
    PublicationCandidateFindingCode, PublicationCandidateFindingSeverity, PublicationCandidatePdf,
    PublicationCandidatePreflight, PublicationCandidateWriter, PublicationFindingFacts,
    PublicationWarningAcknowledgement, RasterResolutionPreset, RasterSourceProfileOrigin,
    ScenePdfOptions, ScenePdfWriter, inspect_output_intent, prepare_publication_colors,
    prepare_publication_rasters,
};
use raster_boundary::TrustedRasterBoundary;

#[path = "support/pdf_operators.rs"]
mod pdf_operators;
use pdf_operators::alpha_invocation_count;

const OUTPUT_PROFILE: &[u8] = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");
const TAGGED_TIFF: &[u8] = include_bytes!("fixtures/embedded-rgb-alpha-3x2.tif");
const UNTAGGED_TIFF: &[u8] = include_bytes!("fixtures/oriented-alpha-3x2.tif");

#[test]
fn oriented_picture_crop_and_flip_keep_source_profile_and_alpha_shadow_aligned() {
    let inspection = inspect_raster(TAGGED_TIFF).unwrap();
    assert_eq!(inspection.exif_orientation, 6);
    let (mut project, assets, asset) = placed_raster_project(TAGGED_TIFF, inspection);
    let original_assets = assets.clone();
    let object = &mut project.document.pages[0].objects[0];
    object.size = PhysicalSize::new(25_400, 50_800).unwrap();
    object.transform.rotation_microdegrees = 20_000_000;
    object.effects.shadow = Some(document_core::model::HardShadow {
        offset_x: document_core::model::Micrometres::new(2_540),
        offset_y: document_core::model::Micrometres::new(5_080),
        color: Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        },
    });
    let crop = document_core::model::ImageCrop::new(125_000, 250_000, 375_000, 250_000).unwrap();
    let placement = object.transform;
    let mut previous_digest = None;
    for (horizontal, vertical, matrix) in [
        (false, false, "144 0 0 -288 -18 216 cm"),
        (true, false, "-144 0 0 -288 90 216 cm"),
        (false, true, "144 0 0 288 -18 -72 cm"),
        (true, true, "-144 0 0 288 90 -72 cm"),
    ] {
        let flip = document_core::model::ImageFlip {
            horizontal,
            vertical,
        };
        let ObjectKind::Image {
            crop: current_crop,
            flip: current_flip,
            ..
        } = &mut project.document.pages[0].objects[0].kind
        else {
            panic!("Picture fixture")
        };
        *current_crop = crop;
        *current_flip = flip;
        let publication = ResolvedPublication::resolve(&project).unwrap();
        assert_ne!(previous_digest, Some(publication.semantic_digest()));
        previous_digest = Some(publication.semantic_digest());
        let rasters = prepare_publication_rasters(
            &publication,
            &assets,
            OUTPUT_PROFILE,
            &TrustedRasterBoundary::resolve_packaged(),
            &TrustedColorBoundary::resolve_packaged(),
        )
        .unwrap();
        assert_eq!(
            (rasters[0].evidence().width, rasters[0].evidence().height),
            (2, 3)
        );
        assert!(rasters[0].evidence().has_transparency);
        assert_eq!(
            rasters[0].evidence().source_profile_origin,
            RasterSourceProfileOrigin::Embedded
        );
        let scene =
            ScenePdfWriter::write_with_rasters(&publication, ScenePdfOptions::default(), &rasters)
                .unwrap();
        let pdf = String::from_utf8_lossy(scene.bytes());
        assert_eq!(
            pdf.matches(matrix).count(),
            2,
            "Picture and alpha shadow use the same intrinsic crop/flip matrix"
        );
        let evidence = &scene.evidence().raster_placements[0];
        assert_eq!(evidence.crop, crop);
        assert_eq!(evidence.flip, flip);
        assert_eq!(evidence.transform, placement);
        assert_eq!(evidence.raster.asset, asset);
        assert_eq!(evidence.scene_digest, publication.semantic_digest());
        assert_eq!(assets, original_assets);
    }
}

#[test]
fn legacy_tagged_tiff_recovers_its_exact_profile_without_changing_saved_metadata_or_policy() {
    assert_eq!(
        AssetHash::for_bytes(TAGGED_TIFF).as_str(),
        "10ded8bf59b5a89142dbcedbc70a5dbda4165d0e2e8a4d85164ef71e203019e3"
    );
    let mut legacy = inspect_raster(TAGGED_TIFF).expect("inspect tagged TIFF");
    assert!(matches!(
        legacy.color_profile,
        RasterColorProfile::Embedded { .. }
    ));
    legacy.color_profile = RasterColorProfile::Unspecified;
    let (project, assets, asset) = placed_raster_project(TAGGED_TIFF, legacy);
    let unchanged_assets = assets.clone();
    let publication = ResolvedPublication::resolve(&project).unwrap();
    let source_digest = publication.source_project_digest();
    let rasters = prepare_publication_rasters(
        &publication,
        &assets,
        OUTPUT_PROFILE,
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    )
    .expect("fresh worker recovers the original embedded profile");
    assert_eq!(rasters.len(), 1);
    assert_eq!(rasters[0].evidence().asset, asset);
    assert_eq!(
        rasters[0].evidence().source_profile_origin,
        RasterSourceProfileOrigin::Embedded
    );
    assert_eq!(
        rasters[0].evidence().source_profile_hash,
        print_core::ProfileHash::of(include_bytes!("fixtures/generated-srgb.icc")).bytes()
    );
    assert_eq!(
        publication.untagged_raster_policy(),
        UntaggedRasterPolicy::RequireEmbeddedProfile
    );
    assert_eq!(publication.source_project_digest(), source_digest);
    assert_eq!(assets, unchanged_assets);
}

#[test]
fn untagged_tiff_still_requires_explicit_policy_after_fresh_worker_inspection() {
    let inspection = inspect_raster(UNTAGGED_TIFF).expect("inspect untagged TIFF");
    assert_eq!(inspection.color_profile, RasterColorProfile::Unspecified);
    let (project, assets, asset) = placed_raster_project(UNTAGGED_TIFF, inspection);
    let publication = ResolvedPublication::resolve(&project).unwrap();
    let result = prepare_publication_rasters(
        &publication,
        &assets,
        &[], // Refuse the missing source profile before trying color conversion.
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    );
    assert!(matches!(
        result,
        Err(ProductionRasterError::UntaggedRasterRequiresPolicy {
            asset: refused_asset,
            page_id,
            object_id,
        }) if refused_asset == asset
            && page_id == project.document.pages[0].id
            && object_id == project.document.pages[0].objects[0].id
    ));
    assert_eq!(
        publication.untagged_raster_policy(),
        UntaggedRasterPolicy::RequireEmbeddedProfile
    );
}

#[test]
fn legacy_tiff_profile_recovery_rejects_unrelated_stored_inspection_drift() {
    let mut legacy = inspect_raster(TAGGED_TIFF).expect("inspect tagged TIFF");
    legacy.color_profile = RasterColorProfile::Unspecified;
    legacy.width_pixels += 1;
    let (project, assets, asset) = placed_raster_project(TAGGED_TIFF, legacy);
    let publication = ResolvedPublication::resolve(&project).unwrap();
    let result = prepare_publication_rasters(
        &publication,
        &assets,
        &[], // Metadata drift must be refused before color conversion.
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    );
    assert!(matches!(
        result,
        Err(ProductionRasterError::StoredInspectionDrift(refused_asset)) if refused_asset == asset
    ));
}

fn placed_raster_project(
    bytes: &[u8],
    inspection: RasterInspection,
) -> (ProjectSnapshot, AssetStore, AssetHash) {
    let mut assets = AssetStore::default();
    let asset = assets.insert_raster(bytes.to_vec(), inspection);
    let document = Document::new(
        DocumentId::parse("legacy-tiff-book").unwrap(),
        "Legacy TIFF book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Place TIFF picture",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "TIFF picture".to_owned(),
                        size: PhysicalSize::new(25_400, 25_400).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Image {
                            asset: asset.clone(),
                            wrap_mode: WrapMode::None,
                            content_kind: ImageContentKind::ContinuousTone,
                            crop: document_core::model::ImageCrop::default(),
                            flip: document_core::model::ImageFlip::default(),
                            lock_aspect_ratio: true,
                        },
                    },
                },
            ),
        )
        .unwrap();
    (ProjectSnapshot::from_session(&session), assets, asset)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one worker-backed journey binds source policy, raster evidence, and raster-only blend space"
)]
fn untagged_raster_requires_and_records_the_explicit_srgb_policy() {
    let rgba = [40, 80, 120, 255];
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(&rgba, 1, 1, ExtendedColorType::Rgba8)
        .expect("encode untagged PNG");
    let inspection = inspect_raster(&png_bytes).expect("inspect untagged PNG");
    let mut assets = AssetStore::default();
    let asset = assets.insert_raster(png_bytes, inspection);
    let document = Document::new(
        DocumentId::parse("untagged-raster-book").unwrap(),
        "Untagged raster book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let placed = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Place untagged picture",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: ObjectDraft {
                        name: "Untagged picture".to_owned(),
                        size: PhysicalSize::new(25_400, 25_400).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 500_000,
                        kind: ObjectKind::Image {
                            asset: asset.clone(),
                            wrap_mode: WrapMode::None,
                            content_kind: ImageContentKind::ContinuousTone,
                            crop: document_core::model::ImageCrop::default(),
                            flip: document_core::model::ImageFlip::default(),
                            lock_aspect_ratio: true,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let object_id = placed.created_objects[0].clone();
    let fail_closed =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let Err(error) = prepare_publication_rasters(
        &fail_closed,
        &assets,
        OUTPUT_PROFILE,
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    ) else {
        panic!("untagged raster requires an explicit document policy");
    };
    assert_eq!(
        error,
        ProductionRasterError::UntaggedRasterRequiresPolicy {
            asset: asset.clone(),
            page_id,
            object_id,
        }
    );

    session
        .execute(
            placed.revision,
            Transaction::single(
                "Assume sRGB for untagged pictures",
                Command::UpdateUntaggedRasterPolicy {
                    policy: UntaggedRasterPolicy::AssumeSrgb,
                },
            ),
        )
        .unwrap();
    let allowed = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    assert_ne!(fail_closed.semantic_digest(), allowed.semantic_digest());
    assert_ne!(
        fail_closed.source_project_digest(),
        allowed.source_project_digest()
    );
    let rasters = prepare_publication_rasters(
        &allowed,
        &assets,
        OUTPUT_PROFILE,
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    )
    .expect("explicit sRGB policy allows the same untagged raster");
    assert_eq!(rasters.len(), 1);
    assert_eq!(
        rasters[0].evidence().source_profile_origin,
        RasterSourceProfileOrigin::AssumedSrgb
    );
    assert_ne!(rasters[0].evidence().source_profile_hash, [0; 32]);
    let scene =
        ScenePdfWriter::write_with_rasters(&allowed, ScenePdfOptions::default(), &rasters).unwrap();
    let bytes = String::from_utf8_lossy(scene.bytes());
    assert!(!scene.evidence().uses_device_rgb);
    assert_eq!(bytes.matches("/Subtype /Form").count(), 2);
    assert!(
        bytes.contains("/CS /DeviceCMYK"),
        "raster-only groups do not require a vector palette"
    );
    assert!(!bytes.contains("/DeviceRGB"));
    assert_eq!(alpha_invocation_count(&bytes, 0.5), 1);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end test keeps worker, candidate, and preflight raster evidence in a single chain"
)]
fn tagged_transparent_raster_is_worker_decoded_converted_and_placed_as_cmyk() {
    let source_profile = lcms2::Profile::new_srgb()
        .icc()
        .expect("serialize sRGB profile");
    let rgba = [255, 0, 0, 255, 0, 255, 0, 64];
    let mut png_bytes = Vec::new();
    let mut png_encoder = PngEncoder::new(&mut png_bytes);
    png_encoder
        .set_icc_profile(source_profile)
        .expect("PNG supports ICC");
    png_encoder
        .write_image(&rgba, 2, 1, ExtendedColorType::Rgba8)
        .expect("encode tagged PNG");
    let inspection = inspect_raster(&png_bytes).expect("test import inspection");
    let mut assets = AssetStore::default();
    let asset = assets.insert_raster(png_bytes, inspection);

    let document = Document::new(
        DocumentId::parse("trusted-raster-book").unwrap(),
        "Trusted raster book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page = document.pages[0].id.clone();
    let mut transform = Transform::identity_at(12_000, 18_000);
    transform.scale_x_nanos = 1_500_000_000;
    transform.scale_y_nanos = 700_000_000;
    let mut session = DocumentSession::new(document).unwrap();
    let placed = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Place stretched picture",
                Command::AddObject {
                    location: ObjectLocation::Page(page),
                    object: ObjectDraft {
                        name: "Tagged picture".to_owned(),
                        size: PhysicalSize::new(150, 100).unwrap(),
                        transform,
                        opacity_millionths: 875_000,
                        kind: ObjectKind::Image {
                            asset: asset.clone(),
                            wrap_mode: WrapMode::None,
                            content_kind: ImageContentKind::ContinuousTone,
                            crop: document_core::model::ImageCrop::default(),
                            flip: document_core::model::ImageFlip::default(),
                            lock_aspect_ratio: true,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let styled = session
        .execute(
            placed.revision,
            Transaction::single(
                "Border stretched picture",
                Command::UpdateObjectEffects {
                    object_id: placed.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: None,
                        text_outline: None,
                        picture_border: Some(ShapeStroke {
                            color: Color {
                                red: 10,
                                green: 20,
                                blue: 30,
                                alpha: 255,
                            },
                            width_millipoints: 2_000,
                        }),
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let rasters = prepare_publication_rasters(
        &publication,
        &assets,
        OUTPUT_PROFILE,
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    )
    .expect("both packaged workers accept exact raster");
    assert_eq!(rasters.len(), 1);

    let scene =
        ScenePdfWriter::write_with_rasters(&publication, ScenePdfOptions::default(), &rasters)
            .expect("trusted raster scene serializes");
    let text = String::from_utf8_lossy(scene.bytes());
    assert!(text.contains("/DeviceCMYK"));
    assert!(text.contains("/SMask"));
    assert!(text.contains(" Do\n"));
    let raster = text.find(" Do\n").expect("clipped raster paint");
    let border = text[raster..]
        .find("2 w\n")
        .map(|offset| raster + offset)
        .expect("picture border width after raster");
    assert!(raster < border);
    let placements = &scene.evidence().raster_placements;
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].raster.asset, asset);
    assert_eq!(placements[0].transform.scale_x_nanos, 1_500_000_000);
    assert_eq!(placements[0].transform.scale_y_nanos, 700_000_000);
    assert_eq!(placements[0].scene_digest, publication.semantic_digest());
    assert!(placements[0].raster.has_transparency);
    assert_eq!(
        placements[0].raster.source_profile_origin,
        RasterSourceProfileOrigin::Embedded
    );
    assert_ne!(placements[0].raster.raster_worker_executable_hash, [0; 32]);
    assert_ne!(placements[0].raster.color_worker_executable_hash, [0; 32]);
    assert!(scene.evidence().transparency.object_opacity);
    assert!(!scene.evidence().transparency.paint_alpha);
    assert!(scene.evidence().transparency.raster_soft_mask);

    let fixture = print_core::FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        inspect_output_intent(OUTPUT_PROFILE).unwrap(),
        FontBoundary::NotExercised,
    );
    let colors = prepare_publication_colors(
        &publication,
        OUTPUT_PROFILE,
        &TrustedColorBoundary::resolve_packaged(),
    )
    .expect("trusted color worker accepts the picture border")
    .expect("the visible picture border produces a vector palette");
    let candidate = PublicationCandidateWriter::write_with_resources(
        &publication,
        &fixture,
        OUTPUT_PROFILE,
        &rasters,
        None,
        Some(&colors),
    )
    .expect("production candidate carries exact raster");
    assert_eq!(
        candidate.evidence().raster_placements,
        placements.as_slice()
    );
    let candidate_bytes = String::from_utf8_lossy(candidate.bytes());
    assert!(candidate_bytes.contains("/DeviceCMYK"));
    assert!(!candidate_bytes.contains("/DeviceRGB"));
    assert_eq!(candidate_bytes.matches("/Subtype /Form").count(), 2);
    assert!(candidate_bytes.contains("/CS /DeviceCMYK"));
    assert_eq!(
        alpha_invocation_count(&candidate_bytes, 0.875),
        1,
        "Picture opacity composites the decoded image and its border together"
    );
    let first_form = candidate_bytes
        .split("/Subtype /Form")
        .next()
        .unwrap()
        .lines()
        .rev()
        .find(|line| line.ends_with(" 0 obj"))
        .unwrap()
        .split_ascii_whitespace()
        .next()
        .unwrap();
    let form_dictionary = candidate_bytes
        .split("/Subtype /Form")
        .nth(1)
        .unwrap()
        .split("stream")
        .next()
        .unwrap();
    assert!(
        !form_dictionary.contains(&format!("/G{first_form} ")),
        "Form resources cannot reference their own Form"
    );
    assert_low_resolution_warning(&publication, &fixture, &candidate);
    assert_mismatched_preset_suppresses_resolution_advice(&publication, &fixture, &candidate);
    write_visible_opacity_raster_fixture(&ProjectSnapshot::from_session(&session), &rasters);

    assert_line_art_resolution_blocker(
        &mut session,
        styled.revision,
        &placed.created_objects[0],
        &rasters,
    );
}

// Preserve the resolution-threshold fixture above. This separate composition
// reuses its attested pixels at a visible size; the palette is attested again
// for the new scene, and placement evidence is bound to the new candidate.
fn write_visible_opacity_raster_fixture(project: &ProjectSnapshot, rasters: &[ProductionRaster]) {
    let mut visible = project.clone();
    visible.document.pages[0].objects[0].size = PhysicalSize::new(50_800, 25_400).unwrap();
    let publication = ResolvedPublication::resolve(&visible).unwrap();
    let fixture = FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        inspect_output_intent(OUTPUT_PROFILE).unwrap(),
        FontBoundary::NotExercised,
    );
    let colors = prepare_publication_colors(
        &publication,
        OUTPUT_PROFILE,
        &TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap()
    .unwrap();
    let candidate = PublicationCandidateWriter::write_with_resources(
        &publication,
        &fixture,
        OUTPUT_PROFILE,
        rasters,
        None,
        Some(&colors),
    )
    .unwrap();
    assert_eq!(
        candidate.evidence().raster_placements[0].size,
        PhysicalSize::new(50_800, 25_400).unwrap()
    );
    let bytes = String::from_utf8_lossy(candidate.bytes());
    assert!(bytes.contains("/CS /DeviceCMYK"));
    assert!(!bytes.contains("/DeviceRGB"));
    assert_eq!(alpha_invocation_count(&bytes, 0.875), 1);
    if let Some(path) = std::env::var_os("PUBLISHER_OPACITY_RASTER_PDF") {
        std::fs::write(path, candidate.bytes()).unwrap();
    }
}

fn assert_mismatched_preset_suppresses_resolution_advice(
    publication: &ResolvedPublication,
    fixture: &FixtureContract,
    candidate: &PublicationCandidatePdf,
) {
    let stricter_preset =
        RasterResolutionPreset::from_static_thresholds("strict-test-preset", 350, 250, 1_200, 600)
            .unwrap();
    let mismatched_fixture = fixture
        .clone()
        .with_raster_resolution_preset(stricter_preset);
    let mismatched =
        PublicationCandidatePreflight::for_candidate(publication, &mismatched_fixture, candidate);
    assert!(mismatched.findings.iter().any(|finding| {
        finding.code == PublicationCandidateFindingCode::FixtureEvaluationMismatch
    }));
    assert!(!mismatched.findings.iter().any(|finding| {
        matches!(
            finding.code,
            PublicationCandidateFindingCode::ImageResolutionBelowPreset
                | PublicationCandidateFindingCode::ImageResolutionBelowMinimum
        )
    }));
}

fn assert_line_art_resolution_blocker(
    session: &mut DocumentSession,
    expected_revision: Revision,
    object_id: &document_core::ObjectId,
    rasters: &[ProductionRaster],
) {
    session
        .execute(
            expected_revision,
            Transaction::single(
                "Treat picture as line art",
                Command::UpdateImageContentKind {
                    object_id: object_id.clone(),
                    content_kind: ImageContentKind::LineArt,
                },
            ),
        )
        .unwrap();
    let line_art_publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(session)).unwrap();
    let line_art_fixture = print_core::FixtureContract::new(
        PreflightBasis::from(line_art_publication.clone()),
        inspect_output_intent(OUTPUT_PROFILE).unwrap(),
        FontBoundary::NotExercised,
    );
    let line_art_candidate = PublicationCandidateWriter::write_with_rasters(
        &line_art_publication,
        &line_art_fixture,
        OUTPUT_PROFILE,
        rasters,
    )
    .unwrap();
    let line_art_preflight = PublicationCandidatePreflight::for_candidate(
        &line_art_publication,
        &line_art_fixture,
        &line_art_candidate,
    );
    let resolution = line_art_preflight
        .findings
        .iter()
        .find(|finding| {
            finding.code == PublicationCandidateFindingCode::ImageResolutionBelowMinimum
        })
        .expect("the same placement is below the line-art minimum");
    assert_eq!(
        resolution.severity,
        PublicationCandidateFindingSeverity::Blocker
    );
    assert!(matches!(
        resolution.facts,
        PublicationFindingFacts::ImageResolution {
            content_kind: ImageContentKind::LineArt,
            effective_ppi_milli: 200_000..300_000,
            target_ppi: 1_200,
            minimum_ppi: 600,
        }
    ));
    assert!(PublicationWarningAcknowledgement::for_report(&line_art_preflight).is_none());
}

fn assert_low_resolution_warning(
    publication: &ResolvedPublication,
    fixture: &FixtureContract,
    candidate: &PublicationCandidatePdf,
) {
    let preflight = PublicationCandidatePreflight::for_candidate(publication, fixture, candidate);
    let resolution = preflight
        .findings
        .iter()
        .find(|finding| finding.code == PublicationCandidateFindingCode::ImageResolutionBelowPreset)
        .expect("the deliberately huge 2 by 1 pixel placement is low resolution");
    assert_eq!(
        resolution.severity,
        PublicationCandidateFindingSeverity::Warning
    );
    assert!(matches!(
        resolution.facts,
        PublicationFindingFacts::ImageResolution {
            content_kind: ImageContentKind::ContinuousTone,
            effective_ppi_milli: 200_000..300_000,
            target_ppi: 300,
            minimum_ppi: 200,
        }
    ));
    assert!(!preflight.warnings_acknowledged(None));
    let acknowledgement = PublicationWarningAcknowledgement::for_report(&preflight)
        .expect("a blocker-free low-resolution warning can be acknowledged");
    assert!(preflight.warnings_acknowledged(Some(&acknowledgement)));
    let mut changed = preflight;
    changed.export_identity = print_core::ArtifactHash::of(b"changed export capsule");
    assert!(!changed.warnings_acknowledged(Some(&acknowledgement)));
}
