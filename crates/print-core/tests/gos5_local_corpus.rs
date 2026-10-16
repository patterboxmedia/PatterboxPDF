#![cfg(feature = "pdf-oxide-validator")]

use std::{collections::HashSet, env, fs};

use pdf_oxide::document::PdfDocument;
use pdf_oxide::object::{Object, ObjectRef};
use print_core::{
    ArtifactHash, EvaluationKey, IndependentValidator, PdfOxideValidator, PrintStandard,
    ValidationRequest, ValidationVerdict, audit_active_icc_profiles, audit_public_pdfx_metadata,
    audit_public_pdfx_objects,
};

const GOS5_PDF_ENV: &str = "PUBLISHER_GOS5_PDF";
const GOS5_X4_SHA256: &str = "6D9686303EA9049350BC1B3253C22A3CC8578BD7DFCC6AFEAEF89CBBE37D4CBE";
const MAX_PRINTED_FINDINGS_PER_COMPONENT: usize = 20;

#[derive(Debug, Default)]
struct FeatureCensus {
    soft_masks: usize,
    transparency_groups: usize,
    non_normal_blend_modes: usize,
    sixteen_bit_images: usize,
    jpx_streams: usize,
    reachable_non_jpx_indexed_image_xobject_streams: usize,
}

fn print_findings(label: &str, findings: &[String]) {
    for finding in findings.iter().take(MAX_PRINTED_FINDINGS_PER_COMPONENT) {
        eprintln!("{label}: {finding}");
    }
    if findings.len() > MAX_PRINTED_FINDINGS_PER_COMPONENT {
        eprintln!(
            "{label}: ... {} more bounded findings omitted",
            findings.len() - MAX_PRINTED_FINDINGS_PER_COMPONENT
        );
    }
}

fn census_document(document: &PdfDocument) -> FeatureCensus {
    let mut census = FeatureCensus::default();
    let mut visited = HashSet::<ObjectRef>::new();
    let mut pending = vec![document.trailer().clone()];
    while let Some(object) = pending.pop() {
        match object {
            Object::Reference(reference) => {
                if visited.insert(reference) {
                    pending.push(
                        document
                            .load_object(reference)
                            .expect("referenced object loads"),
                    );
                }
            }
            Object::Array(values) => pending.extend(values),
            Object::Dictionary(dictionary) => {
                inspect_feature_dictionary(document, &dictionary, &mut census);
                pending.extend(dictionary.into_values());
            }
            Object::Stream { dict, .. } => {
                inspect_feature_dictionary(document, &dict, &mut census);
                pending.extend(dict.into_values());
            }
            Object::Null
            | Object::Boolean(_)
            | Object::Integer(_)
            | Object::Real(_)
            | Object::String(_)
            | Object::Name(_) => {}
        }
    }
    census
}

fn inspect_feature_dictionary(
    document: &PdfDocument,
    dictionary: &std::collections::HashMap<String, Object>,
    census: &mut FeatureCensus,
) {
    let type_name = dictionary.get("Type").and_then(Object::as_name);
    let subtype = dictionary.get("Subtype").and_then(Object::as_name);
    if dictionary
        .get("SMask")
        .is_some_and(|mask| mask.as_name() != Some("None"))
    {
        census.soft_masks += 1;
    }
    if type_name == Some("Group")
        && dictionary.get("S").and_then(Object::as_name) == Some("Transparency")
    {
        census.transparency_groups += 1;
    }
    if dictionary
        .get("BM")
        .is_some_and(contains_non_normal_blend_mode)
    {
        census.non_normal_blend_modes += 1;
    }
    if subtype == Some("Image")
        && dictionary
            .get("BitsPerComponent")
            .and_then(Object::as_integer)
            == Some(16)
    {
        census.sixteen_bit_images += 1;
    }
    if dictionary.get("Filter").is_some_and(contains_jpx_filter) {
        census.jpx_streams += 1;
    }
    if subtype == Some("Image")
        && dictionary.get("ColorSpace").is_some_and(|color_space| {
            reachable_image_color_space_is_indexed(document, color_space)
        })
        && !dictionary
            .get("Filter")
            .is_some_and(|filter| object_contains_jpx_filter(document, filter, &mut HashSet::new()))
    {
        census.reachable_non_jpx_indexed_image_xobject_streams += 1;
    }
}

fn reachable_image_color_space_is_indexed(document: &PdfDocument, color_space: &Object) -> bool {
    let mut color_space = color_space.clone();
    let mut visited = HashSet::<ObjectRef>::new();
    loop {
        match color_space {
            Object::Reference(reference) => {
                assert!(
                    visited.insert(reference),
                    "GOS5 image ColorSpace contains an indirect-reference cycle"
                );
                color_space = document
                    .load_object(reference)
                    .expect("GOS5 image ColorSpace reference loads");
            }
            Object::Array(values) => {
                return values.first().and_then(Object::as_name) == Some("Indexed");
            }
            Object::Name(name)
                if matches!(
                    name.as_str(),
                    "DeviceGray" | "G" | "DeviceRGB" | "RGB" | "DeviceCMYK" | "CMYK"
                ) =>
            {
                return false;
            }
            Object::Name(name) => panic!(
                "GOS5 reachable-graph census cannot classify named image ColorSpace alias /{name} without a resource scope"
            ),
            _ => return false,
        }
    }
}

