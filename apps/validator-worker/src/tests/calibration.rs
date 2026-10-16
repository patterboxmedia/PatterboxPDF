//! Exact local writer cases; this module is compiled only into Windows tests.

use std::{fs, path::Path};

use print_core::{
    ArtifactHash, EvaluationKey, IndependentValidator, PdfOxideValidator, PrintStandard,
    ScenePdfOptions, ScenePdfWriter, ValidationRequest, ValidationVerdict,
};
use serde_json::{Value, json};
use validator_boundary::writer_profile::audit_publisher_writer_output;

const PROFILE: &[u8] =
    include_bytes!("../../../../crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
const LICENSE: &[u8] =
    include_bytes!("../../../../crates/print-core/tests/fixtures/LICENSE-CC0-1.0.txt");

#[test]
#[ignore = "requires explicit retained original and SHA256, new output directory, source commit and packaged color/raster workers"]
fn retain_writer_output_intent_calibration_pair() {
    let original_path = std::path::PathBuf::from(
        std::env::var_os("PUBLISHER_CALIBRATION_ORIGINAL_PDF")
            .expect("explicit retained production Picture PDF"),
    );
    assert!(original_path.is_absolute());
    let original = fs::read(&original_path).expect("read retained original PDF");
    // Candidate identity includes the confined worker fingerprints. A new worker
    // can legitimately change the PDF identifier without changing its content.
    // Keep old corpora immutable; require an explicitly pinned current input and
    // matching retained manifest, then prove exact regeneration below.
    let retained_sha256 = std::env::var("PUBLISHER_CALIBRATION_ORIGINAL_SHA256")
        .expect("explicit retained production Picture SHA256");
    assert!(retained_sha256.len() == 64 && retained_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let original_manifest = fs::read(original_path.parent().unwrap().join("manifest.json"))
        .expect("read the retained source manifest");
    let original_provenance: Value = serde_json::from_slice(&original_manifest).unwrap();
    assert_eq!(
        ArtifactHash::of(&original).hex(),
        retained_sha256
    );
    assert!(original_provenance["cases"].as_array().unwrap().iter().any(|case|
        case["file"] == "production-picture-shadow.pdf" && case["sha256"] == retained_sha256));
    let regenerated = super::production_picture_shadow_candidate();
    assert!(
        regenerated.bytes() == original.as_slice(),
        "production PDF differs from retained original: retained={} generated={}",
        retained_sha256,
        ArtifactHash::of(regenerated.bytes()).hex()
    );
    let repeated = super::production_picture_shadow_candidate();
    assert!(
        repeated.bytes() == original.as_slice(),
        "repeat production PDF differs from retained original: retained={} generated={}",
        retained_sha256,
        ArtifactHash::of(repeated.bytes()).hex()
    );

    let (destination, source_commit) = calibration_destination();
    let (mutant, mut mutation) =
        replace_once(&original, "/DestOutputProfile", "/DestOutputProfild");
    let changed = original
        .iter()
        .zip(&mutant)
        .enumerate()
        .filter_map(|(offset, (before, after))| (before != after).then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(changed.len(), 1, "only one output-intent key byte changes");
    mutation["changed_byte_offsets"] = json!(changed);

    let mut control = retain_case(
        &destination,
        "production-picture-shadow",
        &original,
        None,
        &json!({"route": "PublicationCandidateWriter::write_with_resources",
            "fixture": "production_picture_shadow_candidate",
            "retained_original_path": original_path,
            "retained_original_sha256": retained_sha256,
            "retained_original_manifest_sha256": ArtifactHash::of(&original_manifest).hex(),
            "retained_original_source_commit": original_provenance["source_baseline_commit"]}),
    );
    assert_eq!(control["local_diagnostic"]["observed"], "passed");
    assert_eq!(control["local_diagnostic"]["findings"], json!([]));
    control["local_diagnostic"]["expectation"] = json!("passed with no findings");
    let mut variant = retain_case(
        &destination,
        "production-picture-shadow-missing-output-profile",
        &mutant,
        Some(("/DestOutputProfile", "required writer entry missing")),
        &json!({"base_case": "production-picture-shadow", "mutation": mutation}),
    );
    assert_eq!(variant["local_diagnostic"]["observed"], "failed");
    assert!(
        variant["local_diagnostic"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding
                .as_str()
                .unwrap_or_default()
                .contains("OutputIntent must contain a readable N=4 CMYK DestOutputProfile")),
        "missing targeted OutputIntent finding: {variant}"
    );
    variant["local_diagnostic"]["expectation"] =
        json!("failed with missing readable CMYK DestOutputProfile finding");
    variant["local_diagnostic"]["required_finding"] =
        json!("OutputIntent must contain a readable N=4 CMYK DestOutputProfile");
    fs::write(destination.join("LICENSE-CC0-1.0.txt"), LICENSE).unwrap();
    retain_manifest(
        &destination,
        &source_commit,
        "publisher-local-output-intent-calibration-v1",
        &json!({"artwork": "generated two-pixel RGBA Picture and hard shadow",
            "output_profile": "SWOP2006coated_300_mth.icc",
            "output_profile_sha256": ArtifactHash::of(PROFILE).hex(),
            "output_profile_license": "CC0-1.0",
            "license_file": "LICENSE-CC0-1.0.txt",
            "license_sha256": ArtifactHash::of(LICENSE).hex()}),
        &[control, variant],
    );
}

#[test]
#[ignore = "requires new absolute PUBLISHER_CALIBRATION_OUTPUT, PUBLISHER_CALIBRATION_SOURCE_COMMIT and packaged color/raster workers"]
fn retain_writer_transparency_calibration_bundle() {
    let (destination, source_commit) = calibration_destination();

    let publication = super::grouped_publication();
    let group = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    let repeated = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    assert_eq!(group.bytes(), repeated.bytes());
    let shadow = super::production_picture_shadow_candidate();
    assert_eq!(
        shadow.bytes(),
        super::production_picture_shadow_candidate().bytes()
    );
    assert!(String::from_utf8_lossy(group.bytes()).contains("/S /Transparency"));
    assert!(String::from_utf8_lossy(shadow.bytes()).contains("/S /Luminosity"));

    let (knockout, knockout_change) = replace_once(group.bytes(), "/K false", "/K true ");
    let (alpha, alpha_change) = replace_once(shadow.bytes(), "/S /Luminosity", "/S /Alpha     ");
    let cases = vec![
        retain_case(
            &destination,
            "grouped-scene",
            group.bytes(),
            None,
            &json!({"route": "ScenePdfWriter::write", "fixture": "grouped_publication",
                "scope": "actual scene bytes; no PDF/X metadata or output-intent envelope"}),
        ),
        retain_case(
            &destination,
            "grouped-scene-knockout",
            &knockout,
            Some(("/Group/K:", "outside the writer profile")),
            &json!({"base_case": "grouped-scene", "mutation": knockout_change}),
        ),
        retain_case(
            &destination,
            "production-picture-shadow",
            shadow.bytes(),
            None,
            &json!({"route": "PublicationCandidateWriter::write_with_resources",
                "fixture": "production_picture_shadow_candidate",
                "scope": "actual production candidate after confined raster/color preparation"}),
        ),
        retain_case(
            &destination,
            "production-picture-shadow-alpha",
            &alpha,
            Some(("/SMask/S:", "outside the writer profile")),
            &json!({"base_case": "production-picture-shadow", "mutation": alpha_change}),
        ),
    ];
    fs::write(destination.join("LICENSE-CC0-1.0.txt"), LICENSE).unwrap();
    retain_manifest(
        &destination,
        &source_commit,
        "publisher-local-transparency-calibration-v1",
        &json!({
            "artwork": "generated ellipse and two-pixel RGBA raster; no imported publication or fonts",
            "picture_rgba8": [255, 0, 0, 255, 0, 120, 255, 64],
            "output_profile": "SWOP2006coated_300_mth.icc",
            "output_profile_sha256": ArtifactHash::of(PROFILE).hex(),
            "output_profile_upstream_commit": "5a36861414d0b634d6f7a1d34ffa4dd2a895a92e",
            "output_profile_license": "CC0-1.0",
            "license_file": "LICENSE-CC0-1.0.txt",
            "license_sha256": ArtifactHash::of(LICENSE).hex()
        }),
        &cases,
    );
}

#[test]
#[ignore = "requires new absolute PUBLISHER_CALIBRATION_OUTPUT, PUBLISHER_CALIBRATION_SOURCE_COMMIT and packaged font worker"]
fn retain_writer_font_calibration_bundle() {
    let (destination, source_commit) = calibration_destination();
    let mut cases = Vec::new();
    let mut font_rights = Vec::new();
    let mut retained_hash_pairs = Vec::new();
    for (id, fixture, font, program_key, license_file, license) in [
        (
            "truetype-scene",
            "DejaVuSans.ttf",
            include_bytes!("../../../../crates/layout-core/tests/fixtures/DejaVuSans.ttf")
                .as_slice(),
            "/FontFile2",
            "LICENSE-DejaVu",
            include_bytes!("../../../../crates/layout-core/tests/fixtures/LICENSE-DejaVu")
                .as_slice(),
        ),
        (
            "cff-scene",
            "NotoSansCJKsc-Regular.otf",
            include_bytes!(
                "../../../../crates/layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf"
            )
            .as_slice(),
            "/FontFile3",
            "LICENSE-NotoSansCJK",
            include_bytes!("../../../../crates/layout-core/tests/fixtures/LICENSE-NotoSansCJK")
                .as_slice(),
        ),
    ] {
        let scene = super::actual_font_scene_candidate(font);
        assert_eq!(
            scene.bytes(),
            super::actual_font_scene_candidate(font).bytes()
        );
        let (missing, mutation, program_object) = missing_font_program(scene.bytes(), program_key);
        retained_hash_pairs.push((id, scene.bytes().to_vec(), missing.clone()));
        let mut control = retain_case(
            &destination,
            id,
            scene.bytes(),
            None,
            &json!({"route": "ScenePdfWriter::write_with_rasters_and_fonts",
                "fixture": "actual_font_scene_candidate", "font_fixture": fixture,
                "font_sha256": ArtifactHash::of(font).hex(), "font_program_key": program_key,
                "cff_program_shape": (id == "cff-scene").then_some("raw CID-keyed CFF 1; FontFile3 CIDFontType0C, no Length1; Type0 BaseFont adds Identity-H"),
                "preparation": "TrustedFontBoundary::resolve_packaged; actual confined preparation",
                "scope": "actual scene bytes; no PDF/X metadata or output-intent envelope"}),
        );
        require_font_control(&mut control);
        cases.push(control);
        cases.push(retain_case(
            &destination,
            &format!("{id}-missing-program"),
            &missing,
            Some(("/FontDescriptor", "writer embeds exactly one font program")),
            &json!({"base_case": id, "mutation": mutation,
                "scope": "descriptor reference removed; original unreferenced program stream retained"}),
        ));
        cases.extend(retain_font_semantic_variants(
            &destination,
            id,
            scene.bytes(),
            program_object,
        ));
        fs::write(destination.join(license_file), license).unwrap();
        font_rights.push(
            json!({"fixture": fixture, "source_sha256": ArtifactHash::of(font).hex(),
            "license_file": license_file, "license_sha256": ArtifactHash::of(license).hex()}),
        );
    }
    fs::write(destination.join("LICENSE-CC0-1.0.txt"), LICENSE).unwrap();
    retain_manifest(
        &destination,
        &source_commit,
        "publisher-local-font-calibration-v2",
        &json!({"content": "generated five-character Magic text; no imported publication",
            "fonts": font_rights,
            "fixture_provenance": "crates/layout-core/tests/fixtures/README.md",
            "fixture_provenance_sha256": ArtifactHash::of(include_bytes!(
                "../../../../crates/layout-core/tests/fixtures/README.md")).hex(),
            "diagnostic_profile_only": "SWOP2006coated_300_mth.icc; not embedded in either scene",
            "diagnostic_profile_sha256": ArtifactHash::of(PROFILE).hex(),
            "diagnostic_profile_license_file": "LICENSE-CC0-1.0.txt",
            "diagnostic_profile_license_sha256": ArtifactHash::of(LICENSE).hex()}),
        &cases,
    );
    for (id, original, missing) in retained_hash_pairs {
        eprintln!(
            "current {id} control={} missing-program={}",
            ArtifactHash::of(&original).hex(),
            ArtifactHash::of(&missing).hex()
        );
        assert_current_font_hashes(id, &original, &missing);
    }
}

fn missing_font_program(bytes: &[u8], program_key: &str) -> (Vec<u8>, Value, u32) {
    let text = String::from_utf8_lossy(bytes);
    let entries = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(&format!("{program_key} ")))
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "one emitted font-program reference");
    let fields = entries[0].split_ascii_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0], program_key);
    let object = fields[1].parse::<u32>().unwrap();
    assert!(object > 0);
    assert_eq!(&fields[2..], &["0", "R"]);
    let (missing, mutation) = replace_once(bytes, entries[0], &" ".repeat(entries[0].len()));
    (missing, mutation, object)
}

