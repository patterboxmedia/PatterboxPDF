use document_core::model::{
    BleedExpectation, Color, DocumentId, ExactFontVariantBinding, FontFaceBinding, HardShadow,
    InlineTextSpan, InlineTextStyle, Insets, Micrometres, ObjectDraft, ObjectEffects, ObjectKind,
    PhysicalSize, Revision, ShapeKind, ShapeStroke, TableData, TextFrame, TextSourceRange,
    TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
};
use layout_core::{ExactFontResource, ResolvedPublication};
use print_core::{
    FixtureContract, FontBoundary, PreflightBasis, PublicationCandidateError,
    PublicationCandidateFindingCode, PublicationCandidatePreflight, PublicationCandidateWriter,
    PublicationColorEvidence, PublicationFindingFacts, PublicationFontEmbeddingState,
    PublicationPageEdge, PublicationRepairAction, PublicationTextEvidence, TrustedColorBoundary,
    TrustedFontBoundary, inspect_output_intent, prepare_publication_colors,
    prepare_publication_fonts,
};

const PROFILE: &[u8] = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");

#[test]
fn ordered_blank_pages_receive_one_deterministic_print_envelope() {
    let document = Document::new(
        DocumentId::parse("blank-book").unwrap(),
        "Blank book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let first_page = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add intentional blank",
                Command::AddPage {
                    after: Some(first_page),
                    trim: PhysicalSize::new(215_900, 279_400).unwrap(),
                    bleed: Insets::uniform(3_175).unwrap(),
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);

    let first = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let second = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.evidence().pages.len(), 2);
    assert!(
        first
            .evidence()
            .pages
            .iter()
            .all(|page| page.object_ids_in_paint_order.is_empty())
    );
    assert_eq!(first.evidence().scene_digest, publication.semantic_digest());
    assert_ne!(
        first.evidence().source_scene_artifact_hash,
        first.artifact_hash(),
        "the decorated candidate must be distinct from the undecorated scene"
    );
    let bytes = String::from_utf8_lossy(first.bytes());
    assert!(bytes.contains("/Count 2"));
    assert!(bytes.contains("/OutputIntents"));
    assert!(bytes.contains("/GTS_PDFXVersion"));
    assert!(bytes.contains("/Metadata"));

    let preflight = PublicationCandidatePreflight::for_candidate(&publication, &fixture, &first);
    assert!(preflight.ready_for_independent_validation());
    assert_eq!(preflight.export_identity, first.export_identity());
    assert!(preflight.findings.iter().any(|finding| {
        finding.code == PublicationCandidateFindingCode::IndependentValidationRequired
    }));
}