fn object_contains_jpx_filter(
    document: &PdfDocument,
    filter: &Object,
    visited: &mut HashSet<ObjectRef>,
) -> bool {
    match filter {
        Object::Reference(reference) => {
            assert!(
                visited.insert(*reference),
                "GOS5 image Filter contains an indirect-reference cycle"
            );
            let resolved = document
                .load_object(*reference)
                .expect("GOS5 image Filter reference loads");
            object_contains_jpx_filter(document, &resolved, visited)
        }
        Object::Name(name) => name == "JPXDecode",
        Object::Array(values) => values
            .iter()
            .any(|value| object_contains_jpx_filter(document, value, visited)),
        _ => false,
    }
}

fn contains_non_normal_blend_mode(value: &Object) -> bool {
    match value {
        Object::Name(name) => name != "Normal" && name != "Compatible",
        Object::Array(values) => values.iter().any(contains_non_normal_blend_mode),
        _ => false,
    }
}

fn contains_jpx_filter(value: &Object) -> bool {
    match value {
        Object::Name(name) => name == "JPXDecode",
        Object::Array(values) => values.iter().any(contains_jpx_filter),
        _ => false,
    }
}

#[test]
#[ignore = "requires the separately downloaded, non-redistributable official GOS5 corpus"]
fn official_gos5_x4_is_hash_pinned_and_each_authority_component_is_inspectable() {
    let path = env::var_os(GOS5_PDF_ENV)
        .unwrap_or_else(|| panic!("set {GOS5_PDF_ENV} to the official GOS5 X4 PDF"));
    let bytes = fs::read(&path).expect("read the separately downloaded GOS5 PDF");
    let artifact_hash = ArtifactHash::of(&bytes);
    assert_eq!(artifact_hash.hex().to_uppercase(), GOS5_X4_SHA256);

    let document = PdfDocument::from_bytes(bytes.clone()).expect("GOS5 PDF parses");
    let feature_census = census_document(&document);
    eprintln!("GOS5 allowed-feature census: {feature_census:?}");
    assert!(
        feature_census.soft_masks > 0,
        "the exact GOS5 X4 reachable object graph must contain an SMask"
    );
    assert!(
        feature_census.transparency_groups > 0,
        "the exact GOS5 X4 reachable object graph must contain a transparency group"
    );
    assert!(
        feature_census.non_normal_blend_modes > 0,
        "the exact GOS5 X4 reachable object graph must contain a non-Normal blend mode"
    );
    assert!(
        feature_census.sixteen_bit_images > 0,
        "the exact GOS5 X4 reachable object graph must contain a 16-bit image"
    );
    assert!(
        feature_census.jpx_streams > 0,
        "the exact GOS5 X4 reachable object graph must contain a real JPXDecode stream"
    );
    assert_eq!(
        feature_census.reachable_non_jpx_indexed_image_xobject_streams, 17,
        "the exact GOS5 X4 reachable object graph must contain seventeen non-JPX Indexed Image XObject streams; this census does not prove that content paints them"
    );
    let profile = document
        .output_intent_cmyk_profile()
        .expect("GOS5 X4 carries a CMYK output-intent profile");
    let profile_hash = ArtifactHash::of(profile.bytes());
    eprintln!("starting PDFOxide diagnostic plus Publisher font audit");
    let diagnostic = PdfOxideValidator::new(profile_hash)
        .validate(ValidationRequest::new(
            EvaluationKey::from_bytes([0x50; 32]),
            &bytes,
            PrintStandard::PdfX4,
        ))
        .expect("the diagnostic component returns a bounded verdict");
    eprintln!("starting active-color authority");
    let icc = audit_active_icc_profiles(&bytes).expect("the active-color component completes");
    eprintln!("starting public-object authority");
    let public_objects =
        audit_public_pdfx_objects(&bytes).expect("the public-object component completes");
    eprintln!("starting metadata authority");
    let metadata = audit_public_pdfx_metadata(&bytes).expect("the metadata component completes");

    eprintln!("diagnostic verdict: {:?}", diagnostic.verdict);
    print_findings("diagnostic", &diagnostic.findings);
    print_findings("active color", icc.findings());
    print_findings("public object", public_objects.findings());
    print_findings("metadata", metadata.findings());

    assert!(
        metadata.findings().is_empty(),
        "the independently authored known-good X4 file must retain its PDF/X metadata identity"
    );
    assert!(
        icc.findings().is_empty(),
        "the independently authored known-good X4 file must not produce active-color failures"
    );
    assert_eq!(
        diagnostic.verdict,
        ValidationVerdict::Passed,
        "the known-good GOS5 X4 file must not retain a blocking local font rule: {:?}",
        diagnostic.findings
    );
    assert!(
        diagnostic.findings.iter().all(|finding| {
            !finding.contains("XFONT-002")
                && !finding.contains("font /T3_0")
                && !finding.contains("font /T3_1")
        }),
        "GOS5 Type 3 fonts must be proved from their Encoding and CharProcs, not rejected categorically"
    );
    assert!(
        public_objects
            .findings()
            .iter()
            .all(|finding| !finding.contains("PX4-FORM-001") && !finding.contains("PX4-FORM-003")),
        "GOS5's signature field disproves the former blanket PDF/X form classification"
    );
    assert!(
        public_objects
            .findings()
            .iter()
            .any(|finding| finding.contains("PUBLISHER-SAFE-FORM-001")),
        "Publisher's deliberately stricter no-form policy must remain visible"
    );
}