fn assert_current_font_hashes(id: &str, original: &[u8], missing: &[u8]) {
    // The September 30 writer adds per-run paint and balanced object state.
    // These current controls retain the same exact font programs and geometry;
    // historical September 26 controls remain separately pinned and preserved.
    let (original_hash, missing_hash) = match id {
        "truetype-scene" => (
            "1735202fd1227b2b0a9076f3a7e726e25377728ddd7f39e9b49f784d26f9a386",
            "0a7e8eae9908cc7845d9ff0439cf0ace03b0ab41b599e34a10a6c82c5b0499cf",
        ),
        "cff-scene" => (
            "16c3b93c0ee24ac65b4d852aa271729a9dc51948fd844f497cd0415f383652dd",
            "43fbdff55b2fa2d5737a5c3bf8c3ff796b36d6e2c4b290115d1daf7412cd03e4",
        ),
        _ => unreachable!("two retained font scenes"),
    };
    assert_eq!(ArtifactHash::of(original).hex(), original_hash);
    assert_eq!(ArtifactHash::of(missing).hex(), missing_hash);
}

fn require_font_control(case: &mut Value) {
    let diagnostic = &mut case["local_diagnostic"];
    let font_markers = ["[XFONT-", "font /"];
    assert!(
        diagnostic["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| {
                let finding = finding.as_str().unwrap();
                font_markers.iter().all(|marker| !finding.contains(marker))
            }),
        "original writer scene has a font finding: {diagnostic}"
    );
    diagnostic["expectation"] =
        json!("font checks clear; PDF/X envelope is absent; no independent or normative verdict");
    diagnostic["required_absent_font_markers"] = json!(font_markers);
}