#[test]
fn real_page_scene_is_bound_but_unmanaged_rgb_remains_a_blocker() {
    let document = Document::new(
        DocumentId::parse("shape-book").unwrap(),
        "Shape book",
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
                "Add cover shape",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Cover panel".to_owned(),
                        size: PhysicalSize::new(90_000, 60_000).unwrap(),
                        transform: Transform::identity_at(12_000, 18_000),
                        opacity_millionths: 800_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 20,
                                green: 80,
                                blue: 160,
                                alpha: 255,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::None,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();

    assert_eq!(candidate.evidence().pages.len(), 1);
    assert_eq!(
        candidate.evidence().pages[0]
            .object_ids_in_paint_order
            .len(),
        1
    );
    assert!(candidate.evidence().transparency.object_opacity);
    assert!(!candidate.evidence().transparency.paint_alpha);
    assert!(!candidate.evidence().transparency.raster_soft_mask);
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    assert!(!preflight.ready_for_independent_validation());
    assert!(
        preflight
            .findings
            .iter()
            .any(|finding| { finding.code == PublicationCandidateFindingCode::UnmanagedDeviceRgb })
    );
    assert!(String::from_utf8_lossy(candidate.bytes()).contains("/OutputIntents"));
    // The legacy preflight probe may describe unmanaged candidate paint. A
    // caller cannot transfer that diagnostic exception to the public writer.
    let (finished, _) = print_core::PublisherSceneAdapter::lower(
        &publication,
        print_core::ScenePdfOptions::default(),
        &[],
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        print_core::FinishedPdfWriter::write(
            &finished,
            &print_core::finished::FinishedPdfOptions {
                metadata: print_core::PdfCandidateMetadata::at_utc(
                    "Public guard",
                    "2026-10-04T00:00:00Z"
                )
                .unwrap(),
                mode: print_core::finished::FinishedOutputMode::PdfX4 {
                    output_profile: PROFILE
                },
            }
        )
        .unwrap_err(),
        print_core::finished::FinishedPdfError::RgbInPdfX
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "this end-to-end contract keeps the authored shape, trusted palette, deterministic candidate, preflight transition, and independent verdict together"
)]
fn trusted_vector_palette_removes_the_unmanaged_rgb_blocker() {
    let document = Document::new(
        DocumentId::parse("managed-shape-book").unwrap(),
        "Managed shape book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add managed cover shape",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Cover panel".to_owned(),
                        size: PhysicalSize::new(90_000, 60_000).unwrap(),
                        transform: Transform::identity_at(12_000, 18_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 20,
                                green: 80,
                                blue: 160,
                                alpha: 255,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::None,
                        },
                    },
                },
            ),
        )
        .unwrap();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Add managed hard shadow",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            color: Color {
                                red: 180,
                                green: 40,
                                blue: 30,
                                alpha: 255,
                            },
                            offset_x: Micrometres::new(2_540),
                            offset_y: Micrometres::new(2_540),
                        }),
                        text_outline: None,
                        picture_border: None,
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);
    let colors = prepare_publication_colors(
        &publication,
        PROFILE,
        &TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap()
    .expect("the painted shape produces a palette");
    let repeated_colors = prepare_publication_colors(
        &publication,
        PROFILE,
        &TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap()
    .expect("the repeated preparation produces the same palette");
    assert_eq!(colors.evidence(), repeated_colors.evidence());
    let candidate = PublicationCandidateWriter::write_with_resources(
        &publication,
        &fixture,
        PROFILE,
        &[],
        None,
        Some(&colors),
    )
    .unwrap();
    let repeated_candidate = PublicationCandidateWriter::write_with_resources(
        &publication,
        &fixture,
        PROFILE,
        &[],
        None,
        Some(&repeated_colors),
    )
    .unwrap();
    assert_eq!(candidate, repeated_candidate);

    assert_eq!(
        candidate.evidence().color,
        PublicationColorEvidence::ManagedDeviceCmyk
    );
    assert_eq!(
        candidate
            .evidence()
            .vector_colors
            .as_ref()
            .map(|evidence| evidence.color_count),
        Some(2)
    );
    let bytes = String::from_utf8_lossy(candidate.bytes());
    assert!(bytes.contains(" k\n"));
    assert!(!bytes.contains(" rg\n"));
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    assert!(preflight.ready_for_independent_validation());
    assert!(
        !preflight
            .findings
            .iter()
            .any(|finding| { finding.code == PublicationCandidateFindingCode::UnmanagedDeviceRgb })
    );
    #[cfg(feature = "pdf-oxide-validator")]
    {
        use print_core::{
            ArtifactHash, PdfOxideValidator, PrintStandard, ValidationRequest, ValidationVerdict,
            run_independent_validation,
        };
        let validation = run_independent_validation(
            &PdfOxideValidator::new(ArtifactHash::of(PROFILE)),
            ValidationRequest::new(
                preflight.evaluation_key,
                candidate.bytes(),
                PrintStandard::PdfX4,
            ),
        )
        .expect("the independent parser validates the exact managed-color candidate");
        assert_eq!(validation.verdict(), ValidationVerdict::Passed);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "this contract test keeps the legacy outline candidate and trusted embedded-subset candidate beside the same exact scene so the blocker transition remains explicit"
)]
fn resolved_text_legacy_outlines_and_embedded_subset_have_distinct_preflight_results() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = Document::new(
        DocumentId::parse("outline-book").unwrap(),
        "Outline book",
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
                "Add exact headline",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Headline".to_owned(),
                        size: PhysicalSize::new(120_000, 40_000).unwrap(),
                        transform: Transform::identity_at(10_000, 15_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Magic".to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "Exact test face".to_owned(),
                                font_size_millipoints: 24_000,
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    let font_resource = ExactFontResource {
        source,
        face_index: 0,
        bytes: font_bytes,
    };
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&font_resource),
    )
    .unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();

    assert_eq!(
        candidate.evidence().text,
        PublicationTextEvidence::NativeOutlines { source_count: 1 }
    );
    let bytes = String::from_utf8_lossy(candidate.bytes());
    assert!(!bytes.contains("/Subtype /Type0"));
    assert!(!bytes.contains(" BT\n"));
    assert!(bytes.contains(" m\n"));

    let preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        &publication,
        &fixture,
        &candidate,
        core::slice::from_ref(&font_resource),
    );
    let font_finding = preflight
        .findings
        .iter()
        .find(|finding| finding.code == PublicationCandidateFindingCode::FontEmbeddingUnresolved)
        .expect("outlined text remains blocked until its exact font is embedded");
    assert_eq!(
        font_finding.target.as_ref().map(|target| target.action),
        Some(PublicationRepairAction::PrepareEmbeddableFont)
    );
    assert_eq!(
        font_finding.facts,
        PublicationFindingFacts::FontEmbedding {
            state: PublicationFontEmbeddingState::SubsetAllowed,
        }
    );

    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&font_resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    let embedded = PublicationCandidateWriter::write_with_rasters_and_fonts(
        &publication,
        &fixture,
        PROFILE,
        &[],
        &fonts,
    )
    .unwrap();
    assert_eq!(
        embedded.evidence().text,
        PublicationTextEvidence::EmbeddedFonts {
            source_count: 1,
            font_count: 1,
            full_font_count: 0,
        }
    );
    assert_eq!(embedded.evidence().embedded_fonts.len(), 1);
    let bytes = String::from_utf8_lossy(embedded.bytes());
    assert!(bytes.contains("/Font"));
    assert!(bytes.contains("/FontFile2"));
    assert!(bytes.contains("/ToUnicode"));
    assert!(bytes.contains("/ActualText"));
    assert!(bytes.contains("BT\n"));
    #[cfg(feature = "pdf-oxide-validator")]
    {
        use print_core::{
            ArtifactHash, PdfOxideValidator, PrintStandard, ValidationRequest, ValidationVerdict,
            run_independent_validation,
        };
        let colors = prepare_publication_colors(
            &publication,
            PROFILE,
            &TrustedColorBoundary::resolve_packaged(),
        )
        .unwrap()
        .expect("text contributes an authored color");
        let managed_embedded = PublicationCandidateWriter::write_with_resources(
            &publication,
            &fixture,
            PROFILE,
            &[],
            Some(&fonts),
            Some(&colors),
        )
        .unwrap();
        let validation = run_independent_validation(
            &PdfOxideValidator::new(ArtifactHash::of(PROFILE)),
            ValidationRequest::new(
                managed_embedded.evidence().publication_evaluation_key,
                managed_embedded.bytes(),
                PrintStandard::PdfX4,
            ),
        )
        .expect("the independent parser evaluates the embedded-font candidate");
        assert_eq!(
            validation.verdict(),
            ValidationVerdict::Passed,
            "the independent descendant-font audit must prove the embedded program"
        );
        assert!(
            validation
                .findings()
                .iter()
                .all(|finding| !finding.contains("XFONT-001")),
            "the superseded top-level Type 0 false rejection must not escape"
        );
    }

    let embedded_preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        &publication,
        &fixture,
        &embedded,
        core::slice::from_ref(&font_resource),
    );
    assert!(
        embedded_preflight.findings.iter().all(|finding| {
            !matches!(
                finding.code,
                PublicationCandidateFindingCode::FontEmbeddingUnresolved
                    | PublicationCandidateFindingCode::FullFontEmbeddingRequired
                    | PublicationCandidateFindingCode::FontEmbeddingPermissionBlocked
            )
        }),
        "a worker-attested embedded subset must clear the font blocker"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one rights-aware preflight fixture keeps the regular and restricted variant evidence in one auditable path"
)]
fn preflight_inspects_nonembedded_and_restricted_inline_face_variants() {
    let regular_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let mut bold_bytes =
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf").to_vec();
    let table_count = usize::from(u16::from_be_bytes([bold_bytes[4], bold_bytes[5]]));
    let os2_record = (0..table_count)
        .map(|index| 12 + index * 16)
        .find(|offset| bold_bytes[*offset..*offset + 4] == *b"OS/2")
        .expect("bold fixture has OS/2 table");
    let os2_offset = usize::try_from(u32::from_be_bytes([
        bold_bytes[os2_record + 8],
        bold_bytes[os2_record + 9],
        bold_bytes[os2_record + 10],
        bold_bytes[os2_record + 11],
    ]))
    .unwrap();
    bold_bytes[os2_offset + 8..os2_offset + 10].copy_from_slice(&0x0002_u16.to_be_bytes());
    let resources = vec![
        ExactFontResource {
            source: AssetHash::for_bytes(&regular_bytes),
            face_index: 0,
            bytes: regular_bytes,
        },
        ExactFontResource {
            source: AssetHash::for_bytes(&bold_bytes),
            face_index: 0,
            bytes: bold_bytes,
        },
    ];
    let document = Document::new(
        DocumentId::parse("variant-font-preflight").unwrap(),
        "Variant font preflight",
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
                "Add regular and restricted bold",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Variant rights".to_owned(),
                        size: PhysicalSize::new(120_000, 40_000).unwrap(),
                        transform: Transform::identity_at(10_000, 15_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Plain Bold".to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: resources[0].source.clone(),
                                    face_index: 0,
                                }),
                                exact_font_variants: vec![ExactFontVariantBinding {
                                    font_weight: 700,
                                    italic: false,
                                    face: FontFaceBinding {
                                        source: resources[1].source.clone(),
                                        face_index: 0,
                                    },
                                }],
                                font_family: "Exact rights pair".to_owned(),
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting {
                                inline_spans: vec![InlineTextSpan {
                                    range: TextSourceRange { start: 6, end: 10 },
                                    style: InlineTextStyle {
                                        font_size_millipoints: None,
                                        bold: Some(true),
                                        italic: None,
                                        ..InlineTextStyle::default()
                                    },
                                }],
                                paragraph_runs: Vec::new(),
                            },
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &resources,
    )
    .unwrap();
    let fixture = fixture(&publication);
    let outlined = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        &publication,
        &fixture,
        &outlined,
        &resources,
    );

    let font_findings = preflight
        .findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.code,
                PublicationCandidateFindingCode::FontEmbeddingUnresolved
                    | PublicationCandidateFindingCode::FontEmbeddingPermissionBlocked
                    | PublicationCandidateFindingCode::FullFontEmbeddingRequired
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        font_findings.len(),
        2,
        "both used exact faces are inspected"
    );
    assert!(font_findings.iter().any(|finding| {
        finding.facts
            == PublicationFindingFacts::FontEmbedding {
                state: PublicationFontEmbeddingState::SubsetAllowed,
            }
    }));
    assert!(font_findings.iter().any(|finding| {
        finding.facts
            == PublicationFindingFacts::FontEmbedding {
                state: PublicationFontEmbeddingState::Restricted,
            }
    }));
}

