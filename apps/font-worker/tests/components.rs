//! Actual native preparation controls for unhinted TrueType components.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn run(name: &str) -> (Value, Option<Vec<u8>>) {
    run_with_subset_rights(name, false)
}
fn run_with_subset_rights(name: &str, subset: bool) -> (Value, Option<Vec<u8>>) {
    let compiled_fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tests/components");
    // Native Cargo uses its manifest directory. Cross-built tests run from the
    // source root, since the build host's manifest path is not a Windows path.
    let fixtures = if compiled_fixtures.is_dir() {
        compiled_fixtures
    } else {
        std::env::current_dir()
            .unwrap()
            .join("apps/font-worker/src/tests/components")
    };
    let mut source = fixtures.join(format!("{name}.ttf"));
    let mut bytes = std::fs::read(&source).unwrap();
    if subset {
        permit_subset(&mut bytes);
        source = std::env::temp_dir().join(format!(
            "pdf-library-subset-component-{}-{}.ttf",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&source, &bytes).unwrap();
    }
    let controls: Value =
        serde_json::from_slice(include_bytes!("../src/tests/components/provenance.json")).unwrap();
    let gid = u16::try_from(
        controls["controls"][format!("{name}.ttf")]["source_gid"]
            .as_u64()
            .unwrap(),
    )
    .unwrap();
    let output = std::env::temp_dir().join(format!(
        "pdf-library-components-{}-{}.font",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut glyphs = b"publisher-v2/font-glyph-set/v1\0".to_vec();
    glyphs.extend(1_u32.to_be_bytes());
    glyphs.extend(gid.to_be_bytes());
    let request = json!({"operation":"prepare","protocol_version":9,"font_path":source,"expected_source_sha256":hash(&bytes),"prepared_output_path":output,"preparation_policy":"publication","face_index":0,"glyph_ids":[gid],"expected_glyph_set_sha256":hash(&glyphs)});
    // The native replay runner stages the exact worker beside this test binary.
    let staged_worker = std::env::current_exe().unwrap().with_file_name(format!(
        "publisher-font-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let worker = if staged_worker.is_file() {
        staged_worker
    } else {
        PathBuf::from(env!("CARGO_BIN_EXE_publisher-font-worker"))
    };
    let mut command = Command::new(worker);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    if subset {
        std::fs::remove_file(source).unwrap();
    }
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    if !result.status.success() {
        assert!(!output.exists());
        return (response, None);
    }
    let prepared = std::fs::read(&output).unwrap();
    std::fs::remove_file(output).unwrap();
    if response["report"]["preparation_kind"] == json!("full") {
        assert_eq!(
            prepared, bytes,
            "rights-required full bytes must remain exact"
        );
    }
    assert_eq!(response["report"]["prepared_sha256"], hash(&prepared));
    (response["report"].clone(), Some(prepared))
}
fn permit_subset(bytes: &mut [u8]) {
    let word = |b: &[u8], at| u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
    let sum = |b: &[u8]| {
        b.chunks(4).fold(0_u32, |sum, chunk| {
            let mut value = [0; 4];
            value[..chunk.len()].copy_from_slice(chunk);
            sum.wrapping_add(u32::from_be_bytes(value))
        })
    };
    let count = usize::from(u16::from_be_bytes(bytes[4..6].try_into().unwrap()));
    let mut head = None;
    for i in 0..count {
        let at = 12 + i * 16;
        let offset = usize::try_from(word(bytes, at + 8)).unwrap();
        if &bytes[at..at + 4] == b"OS/2" {
            assert_eq!(&bytes[offset + 8..offset + 10], &0x0108_u16.to_be_bytes());
            bytes[offset + 8..offset + 10].fill(0);
        }
        if &bytes[at..at + 4] == b"head" {
            head = Some(offset);
            bytes[offset + 8..offset + 12].fill(0);
        }
    }
    for i in 0..count {
        let at = 12 + i * 16;
        let offset = usize::try_from(word(bytes, at + 8)).unwrap();
        let length = usize::try_from(word(bytes, at + 12)).unwrap();
        let checksum = sum(&bytes[offset..offset + length]);
        bytes[at + 4..at + 8].copy_from_slice(&checksum.to_be_bytes());
    }
    let head = head.unwrap();
    let adjustment = 0xb1b0_afba_u32.wrapping_sub(sum(bytes));
    bytes[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
    assert_eq!(sum(bytes), 0xb1b0_afba);
}
fn prepare(name: &str) -> (Value, Vec<u8>) {
    let (report, bytes) = run(name);
    assert!(bytes.is_some(), "{name}: {report}");
    (report, bytes.unwrap())
}
#[test]
fn unhinted_component_round_flag_preserves_fractional_design_offset() {
    let (clear, _) = prepare("scaled-offset-clear");
    let (round, _) = prepare("scaled-offset-round");
    assert_eq!(clear["pdf_metrics"]["cap_height"], json!(50.5));
    assert_eq!(round["pdf_metrics"], clear["pdf_metrics"]);
}
#[test]
fn point_matching_ignores_round_to_grid_flag() {
    let (clear, _) = prepare("point-match-clear");
    let (round, _) = prepare("point-match-round");
    assert_eq!(clear["pdf_metrics"]["cap_height"], json!(80.5));
    assert_eq!(round["pdf_metrics"], clear["pdf_metrics"]);
}
#[test]
fn child_horizontal_phantoms_are_valid_attachment_anchors() {
    for name in ["child-pp1", "child-pp2"] {
        let (report, _) = prepare(name);
        assert_eq!(report["pdf_metrics"]["widths"][3], json!(900.));
        assert_eq!(report["pdf_metrics"]["cap_height"], json!(100.));
    }
}
#[test]
fn use_my_metrics_selects_component_advance_and_origin() {
    let (plain, _) = prepare("metrics-unmarked");
    let (selected, _) = prepare("metrics-marked");
    assert_eq!(plain["pdf_metrics"]["widths"][3], json!(900.));
    assert_eq!(selected["pdf_metrics"]["widths"][3], json!(600.));
    assert_eq!(plain["pdf_metrics"]["bbox"][2], json!(150.));
    assert_eq!(selected["pdf_metrics"]["bbox"][2], json!(120.));
}
#[test]
fn inherited_metrics_remain_original_under_scale_translation_and_recursion() {
    for (name, cap) in [
        ("metrics-marked-scaled", 50.),
        ("metrics-marked-translated", 120.),
        ("metrics-marked-nested", 100.),
    ] {
        let (report, _) = prepare(name);
        let widths = report["pdf_metrics"]["widths"].as_array().unwrap();
        assert_eq!(widths.last().unwrap(), &json!(600.));
        assert_eq!(report["pdf_metrics"]["cap_height"], json!(cap));
    }
}
#[test]
fn parent_horizontal_phantoms_use_final_composite_point_numbers() {
    for (name, xmax) in [("parent-pp1", 120.), ("parent-pp2", 940.)] {
        let (report, _) = prepare(name);
        assert_eq!(report["pdf_metrics"]["bbox"][2], json!(xmax));
    }
    assert_eq!(
        prepare("child-pp2-scaled").0["pdf_metrics"]["cap_height"],
        json!(100.)
    );
}
#[test]
fn first_component_point_matching_remains_malformed_with_phantoms() {
    let (failure, prepared) = run("first-point-match-invalid");
    assert!(prepared.is_none());
    assert_eq!(failure["error"], json!("malformed-font"));
}
#[test]
fn component_metric_context_preserves_shear_and_positive_scales() {
    for name in ["metrics-marked-shear", "metrics-marked-unequal"] {
        assert_eq!(prepare(name).0["pdf_metrics"]["widths"][3], json!(600.));
    }
    for (marked, unmarked) in [
        ("metrics-marked-rotated", "metrics-unmarked-rotated"),
        ("metrics-marked-negative-x", "metrics-unmarked-negative-x"),
    ] {
        let (failure, bytes) = run(marked);
        assert!(bytes.is_none());
        assert_eq!(
            failure["error"],
            json!("unsupported-component-metric-context")
        );
        assert_eq!(prepare(unmarked).0["pdf_metrics"]["widths"][3], json!(900.));
    }
}
#[test]
fn vertical_anchor_unknown_coordinates_are_required_only_when_geometry_depends_on_them() {
    for name in ["vertical-unresolved-absent", "vertical-unresolved-paired"] {
        let (failure, bytes) = run(name);
        assert!(bytes.is_none());
        assert_eq!(
            failure["error"],
            json!("unsupported-vertical-phantom-context")
        );
    }
    for name in [
        "vertical-cancelled-x",
        "vertical-cancelled-both",
        "vertical-empty-unused",
        "vhea-only-unused",
    ] {
        assert!(prepare(name).0["pdf_metrics"]["cap_height"].is_number());
    }
    for name in ["empty-invalid-anchor", "vertical-pair-malformed"] {
        let (failure, bytes) = run(name);
        assert!(bytes.is_none());
        assert_eq!(failure["error"], json!("malformed-font"));
    }
}
#[test]
fn unused_source_context_is_deferred_but_all_full_retained_glyphs_are_checked() {
    let (subset, _) = prepare("unused-unresolved-subset");
    assert_eq!(subset["preparation_kind"], json!("subset"));
    assert_eq!(subset["pdf_metrics"]["cap_height"], json!(100.));
    let (full, bytes) = run("unused-unresolved-full");
    assert!(bytes.is_none());
    assert_eq!(full["error"], json!("unsupported-component-metric-context"));
    let (malformed, bytes) = run("unused-malformed-subset");
    assert!(bytes.is_none());
    assert_eq!(
        malformed["error"],
        json!("malformed-font"),
        "typed deferral cannot hide later invalid component indices"
    );
}
#[test]
fn unresolved_optional_cap_reference_is_never_an_empty_cap_proof() {
    let (unknown, _) = prepare("optional-h-unknown-subset");
    assert_eq!(unknown["requires_cap_height_fact"], json!(true));
    assert!(unknown["pdf_metrics"]["cap_height"].is_null());
    let (declared, _) = prepare("optional-h-declared-subset");
    assert_eq!(declared["requires_cap_height_fact"], json!(false));
    assert_eq!(declared["pdf_metrics"]["cap_height"], json!(700.));
}
#[test]
fn unsupported_vertical_table_version_is_only_needed_by_an_actual_anchor() {
    assert_eq!(
        prepare("vertical-version-unused").0["pdf_metrics"]["cap_height"],
        json!(100.)
    );
    let (failure, bytes) = run("vertical-version-needed");
    assert!(bytes.is_none());
    assert_eq!(
        failure["error"],
        json!("unsupported-vertical-metrics-version")
    );
}
#[test]
fn subset_rights_preserve_all_supported_full_component_counterparts() {
    let controls: Value =
        serde_json::from_slice(include_bytes!("../src/tests/components/provenance.json")).unwrap();
    let mut failures = Vec::new();
    let mut checked = 0;
    for (file, control) in controls["controls"].as_object().unwrap() {
        if control["expected_outcome"] != "prepared"
            || control["expected_preparation_kind"] != "full"
        {
            continue;
        }
        let name = file.strip_suffix(".ttf").unwrap();
        let full = prepare(name).0;
        let (subset, bytes) = run_with_subset_rights(name, true);
        checked += 1;
        if bytes.is_none() {
            failures.push(format!("{name}: {subset}"));
            continue;
        }
        assert_eq!(subset["preparation_kind"], "subset", "{name}");
        let gid = control["source_gid"].as_u64().unwrap();
        let cid = subset["remapped_glyphs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["source_glyph_id"] == gid)
            .unwrap()["cid"]
            .as_u64()
            .unwrap();
        assert_eq!(
            subset["pdf_metrics"]["widths"][usize::try_from(cid).unwrap()],
            full["pdf_metrics"]["widths"][usize::try_from(gid).unwrap()],
            "{name}"
        );
        assert_eq!(
            subset["pdf_metrics"]["cap_height"], full["pdf_metrics"]["cap_height"],
            "{name}"
        );
        assert_eq!(
            subset["requires_cap_height_fact"], full["requires_cap_height_fact"],
            "{name}"
        );
    }
    assert_eq!(checked, 23);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
