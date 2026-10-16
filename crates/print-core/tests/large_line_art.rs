use document_core::raster_capacity::production_raster_pixels;

#[test]
fn letter_1200_with_bleed_has_a_finite_portrait_and_landscape_limit() {
    assert_eq!(production_raster_pixels(10_500, 13_500), Ok(141_750_000));
    assert_eq!(production_raster_pixels(13_500, 10_500), Ok(141_750_000));
    assert!(production_raster_pixels(10_501, 13_500).is_err());
    assert!(production_raster_pixels(13_501, 1).is_err());
}

#[cfg(windows)]
mod native {
    use color_boundary::TrustedColorBoundary;
    use document_core::model::{
        DocumentId, ImageContentKind, ImageCrop, ImageFlip, Insets, ObjectDraft, ObjectKind,
        PhysicalSize, Transform, WrapMode,
    };
    use document_core::raster_capacity::{
        MAX_RETAINED_RASTER_BYTES, RasterMemoryPermit, RasterWorkPermit,
    };
    use document_core::{
        AssetHash, AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot,
        Transaction, UntaggedRasterPolicy, inspect_raster, open_package, save_new_package,
    };
    use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
    use layout_core::ResolvedPublication;
    use print_core::{
        FixtureContract, FontBoundary, PreflightBasis, PublicationCandidatePreflight,
        PublicationCandidateWriter, PublicationGate, PublicationReleaseBlocker, ScenePdfOptions,
        ScenePdfWriter, inspect_output_intent, prepare_publication_rasters,
    };
    use raster_boundary::{RasterWorkerError, TrustedRasterBoundary};
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use validator_boundary::{
        StructuralCheckStatus, TrustedValidationVerdict, TrustedValidatorBoundary,
        ValidatorRuleCoverageStatus,
    };

    const WIDTH: u32 = 10_500;
    const HEIGHT: u32 = 13_500;
    const OUTPUT: &[u8] = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");