#[test]
fn full_font_requirement_is_cleared_only_by_the_attested_complete_program() {
    let font_resource = full_font_resource();
    let document = Document::new(
        DocumentId::parse("full-font-book").unwrap(),
        "Full font book",
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
                "Add full-font headline",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Headline".to_owned(),
                        size: PhysicalSize::new(120_000, 40_000).unwrap(),
                        transform: Transform::identity_at(10_000, 15_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Magic".to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: font_resource.source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "Exact full test face".to_owned(),
                                font_size_millipoints: 24_000,
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&font_resource),
    )
    .unwrap();
    let fixture = fixture(&publication);
    let outlined = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let outlined_preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        &publication,
        &fixture,
        &outlined,
        core::slice::from_ref(&font_resource),
    );
    assert!(outlined_preflight.findings.iter().any(|finding| {
        finding.code == PublicationCandidateFindingCode::FullFontEmbeddingRequired
    }));

    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&font_resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    let embedded = PublicationCandidateWriter::write_with_rasters_and_fonts(
        &publication,
        &fixture,
        PROFILE,
        &[],
        &fonts,
    )
    .unwrap();
    assert_eq!(
        embedded.evidence().text,
        PublicationTextEvidence::EmbeddedFonts {
            source_count: 1,
            font_count: 1,
            full_font_count: 1,
        }
    );
    assert!(
        embedded
            .bytes()
            .windows(font_resource.bytes.len())
            .any(|window| window == font_resource.bytes)
    );
    assert!(String::from_utf8_lossy(embedded.bytes()).contains("/BaseFont /DejaVuSans"));
    let embedded_preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        &publication,
        &fixture,
        &embedded,
        core::slice::from_ref(&font_resource),
    );
    assert!(embedded_preflight.findings.iter().all(|finding| {
        !matches!(
            finding.code,
            PublicationCandidateFindingCode::FontEmbeddingUnresolved
                | PublicationCandidateFindingCode::FullFontEmbeddingRequired
                | PublicationCandidateFindingCode::FontEmbeddingPermissionBlocked
        )
    }));
}