fn retain_font_semantic_variants(
    destination: &Path,
    id: &str,
    original: &[u8],
    program_object: u32,
) -> [Value; 2] {
    let (signature, corrupt_signature, program_reason, missing_glyph) = match id {
        "truetype-scene" => (
            "\0\x01\0\0",
            "\0\0\0\0",
            "embedded TrueType font cannot be parsed",
            "uses CID 65535, which is absent from its embedded program",
        ),
        "cff-scene" => (
            "\x01\0\x04",
            "\x01\0\0",
            "CFF header or indexes are malformed",
            "uses CID 65535, which is absent from its embedded CFF program",
        ),
        _ => unreachable!("two retained font scenes"),
    };
    let (program_range, _) = emitted_stream(original, program_object);
    let (corrupt, program_change) = replace_once(
        original,
        &format!("stream\n{signature}"),
        &format!("stream\n{corrupt_signature}"),
    );
    assert_eq!(
        usize::try_from(program_change["byte_offset"].as_u64().unwrap()).unwrap()
            + "stream\n".len(),
        program_range.start,
        "signature mutation belongs to the descriptor's exact program object"
    );
    let mut corrupt_case = retain_case(
        destination,
        &format!("{id}-corrupt-program"),
        &corrupt,
        None,
        &json!({"base_case": id, "mutation": program_change,
            "target_object": program_object, "stream_range": [program_range.start, program_range.end],
            "scope": "embedded program header only; no table or CharString coverage"}),
    );
    require_font_diagnostic(
        &mut corrupt_case,
        "font /PV2Font",
        &["no readable embedded font program", program_reason],
    );

    // These hash-pinned scenes invoke their only text Form as /PV2Object0.
    assert_eq!(
        original
            .windows(b"/PV2Object0 5 0 R".len())
            .filter(|w| *w == b"/PV2Object0 5 0 R")
            .count(),
        1
    );
    let (page_range, _) = emitted_stream(original, 4);
    assert!(
        original[page_range]
            .windows(b"/PV2Object0 Do".len())
            .any(|w| w == b"/PV2Object0 Do")
    );
    let (form_range, form_header) = emitted_stream(original, 5);
    assert!(form_header.contains("/Subtype /Form"));
    let before = "(\\000\\001) Tj";
    let (missing, glyph_change) = replace_once(original, before, "(\\377\\377) Tj");
    let offset = usize::try_from(glyph_change["byte_offset"].as_u64().unwrap()).unwrap();
    assert!(offset >= form_range.start && offset + before.len() <= form_range.end);
    let mut glyph_case = retain_case(
        destination,
        &format!("{id}-missing-used-glyph"),
        &missing,
        None,
        &json!({"base_case": id, "mutation": glyph_change,
            "target_object": 5, "stream_range": [form_range.start, form_range.end],
            "scope": "first shown CID 1 becomes absent CID 65535; font program and ActualText unchanged"}),
    );
    require_font_diagnostic(
        &mut glyph_case,
        "form /PV2Object0 font /PV2Font",
        &[missing_glyph],
    );
    [corrupt_case, glyph_case]
}