    fn input_source() -> (Vec<u8>, [u8; 32], [u8; 32]) {
        let Some(directory) = std::env::var_os("PUBLISHER_LARGE_RASTER_SOURCE") else {
            return source();
        };
        let directory = std::path::PathBuf::from(directory);
        let facts: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("pixel-proof.json")).unwrap())
                .unwrap();
        let png = std::fs::read(directory.join("letter-1200-bleed.png")).unwrap();
        assert_eq!(
            AssetHash::for_bytes(&png).as_str(),
            facts["sourceHash"].as_str().unwrap()
        );
        let parse = |key: &str| {
            let value = facts[key].as_str().unwrap();
            assert_eq!(value.len(), 64);
            std::array::from_fn(|index| {
                u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap()
            })
        };
        (png, parse("decodedRgbHash"), parse("sourceProfileHash"))
    }

    fn source() -> (Vec<u8>, [u8; 32], [u8; 32]) {
        let mut rgb =
            vec![255_u8; usize::try_from(u64::from(WIDTH) * u64::from(HEIGHT) * 3).unwrap()];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if x % 1_200 < 8
                    || y % 1_200 < 8
                    || (x / 1_200 + y / 1_200) % 2 == 0 && x % 1_200 < 300 && y % 1_200 < 300
                {
                    let index =
                        usize::try_from((u64::from(y) * u64::from(WIDTH) + u64::from(x)) * 3)
                            .unwrap();
                    rgb[index..index + 3].fill(0);
                }
            }
        }
        let raw_hash = Sha256::digest(&rgb).into();
        let profile = lcms2::Profile::new_srgb().icc().unwrap();
        let profile_hash = Sha256::digest(&profile).into();
        let mut png = Vec::new();
        let mut encoder = PngEncoder::new(&mut png);
        encoder.set_icc_profile(profile).unwrap();
        encoder
            .write_image(&rgb, WIDTH, HEIGHT, ExtendedColorType::Rgb8)
            .unwrap();
        (png, raw_hash, profile_hash)
    }

    // Parse only the emitted writer image dictionary; independently inflate its
    // exact stream in bounded chunks and compare the attested full CMYK plane.
    fn image_pixel_proof(pdf: &[u8]) -> (usize, [u8; 32]) {
        let image = pdf
            .windows(b"/Subtype /Image".len())
            .position(|window| window == b"/Subtype /Image")
            .unwrap();
        let marker = b"stream\n";
        let stream = image
            + pdf[image..]
                .windows(marker.len())
                .position(|window| window == marker)
                .unwrap();
        let dictionary_start = pdf[..image]
            .windows(2)
            .rposition(|window| window == b"<<")
            .unwrap();
        let dictionary = std::str::from_utf8(&pdf[dictionary_start..stream]).unwrap();
        assert!(dictionary.contains("/Width 10500") && dictionary.contains("/Height 13500"));
        assert!(
            dictionary.contains("/BitsPerComponent 8")
                && dictionary.contains("/ColorSpace /DeviceCMYK")
        );
        assert!(dictionary.contains("/Filter /FlateDecode"));
        let length = dictionary
            .split("/Length ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let start = stream + marker.len();
        let maximum = usize::try_from(u64::from(WIDTH) * u64::from(HEIGHT) * 4).unwrap();
        let mut decoded =
            flate2::read::ZlibDecoder::new(&pdf[start..start + length]).take((maximum + 1) as u64);
        let mut hash = Sha256::new();
        let mut count = 0;
        let mut buffer = vec![0_u8; 65_536];
        loop {
            let read = decoded.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            count += read;
            hash.update(&buffer[..read]);
        }
        assert_eq!(count, maximum);
        (count, hash.finalize().into())
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One real full-size input binds confined workers, source preservation, PDF pixels and refusal recovery.
    fn full_bleed_letter_1200_keeps_every_source_and_output_pixel() {
        let folder = std::env::var_os("PUBLISHER_LARGE_RASTER_EVIDENCE").map_or_else(
            || std::env::temp_dir().join(format!("publisher-large-raster-{}", std::process::id())),
            std::path::PathBuf::from,
        );
        std::fs::create_dir(&folder).unwrap();
        let (png, raw_hash, profile_hash) = input_source();
        std::fs::write(folder.join("letter-1200-bleed.png"), &png).unwrap();
        let asset = AssetHash::for_bytes(&png);
        let inspection = inspect_raster(&png).unwrap();
        assert_eq!(
            (
                inspection.oriented_width_pixels,
                inspection.oriented_height_pixels
            ),
            (WIDTH, HEIGHT)
        );
        let boundary = TrustedRasterBoundary::resolve_packaged();
        let owner = RasterWorkPermit::acquire().unwrap();
        assert!(matches!(
            boundary.decode(&png, &asset, UntaggedRasterPolicy::RequireEmbeddedProfile),
            Err(RasterWorkerError::ResourceLimit)
        ));
        drop(owner);
        let memory = RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES).unwrap();
        assert!(matches!(
            boundary.decode(&png, &asset, UntaggedRasterPolicy::RequireEmbeddedProfile),
            Err(RasterWorkerError::ResourceLimit)
        ));
        drop(memory);
        let mut assets = AssetStore::default();
        assets.insert_raster(png, inspection);
        let mut session = DocumentSession::new(
            Document::new(
                DocumentId::parse("letter-line-art").unwrap(),
                "Letter 1200 ppi line art",
                PhysicalSize::new(215_900, 279_400).unwrap(),
                Insets::uniform(3_175).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let page = session.document().pages[0].id.clone();
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Full bleed line art",
                    Command::AddObject {
                        location: ObjectLocation::Page(page),
                        object: ObjectDraft {
                            name: "1200 ppi line art".into(),
                            size: PhysicalSize::new(222_250, 285_750).unwrap(),
                            transform: Transform::identity_at(-3_175, -3_175),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Image {
                                asset: asset.clone(),
                                content_kind: ImageContentKind::LineArt,
                                crop: ImageCrop::default(),
                                flip: ImageFlip::default(),
                                wrap_mode: WrapMode::None,
                                lock_aspect_ratio: true,
                            },
                        },
                    },
                ),
            )
            .unwrap();
        let project = ProjectSnapshot::from_session(&session);
        let publication = ResolvedPublication::resolve(&project).unwrap();
        let started = std::time::Instant::now();
        let rasters = prepare_publication_rasters(
            &publication,
            &assets,
            OUTPUT,
            &boundary,
            &TrustedColorBoundary::resolve_packaged(),
        )
        .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(rasters.len(), 1);
        let raster = rasters[0].evidence();
        assert_eq!((raster.width, raster.height), (WIDTH, HEIGHT));
        assert_eq!(raster.decoded_rgb_hash, raw_hash);
        assert_eq!(raster.source_profile_hash, profile_hash);
        assert!(!raster.has_transparency);
        let pdf =
            ScenePdfWriter::write_with_rasters(&publication, ScenePdfOptions::default(), &rasters)
                .unwrap();
        assert!(pdf.bytes().len() < 256 * 1024 * 1024);
        assert!(!pdf.bytes().windows(6).any(|window| window == b"/SMask"));
        let (decoded_bytes, output_hash) = image_pixel_proof(pdf.bytes());
        assert_eq!(output_hash, raster.converted_cmyk_hash);
        assert_eq!(pdf.evidence().raster_placements[0].raster, *raster);
        std::fs::write(folder.join("letter-1200-bleed.pdf"), pdf.bytes()).unwrap();
        let source = folder.join("letter-1200-bleed.publisher-v2");
        save_new_package(&source, &project, &assets).unwrap();
        let reopened = open_package(&source).unwrap();
        assert_eq!(reopened.snapshot, project);
        assert_eq!(
            reopened.assets.get(&asset).unwrap(),
            assets.get(&asset).unwrap()
        );
        let facts = serde_json::json!({"sourceWidth":WIDTH,"sourceHeight":HEIGHT,"sourcePixels":141_750_000,
            "sourceHash":asset.as_str(),"decodedRgbHash":hash_hex(&raw_hash),
            "sourceProfileHash":hash_hex(&profile_hash),"decodedCmykBytes":decoded_bytes,
            "convertedCmykHash":hash_hex(&output_hash),"pdfBytes":pdf.bytes().len(),
            "preparationMillis":elapsed.as_millis(),"crop":pdf.evidence().raster_placements[0].crop,
            "transform":pdf.evidence().raster_placements[0].transform,
            "scope":"Full source/output pixels, source profile, confined preparation, budget refusal and exact reopen; no PDF/X conformance claim"});
        std::fs::write(
            folder.join("pixel-proof.json"),
            serde_json::to_vec_pretty(&facts).unwrap(),
        )
        .unwrap();
        // The scene-only witness above intentionally lacks a PDF/X envelope.
        // Exercise the actual candidate writer using the same opaque prepared
        // raster, selected profile and canonical snapshot, never patched PDF metadata.
        let contract = FixtureContract::new(
            PreflightBasis::from(publication.clone()),
            inspect_output_intent(OUTPUT).unwrap(),
            FontBoundary::NotExercised,
        );
        let candidate = PublicationCandidateWriter::write_with_resources(
            &publication,
            &contract,
            OUTPUT,
            &rasters,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            image_pixel_proof(candidate.bytes()),
            (decoded_bytes, output_hash)
        );
        std::fs::write(
            folder.join("letter-1200-bleed-production.pdf"),
            candidate.bytes(),
        )
        .unwrap();
        let preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
            &publication,
            &contract,
            &candidate,
            &[],
        );
        assert!(!preflight.has_blockers(), "{:?}", preflight.findings);
        let checked = TrustedValidatorBoundary::resolve_packaged()
            .validate(
                candidate.bytes(),
                preflight.evaluation_key.bytes(),
                preflight.export_identity.bytes(),
                preflight.profile_hash.bytes(),
            )
            .unwrap();
        let gate = PublicationGate::evaluate(&preflight, Some(&checked));
        std::fs::write(folder.join("production-validation.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "scope":"Actual PublicationCandidateWriter from exact canonical source and trusted raster; no conformance claim",
            "candidateSha256":hash_hex(&candidate.artifact_hash().bytes()), "candidateBytes":candidate.bytes().len(),
            "sceneOnlySha256":hash_hex(&Sha256::digest(pdf.bytes()).into()),
            "profileHash":hash_hex(&preflight.profile_hash.bytes()), "evaluationHash":hash_hex(&preflight.evaluation_key.bytes()),
            "capsuleHash":hash_hex(&preflight.export_identity.bytes()), "workerHash":hash_hex(&checked.worker_executable_hash()),
            "diagnosticVerdict":format!("{:?}",checked.verdict()), "structuralCheck":format!("{:?}",checked.structural_check_status()),
            "findings":checked.findings(), "ruleCoverage":format!("{:?}",checked.rule_coverage_status()),
            "validatorMiB":768,"qpdfAfterSuccessfulWorkerMiB":2048,"qpdfAfterRefusalMiB":768,
            "order":"closed profile/exact samples and diagnostics first; then genuine qpdf--check with warnings",
            "editingScenario":"Large validation is separate from the ordinary 1GiB editing scenario",
            "gateAllowed":gate.allowed,"authorizationAvailable":gate.authorization().is_some(),"releaseBlockers":format!("{:?}",gate.release_blockers()),
        })).unwrap()).unwrap();
        assert_eq!(
            checked.verdict(),
            TrustedValidationVerdict::Passed,
            "{:?}",
            checked.findings()
        );
        assert_eq!(
            checked.structural_check_status(),
            StructuralCheckStatus::Passed
        );
        assert_eq!(
            checked.rule_coverage_status(),
            ValidatorRuleCoverageStatus::Incomplete
        );
        assert!(!gate.allowed && gate.authorization().is_none());
        assert!(
            gate.release_blockers()
                .contains(&PublicationReleaseBlocker::ValidatorRuleCoverageNotReleaseProven)
        );
    }

    fn hash_hex(bytes: &[u8; 32]) -> String {
        use std::fmt::Write;
        bytes
            .iter()
            .fold(String::with_capacity(64), |mut output, byte| {
                write!(output, "{byte:02x}").unwrap();
                output
            })
    }
}