#[test]
fn required_bleed_shortfall_targets_the_exact_page_object_and_edge() {
    let mut document = Document::new(
        DocumentId::parse("bleed-shortfall-book").unwrap(),
        "Bleed shortfall book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    document.pages[0].bleed_expectations.right = BleedExpectation::Required;
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add trim-aligned panel",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: ObjectDraft {
                        name: "Right edge panel".to_owned(),
                        size: PhysicalSize::new(15_900, 80_000).unwrap(),
                        transform: Transform::identity_at(200_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 30,
                                green: 80,
                                blue: 120,
                                alpha: 255,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::None,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    let finding = preflight
        .findings
        .iter()
        .find(|finding| finding.code == PublicationCandidateFindingCode::RequiredBleedMissing)
        .expect("required edge shortfall");
    let target = finding.target.as_ref().expect("repair target");
    assert_eq!(target.page_id, page_id.as_str());
    assert!(target.object_id.is_some());
    assert_eq!(
        target.action,
        PublicationRepairAction::ExtendObjectToBleed {
            edge: PublicationPageEdge::Right,
        }
    );
    assert_eq!(
        finding.facts,
        PublicationFindingFacts::BleedEdge {
            edge: PublicationPageEdge::Right,
        }
    );
}

#[test]
fn one_object_covering_required_bleed_satisfies_the_page_edge() {
    let mut document = Document::new(
        DocumentId::parse("covered-required-bleed-book").unwrap(),
        "Covered required bleed book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    document.pages[0].bleed_expectations.right = BleedExpectation::Required;
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    for (name, x, width) in [
        ("Covering background", 190_000, 29_075),
        ("Trim detail", 205_000, 10_900),
    ] {
        let expected = session.revision();
        session
            .execute(
                expected,
                Transaction::single(
                    "Add edge artwork",
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: name.to_owned(),
                            size: PhysicalSize::new(width, 40_000).unwrap(),
                            transform: Transform::identity_at(x, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: Color {
                                    red: 30,
                                    green: 80,
                                    blue: 120,
                                    alpha: 255,
                                },
                                stroke: ShapeStroke::default(),
                                wrap_mode: WrapMode::None,
                            },
                        },
                    },
                ),
            )
            .unwrap();
    }
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    assert!(
        preflight.findings.iter().all(|finding| {
            finding.code != PublicationCandidateFindingCode::RequiredBleedMissing
        })
    );
}

#[test]
fn visible_hard_shadow_uses_its_painted_extent_for_required_bleed() {
    let mut document = Document::new(
        DocumentId::parse("shadow-bleed-book").unwrap(),
        "Shadow bleed book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    document.pages[0].bleed_expectations.right = BleedExpectation::Required;
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add near-edge panel",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Shadowed panel".to_owned(),
                        size: PhysicalSize::new(13_000, 30_000).unwrap(),
                        transform: Transform::identity_at(200_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 30,
                                green: 60,
                                blue: 90,
                                alpha: 255,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::None,
                        },
                    },
                },
            ),
        )
        .unwrap();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Extend hard shadow through bleed",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            color: Color {
                                red: 10,
                                green: 20,
                                blue: 30,
                                alpha: 255,
                            },
                            offset_x: Micrometres::new(6_075),
                            offset_y: Micrometres::new(0),
                        }),
                        text_outline: None,
                        picture_border: None,
                    },
                },
            ),
        )
        .unwrap();

    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let candidate =
        PublicationCandidateWriter::write(&publication, &fixture(&publication), PROFILE).unwrap();
    let preflight = PublicationCandidatePreflight::for_candidate(
        &publication,
        &fixture(&publication),
        &candidate,
    );
    assert!(
        preflight.findings.iter().all(|finding| {
            finding.code != PublicationCandidateFindingCode::RequiredBleedMissing
        })
    );
}