pub(super) fn emitted_stream(bytes: &[u8], object: u32) -> (std::ops::Range<usize>, &str) {
    // Exact raw pdf-writer fixture syntax, never a general PDF parsing route.
    let marker = format!("\n{object} 0 obj\n<<\n");
    let starts = bytes
        .windows(marker.len())
        .enumerate()
        .filter_map(|(index, value)| (value == marker.as_bytes()).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1);
    let object_start = starts[0];
    let header_limit = (object_start + 4096).min(bytes.len());
    let header_end = bytes[object_start..header_limit]
        .windows(b">>\nstream\n".len())
        .position(|value| value == b">>\nstream\n")
        .unwrap();
    let stream_start = object_start + header_end + b">>\nstream\n".len();
    let header = std::str::from_utf8(&bytes[object_start..stream_start]).unwrap();
    assert!(!header.contains("endobj"));
    let lengths = header
        .lines()
        .filter_map(|line| line.trim().strip_prefix("/Length "))
        .map(|length| length.parse::<usize>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lengths.len(), 1);
    let stream_end = stream_start.checked_add(lengths[0]).unwrap();
    assert!(bytes[stream_end..].starts_with(b"\nendstream\nendobj\n"));
    (stream_start..stream_end, header)
}

fn require_font_diagnostic(case: &mut Value, owner: &str, reasons: &[&str]) {
    let diagnostic = &mut case["local_diagnostic"];
    assert_eq!(diagnostic["observed"], "failed");
    for reason in reasons {
        assert!(
            diagnostic["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|finding| {
                    let finding = finding.as_str().unwrap();
                    finding.contains(owner) && finding.contains(reason)
                }),
            "missing targeted font finding: {owner}: {reason}: {diagnostic}"
        );
    }
    diagnostic["expectation"] = json!(
        "failed with targeted local font findings; missing PDF/X envelope remains separate; no independent or normative verdict"
    );
    diagnostic["required_font_owner"] = json!(owner);
    diagnostic["required_font_reasons"] = json!(reasons);
}