#[test]
fn transparent_shape_with_only_an_inward_shadow_does_not_clear_required_bleed() {
    let mut document = Document::new(
        DocumentId::parse("inward-shadow-bleed-book").unwrap(),
        "Inward shadow bleed book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    document.pages[0].bleed_expectations.right = BleedExpectation::Required;
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add transparent edge shape",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Transparent panel".to_owned(),
                        size: PhysicalSize::new(19_075, 30_000).unwrap(),
                        transform: Transform::identity_at(200_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 30,
                                green: 60,
                                blue: 90,
                                alpha: 0,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::None,
                        },
                    },
                },
            ),
        )
        .unwrap();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Shift the only visible paint inward",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            color: Color {
                                red: 10,
                                green: 20,
                                blue: 30,
                                alpha: 255,
                            },
                            offset_x: Micrometres::new(-20_000),
                            offset_y: Micrometres::new(0),
                        }),
                        text_outline: None,
                        picture_border: None,
                    },
                },
            ),
        )
        .unwrap();

    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    assert!(
        preflight.findings.iter().any(|finding| {
            finding.code == PublicationCandidateFindingCode::RequiredBleedMissing
        })
    );
}

#[test]
fn whitespace_text_with_visible_effect_settings_does_not_clear_required_bleed() {
    let mut document = Document::new(
        DocumentId::parse("empty-text-bleed-book").unwrap(),
        "Empty text bleed book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    document.pages[0].bleed_expectations.right = BleedExpectation::Required;
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add whitespace text box across bleed",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Empty headline".to_owned(),
                        size: PhysicalSize::new(19_075, 30_000).unwrap(),
                        transform: Transform::identity_at(200_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "  \n  ".to_owned(),
                            style: TextStyle::default(),
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Style the empty headline",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            color: Color {
                                red: 10,
                                green: 20,
                                blue: 30,
                                alpha: 255,
                            },
                            offset_x: Micrometres::new(6_000),
                            offset_y: Micrometres::new(0),
                        }),
                        text_outline: Some(ShapeStroke {
                            color: Color {
                                red: 200,
                                green: 30,
                                blue: 40,
                                alpha: 255,
                            },
                            width_millipoints: 7_200,
                        }),
                        picture_border: None,
                    },
                },
            ),
        )
        .unwrap();

    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    assert!(
        publication.pages()[0].objects[0]
            .paint_regions()
            .unwrap()
            .is_empty()
    );
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    assert!(
        preflight.findings.iter().any(|finding| {
            finding.code == PublicationCandidateFindingCode::RequiredBleedMissing
        })
    );
}

#[test]
fn auto_bleed_warns_while_not_intended_stays_quiet() {
    let auto = bleed_preflight(BleedExpectation::Auto, "auto-bleed-book", 20_000);
    let warning = auto
        .findings
        .iter()
        .find(|finding| finding.code == PublicationCandidateFindingCode::PossibleBleedMissing)
        .expect("auto edge should ask the user to review bleed intent");
    assert_eq!(
        warning.target.as_ref().map(|target| target.action),
        Some(PublicationRepairAction::ReviewBleedIntent {
            edge: PublicationPageEdge::Right,
        })
    );

    let not_intended = bleed_preflight(BleedExpectation::NotIntended, "no-bleed-book", 20_000);
    assert!(not_intended.findings.iter().all(|finding| {
        !matches!(
            finding.code,
            PublicationCandidateFindingCode::PossibleBleedMissing
                | PublicationCandidateFindingCode::RequiredBleedMissing
        )
    }));

    let outside_page = bleed_preflight(BleedExpectation::Auto, "outside-page-book", -100_000);
    assert!(
        outside_page.findings.iter().all(|finding| {
            finding.code != PublicationCandidateFindingCode::PossibleBleedMissing
        })
    );
}

#[test]
fn overset_text_targets_the_exact_text_box() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = Document::new(
        DocumentId::parse("overset-book").unwrap(),
        "Overset book",
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
                "Add overset story",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: ObjectDraft {
                        name: "Overset story".to_owned(),
                        size: PhysicalSize::new(40_000, 8_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "This deliberately long story cannot fit inside its shallow text box."
                                .repeat(8),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "Exact test face".to_owned(),
                                font_size_millipoints: 18_000,
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    let object_id = session.document().pages[0].objects[0].id.clone();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    let finding = preflight
        .findings
        .iter()
        .find(|finding| finding.code == PublicationCandidateFindingCode::TextOverset)
        .expect("overset text should be a blocking preflight finding");
    let target = finding.target.as_ref().expect("overset repair target");
    assert_eq!(target.page_id, page_id.as_str());
    assert_eq!(target.object_id.as_deref(), Some(object_id.as_str()));
    assert_eq!(target.action, PublicationRepairAction::ResolveTextOverflow);
}

#[test]
fn unbound_inline_variants_emit_one_identical_font_blocker() {
    let document = Document::new(
        DocumentId::parse("unbound-inline-variant-book").unwrap(),
        "Unbound inline variant book",
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
                "Add unbound whitespace variants",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Unbound variants".to_owned(),
                        size: PhysicalSize::new(40_000, 20_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "  ".to_owned(),
                            style: TextStyle::default(),
                            frame: TextFrame::default(),
                            story: TextStoryFormatting {
                                inline_spans: vec![InlineTextSpan {
                                    range: TextSourceRange { start: 1, end: 2 },
                                    style: InlineTextStyle {
                                        font_size_millipoints: None,
                                        bold: Some(true),
                                        italic: None,
                                        ..InlineTextStyle::default()
                                    },
                                }],
                                paragraph_runs: Vec::new(),
                            },
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    let blockers = preflight
        .findings
        .iter()
        .filter(|finding| {
            finding.code == PublicationCandidateFindingCode::FontEmbeddingUnresolved
                && finding.facts
                    == PublicationFindingFacts::FontEmbedding {
                        state: PublicationFontEmbeddingState::Unbound,
                    }
        })
        .count();

    assert_eq!(
        blockers, 1,
        "hidden weight differences must not duplicate the same repair guidance"
    );
}

#[test]
fn overset_table_cell_identifies_its_row_and_column() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = Document::new(
        DocumentId::parse("overset-table-book").unwrap(),
        "Overset table book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(1, 2).unwrap();
    table.cells[1] = "MAGIC ".repeat(1_000);
    table.style.color.alpha = 120;
    table.style.font_face = Some(FontFaceBinding {
        source: source.clone(),
        face_index: 0,
    });
    table.style.font_family = "Exact table face".to_owned();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add word-search table",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(80_000, 20_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    let object_id = session.document().pages[0].objects[0].id.clone();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    assert!(!candidate.evidence().transparency.object_opacity);
    assert!(candidate.evidence().transparency.paint_alpha);
    assert!(!candidate.evidence().transparency.raster_soft_mask);
    assert!(preflight.findings.iter().all(|finding| {
        finding.code != PublicationCandidateFindingCode::TransparencyEvidenceMismatch
    }));
    let finding = preflight
        .findings
        .iter()
        .find(|finding| finding.code == PublicationCandidateFindingCode::TableCellOverset)
        .expect("clipped table text should be a blocking preflight finding");
    let target = finding.target.as_ref().expect("table-cell repair target");
    assert_eq!(target.page_id, page_id.as_str());
    assert_eq!(target.object_id.as_deref(), Some(object_id.as_str()));
    assert_eq!(
        target.action,
        PublicationRepairAction::ResolveTableCellOverflow { row: 0, column: 1 }
    );
    assert_eq!(
        finding.facts,
        PublicationFindingFacts::TableCell { row: 0, column: 1 }
    );
}

#[test]
fn revision_and_profile_drift_fail_before_a_candidate_is_returned() {
    let document = Document::new(
        DocumentId::parse("drift-book").unwrap(),
        "Drift book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let first = ResolvedPublication::resolve(&ProjectSnapshot {
        document: document.clone(),
        revision: Revision::initial(),
    })
    .unwrap();
    let fixture = fixture(&first);
    let mut session = DocumentSession::new(document).unwrap();
    let first_page = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add page",
                Command::AddPage {
                    after: Some(first_page),
                    trim: PhysicalSize::new(215_900, 279_400).unwrap(),
                    bleed: Insets::uniform(3_175).unwrap(),
                },
            ),
        )
        .unwrap();
    let changed = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let same_revision_other_scene = ResolvedPublication::resolve(&ProjectSnapshot {
        document: Document::new(
            DocumentId::parse("same-revision-other-book").unwrap(),
            "Different saved project",
            PhysicalSize::new(210_000, 297_000).unwrap(),
            Insets::uniform(3_000).unwrap(),
        )
        .unwrap(),
        revision: Revision::initial(),
    })
    .unwrap();

    assert_eq!(
        PublicationCandidateWriter::write(&changed, &fixture, PROFILE).unwrap_err(),
        PublicationCandidateError::RevisionMismatch
    );
    assert_eq!(
        PublicationCandidateWriter::write(&same_revision_other_scene, &fixture, PROFILE)
            .unwrap_err(),
        PublicationCandidateError::SceneBasisMismatch
    );
    let mut damaged_profile = PROFILE.to_vec();
    damaged_profile[100] ^= 1;
    assert!(matches!(
        PublicationCandidateWriter::write(&first, &fixture, &damaged_profile),
        Err(PublicationCandidateError::OutputProfile(_)
            | PublicationCandidateError::OutputProfileMismatch)
    ));
}

fn fixture(publication: &ResolvedPublication) -> FixtureContract {
    FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        inspect_output_intent(PROFILE).unwrap(),
        FontBoundary::NotExercised,
    )
}