fn calibration_destination() -> (std::path::PathBuf, String) {
    let destination = std::path::PathBuf::from(
        std::env::var_os("PUBLISHER_CALIBRATION_OUTPUT").expect("explicit new output directory"),
    );
    assert!(destination.is_absolute());
    let source_commit = std::env::var("PUBLISHER_CALIBRATION_SOURCE_COMMIT").unwrap();
    assert!(source_commit.len() == 40 && source_commit.bytes().all(|b| b.is_ascii_hexdigit()));
    fs::create_dir(&destination).expect("exclusive new calibration directory");
    (destination, source_commit)
}

fn retain_manifest(
    destination: &Path,
    source_commit: &str,
    bundle_format: &str,
    rights: &Value,
    cases: &[Value],
) {
    let manifest = json!({
        "format": bundle_format,
        "source_baseline_commit": source_commit,
        "source_scope": "baseline plus the working-tree changes bound by the associated build and run receipts",
        "fixture_source_sha256": ArtifactHash::of(include_bytes!("../main.rs")).hex(),
        "exporter_source_sha256": ArtifactHash::of(include_bytes!("calibration.rs")).hex(),
        "authority_manifest_sha256": ArtifactHash::of(include_bytes!(
            "../../../../crates/validator-boundary/rules/pdfx4-authority-v1.json")).hex(),
        "rights": rights,
        "independent_calibration": "pending; no external checker used",
        "normative_coverage": "incomplete; all normative verdicts pending",
        "professional_export": "locked; these QA bytes are not export authorization",
        "cases": cases
    });
    fs::write(
        destination.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    println!(
        "Retained local calibration bundle: {}",
        destination.display()
    );
}

fn replace_once(original: &[u8], before: &str, after: &str) -> (Vec<u8>, Value) {
    assert_eq!(
        before.len(),
        after.len(),
        "retain offsets and stream lengths"
    );
    let offsets = original
        .windows(before.len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == before.as_bytes()).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(
        offsets.len(),
        1,
        "mutation must have one exact source entry"
    );
    let offset = offsets[0];
    let end = offset + before.len();
    let mut mutated = original.to_vec();
    mutated[offset..end].copy_from_slice(after.as_bytes());
    assert_eq!(&mutated[..offset], &original[..offset]);
    assert_eq!(&mutated[end..], &original[end..]);
    (
        mutated,
        json!({"byte_offset": offset, "before": before, "after": after,
            "base_sha256": ArtifactHash::of(original).hex(), "length_preserved": true}),
    )
}

fn retain_case(
    destination: &Path,
    id: &str,
    bytes: &[u8],
    rejection: Option<(&str, &str)>,
    provenance: &Value,
) -> Value {
    let file = format!("{id}.pdf");
    let path = destination.join(&file);
    fs::write(&path, bytes).unwrap();
    let retained = fs::read(&path).unwrap();
    assert_eq!(retained, bytes);
    let hash = ArtifactHash::of(&retained);
    let audit = audit_publisher_writer_output(&retained).unwrap();
    assert_eq!(audit.artifact_sha256, hash.bytes());
    let profile_expectation = if let Some((key, reason)) = rejection {
        assert!(
            audit
                .findings
                .iter()
                .any(|finding| { finding.contains(key) && finding.contains(reason) }),
            "{id}: {:?}",
            audit.findings
        );
        "refused"
    } else {
        assert!(audit.findings.is_empty(), "{id}: {:?}", audit.findings);
        "accepted"
    };
    let diagnostic = PdfOxideValidator::new(ArtifactHash::of(PROFILE));
    let identity = diagnostic.identity();
    let result = diagnostic
        .validate(ValidationRequest::new(
            EvaluationKey::from_bytes(hash.bytes()),
            &retained,
            PrintStandard::PdfX4,
        ))
        .unwrap();
    assert_eq!(ArtifactHash::of(&fs::read(path).unwrap()), hash);
    json!({
        "id": id, "file": file, "bytes": retained.len(), "sha256": hash.hex(),
        "provenance": provenance,
        "writer_profile": {
            "expected": profile_expectation, "observed": profile_expectation,
            "required_rejection_key": rejection.map(|(key, _)| key),
            "required_rejection_reason": rejection.map(|(_, reason)| reason),
            "findings": audit.findings,
            "version": validator_boundary::VALIDATOR_VERSION
        },
        "local_diagnostic": {
            "expectation": "observe only; not an independent or normative verdict",
            "observed": match result.verdict {
                ValidationVerdict::Passed => "passed", ValidationVerdict::Failed => "failed"
            },
            "findings": result.findings, "artifact_sha256": hash.hex(),
            "name": identity.name, "version": identity.version,
            "source_sha256": identity.executable_or_source_hash.hex(),
            "rules_sha256": identity.rules_hash.hex(), "profile_sha256": identity.profile_hash.hex()
        },
        "independent_checker_verdict": "pending",
        "normative_verdict": "pending"
    })
}