fn full_font_resource() -> ExactFontResource {
    let mut bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let table_count = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let os2 = (0..table_count)
        .map(|index| 12 + index * 16)
        .find(|offset| bytes[*offset..*offset + 4] == *b"OS/2")
        .expect("fixture has OS/2 table");
    let table_offset = usize::try_from(u32::from_be_bytes([
        bytes[os2 + 8],
        bytes[os2 + 9],
        bytes[os2 + 10],
        bytes[os2 + 11],
    ]))
    .unwrap();
    let table_length = usize::try_from(u32::from_be_bytes([
        bytes[os2 + 12],
        bytes[os2 + 13],
        bytes[os2 + 14],
        bytes[os2 + 15],
    ]))
    .unwrap();
    let old_padded_end = table_offset + table_length.next_multiple_of(4);
    let new_length = 96_usize;
    let shift = new_length - table_length.next_multiple_of(4);
    bytes.splice(old_padded_end..old_padded_end, vec![0; shift]);
    bytes[os2 + 12..os2 + 16].copy_from_slice(&u32::try_from(new_length).unwrap().to_be_bytes());
    for index in 0..table_count {
        let record = 12 + index * 16;
        let offset = usize::try_from(u32::from_be_bytes([
            bytes[record + 8],
            bytes[record + 9],
            bytes[record + 10],
            bytes[record + 11],
        ]))
        .unwrap();
        if offset >= old_padded_end {
            bytes[record + 8..record + 12]
                .copy_from_slice(&u32::try_from(offset + shift).unwrap().to_be_bytes());
        }
    }
    bytes[table_offset..table_offset + 2].copy_from_slice(&3_u16.to_be_bytes());
    bytes[table_offset + 8..table_offset + 10].copy_from_slice(&0x0108_u16.to_be_bytes());
    ExactFontResource {
        source: AssetHash::for_bytes(&bytes),
        face_index: 0,
        bytes,
    }
}

fn bleed_preflight(
    expectation: BleedExpectation,
    document_id: &str,
    y: i64,
) -> PublicationCandidatePreflight {
    let mut document = Document::new(
        DocumentId::parse(document_id).unwrap(),
        "Bleed policy book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    document.pages[0].bleed_expectations.right = expectation;
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add trim-aligned panel",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Right edge panel".to_owned(),
                        size: PhysicalSize::new(15_900, 80_000).unwrap(),
                        transform: Transform::identity_at(200_000, y),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 30,
                                green: 80,
                                blue: 120,
                                alpha: 255,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::None,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let fixture = fixture(&publication);
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate)
}
