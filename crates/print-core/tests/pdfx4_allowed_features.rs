#![cfg(feature = "pdf-oxide-validator")]

use std::collections::HashSet;

use pdf_oxide::{
    content::{Operator, parse_content_stream},
    document::PdfDocument,
    object::{Object, ObjectRef},
};
use pdf_writer::types::{BlendMode, MaskType, OutputIntentSubtype, TrappingStatus};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, TextStr};
use print_core::{
    ArtifactHash, EvaluationKey, IndependentValidator, PdfOxideValidator, PrintStandard,
    ValidationRequest, ValidationVerdict, audit_active_icc_profiles, audit_public_pdfx_metadata,
    audit_public_pdfx_objects, inspect_output_intent,
};

const CMYK_OUTPUT_PROFILE: &[u8] = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");

const XMP: &[u8] = br#"<?xpacket begin="" id="publisher-v2-allowed-feature-fixture"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/">
      <pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

#[derive(Clone, Copy, Debug)]
enum AllowedFeature {
    AlphaExtGState,
    ImageSoftMask,
    SoftMaskGroup,
    MultiplyBlend,
    SixteenBitImage,
    StaticOptionalContentGroup,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct FeatureCensus {
    alpha_ext_gstates: usize,
    soft_masks: usize,
    transparency_groups: usize,
    non_normal_blend_modes: usize,
    sixteen_bit_images: usize,
    jpx_streams: usize,
    optional_content_groups: usize,
}

fn production_cmyk_output_profile() -> Vec<u8> {
    inspect_output_intent(CMYK_OUTPUT_PROFILE)
        .expect("the retained CMYK fixture is a usable production output profile");
    CMYK_OUTPUT_PROFILE.to_vec()
}

fn allowed_feature_fixture(feature: AllowedFeature) -> (Vec<u8>, ArtifactHash) {
    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);
    let page_id = Ref::new(3);
    let metadata_id = Ref::new(4);
    let profile_id = Ref::new(5);
    let info_id = Ref::new(6);
    let ext_g_state_id = Ref::new(7);
    let image_id = Ref::new(8);
    let image_soft_mask_id = Ref::new(9);
    let transparency_group_id = Ref::new(10);
    let content_id = Ref::new(11);
    let ocg_id = Ref::new(12);
    let profile = production_cmyk_output_profile();
    let profile_hash = ArtifactHash::of(&profile);

    let mut pdf = Pdf::new();
    pdf.set_version(1, 6);
    pdf.set_file_id((vec![0xA4; 16], vec![0xA4; 16]));
    write_catalog(
        &mut pdf,
        feature,
        catalog_id,
        page_tree_id,
        metadata_id,
        profile_id,
        ocg_id,
    );
    write_page(
        &mut pdf,
        feature,
        page_tree_id,
        page_id,
        content_id,
        ext_g_state_id,
        image_id,
        ocg_id,
    );

    let mut content = Content::new();
    match feature {
        AllowedFeature::AlphaExtGState => {
            content.set_parameters(Name(b"AllowedGS"));
            draw_cmyk_square(&mut content);
            pdf.ext_graphics(ext_g_state_id).non_stroking_alpha(0.5);
        }
        AllowedFeature::ImageSoftMask => {
            content.x_object(Name(b"AllowedImage"));
            let mut image = pdf.image_xobject(image_id, &[0, 0, 0, 255]);
            image.width(1).height(1).color_space().device_cmyk();
            image.bits_per_component(8);
            image.pair(Name(b"SMask"), image_soft_mask_id);
            image.finish();
            let mut mask = pdf.image_xobject(image_soft_mask_id, &[128]);
            mask.width(1).height(1).color_space().device_gray();
            mask.bits_per_component(8);
        }
        AllowedFeature::SoftMaskGroup => {
            content.set_parameters(Name(b"AllowedGS"));
            draw_cmyk_square(&mut content);
            let mut group_content = Content::new();
            group_content.set_fill_gray(0.5);
            group_content.rect(0.0, 0.0, 72.0, 72.0).fill_nonzero();
            let group_bytes = group_content.finish();
            let mut group = pdf.form_xobject(transparency_group_id, &group_bytes);
            group.bbox(Rect::new(0.0, 0.0, 72.0, 72.0));
            group.resources().finish();
            let mut group_settings = group.group();
            group_settings.transparency().isolated(true);
            group_settings.color_space().device_gray();
            group_settings.finish();
            group.finish();
            let mut state = pdf.ext_graphics(ext_g_state_id);
            let mut mask = state.soft_mask();
            mask.subtype(MaskType::Alpha).group(transparency_group_id);
        }
        AllowedFeature::MultiplyBlend => {
            content.set_parameters(Name(b"AllowedGS"));
            draw_cmyk_square(&mut content);
            pdf.ext_graphics(ext_g_state_id)
                .blend_mode(BlendMode::Multiply);
        }
        AllowedFeature::SixteenBitImage => {
            content.x_object(Name(b"AllowedImage"));
            let mut image = pdf.image_xobject(image_id, &[0, 0, 0, 0, 0, 0, 0xFF, 0xFF]);
            image.width(1).height(1).color_space().device_cmyk();
            image.bits_per_component(16);
        }
        AllowedFeature::StaticOptionalContentGroup => {
            content
                .begin_marked_content_with_properties(Name(b"OC"))
                .properties_named(Name(b"AllowedLayer"));
            draw_cmyk_square(&mut content);
            content.end_marked_content();
            let mut ocg = pdf.indirect(ocg_id).dict();
            ocg.pair(Name(b"Type"), Name(b"OCG"));
            ocg.pair(Name(b"Name"), TextStr("Allowed static print layer"));
        }
    }
    pdf.stream(content_id, &content.finish());
    write_fixture_identity(&mut pdf, metadata_id, profile_id, info_id, &profile);

    (pdf.finish(), profile_hash)
}

fn write_fixture_identity(
    pdf: &mut Pdf,
    metadata_id: Ref,
    profile_id: Ref,
    info_id: Ref,
    profile: &[u8],
) {
    pdf.metadata(metadata_id, XMP);
    pdf.icc_profile(profile_id, profile).n(4);
    pdf.document_info(info_id)
        .producer(TextStr("Publisher V2 allowed-feature fixture"))
        .trapped(TrappingStatus::NotTrapped)
        .pair(Name(b"GTS_PDFXVersion"), TextStr("PDF/X-4"));
}

fn write_catalog(
    pdf: &mut Pdf,
    feature: AllowedFeature,
    catalog_id: Ref,
    page_tree_id: Ref,
    metadata_id: Ref,
    profile_id: Ref,
    ocg_id: Ref,
) {
    let mut catalog = pdf.catalog(catalog_id);
    catalog.pages(page_tree_id).metadata(metadata_id);
    let mut intents = catalog.output_intents();
    intents
        .push()
        .subtype(OutputIntentSubtype::PDFX)
        .output_condition_identifier(TextStr("Publisher V2 allowed-feature CMYK"))
        .info(TextStr(
            "Synthetic validation fixture; not a production profile",
        ))
        .dest_output_profile(profile_id);
    intents.finish();
    if matches!(feature, AllowedFeature::StaticOptionalContentGroup) {
        let mut properties = catalog.insert(Name(b"OCProperties")).dict();
        properties.insert(Name(b"OCGs")).array().item(ocg_id);
        let mut default = properties.insert(Name(b"D")).dict();
        default.insert(Name(b"Order")).array().item(ocg_id);
        default.insert(Name(b"ON")).array().item(ocg_id);
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "explicit fixture object IDs keep the six allowed-feature branches auditable"
)]
fn write_page(
    pdf: &mut Pdf,
    feature: AllowedFeature,
    page_tree_id: Ref,
    page_id: Ref,
    content_id: Ref,
    ext_g_state_id: Ref,
    image_id: Ref,
    ocg_id: Ref,
) {
    pdf.pages(page_tree_id).kids([page_id]).count(1);
    let mut page = pdf.page(page_id);
    page.parent(page_tree_id)
        .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
        .bleed_box(Rect::new(0.0, 0.0, 612.0, 792.0))
        .trim_box(Rect::new(9.0, 9.0, 603.0, 783.0))
        .contents(content_id);
    let mut resources = page.resources();
    match feature {
        AllowedFeature::AlphaExtGState
        | AllowedFeature::SoftMaskGroup
        | AllowedFeature::MultiplyBlend => {
            resources
                .ext_g_states()
                .pair(Name(b"AllowedGS"), ext_g_state_id);
        }
        AllowedFeature::ImageSoftMask | AllowedFeature::SixteenBitImage => {
            resources.x_objects().pair(Name(b"AllowedImage"), image_id);
        }
        AllowedFeature::StaticOptionalContentGroup => {
            resources
                .insert(Name(b"Properties"))
                .dict()
                .pair(Name(b"AllowedLayer"), ocg_id);
        }
    }
}

fn draw_cmyk_square(content: &mut Content) {
    content.set_fill_cmyk(0.2, 0.1, 0.0, 0.1);
    content.rect(72.0, 72.0, 72.0, 72.0).fill_nonzero();
}

fn census(candidate: &[u8]) -> FeatureCensus {
    let document = PdfDocument::from_bytes(candidate.to_vec()).expect("allowed fixture parses");
    census_document(&document)
}

fn assert_feature_is_applied(candidate: &[u8], feature: AllowedFeature) {
    let document = PdfDocument::from_bytes(candidate.to_vec()).expect("allowed fixture parses");
    let content = document
        .get_page_content_data(0)
        .expect("allowed fixture page content loads");
    let operators = parse_content_stream(&content).expect("allowed fixture content parses");
    let applied = match feature {
        AllowedFeature::AlphaExtGState
        | AllowedFeature::SoftMaskGroup
        | AllowedFeature::MultiplyBlend => operators.iter().any(|operator| {
            matches!(
                operator,
                Operator::SetExtGState { dict_name } if dict_name == "AllowedGS"
            )
        }),
        AllowedFeature::ImageSoftMask | AllowedFeature::SixteenBitImage => operators
            .iter()
            .any(|operator| matches!(operator, Operator::Do { name } if name == "AllowedImage")),
        AllowedFeature::StaticOptionalContentGroup => operators.iter().any(|operator| {
            matches!(
                operator,
                Operator::BeginMarkedContentDict { tag, properties }
                    if tag == "OC" && properties.as_name() == Some("AllowedLayer")
            )
        }),
    };
    assert!(applied, "{feature:?} must be selected by page content");
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
                inspect_dictionary(&dictionary, &mut census);
                pending.extend(dictionary.into_values());
            }
            Object::Stream { dict, .. } => {
                inspect_dictionary(&dict, &mut census);
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

fn inspect_dictionary(
    dictionary: &std::collections::HashMap<String, Object>,
    census: &mut FeatureCensus,
) {
    let type_name = dictionary.get("Type").and_then(Object::as_name);
    let subtype = dictionary.get("Subtype").and_then(Object::as_name);
    if type_name == Some("ExtGState")
        && ["CA", "ca"]
            .iter()
            .any(|key| dictionary.get(*key).is_some_and(is_non_opaque_alpha))
    {
        census.alpha_ext_gstates += 1;
    }
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
    if type_name == Some("OCG") {
        census.optional_content_groups += 1;
    }
}

fn is_non_opaque_alpha(value: &Object) -> bool {
    value.as_real().is_some_and(|alpha| alpha < 1.0)
        || value.as_integer().is_some_and(|alpha| alpha < 1)
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

fn assert_composite_audit_does_not_reject(candidate: &[u8], profile_hash: ArtifactHash) {
    let diagnostic = PdfOxideValidator::new(profile_hash)
        .validate(ValidationRequest::new(
            EvaluationKey::from_bytes([0xA4; 32]),
            candidate,
            PrintStandard::PdfX4,
        ))
        .expect("diagnostic completes");
    assert_eq!(
        diagnostic.verdict,
        ValidationVerdict::Passed,
        "allowed feature must not be rejected by the current diagnostic: {:?}",
        diagnostic.findings
    );
    assert!(
        audit_active_icc_profiles(candidate)
            .expect("active-color audit completes")
            .findings()
            .is_empty(),
        "allowed feature must not be rejected by active-color authority"
    );
    assert!(
        audit_public_pdfx_objects(candidate)
            .expect("public-object audit completes")
            .findings()
            .is_empty(),
        "allowed feature must not be rejected by public-object authority"
    );
    assert!(
        audit_public_pdfx_metadata(candidate)
            .expect("metadata audit completes")
            .findings()
            .is_empty(),
        "allowed feature must not be rejected by metadata authority"
    );
}

#[test]
fn pdfx4_transparency_imaging_and_static_layers_are_positive_controls() {
    let cases = [
        AllowedFeature::AlphaExtGState,
        AllowedFeature::ImageSoftMask,
        AllowedFeature::SoftMaskGroup,
        AllowedFeature::MultiplyBlend,
        AllowedFeature::SixteenBitImage,
        AllowedFeature::StaticOptionalContentGroup,
    ];

    for feature in cases {
        let (candidate, profile_hash) = allowed_feature_fixture(feature);
        let census = census(&candidate);
        assert_feature_is_applied(&candidate, feature);
        assert!(
            intended_feature_count(feature, &census) > 0,
            "{feature:?} fixture must contain its intended feature: {census:?}"
        );
        if matches!(feature, AllowedFeature::SoftMaskGroup) {
            assert!(
                census.soft_masks > 0,
                "soft-mask group requires an SMask dictionary"
            );
        }
        assert_composite_audit_does_not_reject(&candidate, profile_hash);
    }
}

fn intended_feature_count(feature: AllowedFeature, census: &FeatureCensus) -> usize {
    match feature {
        AllowedFeature::AlphaExtGState => census.alpha_ext_gstates,
        AllowedFeature::ImageSoftMask => census.soft_masks,
        AllowedFeature::SoftMaskGroup => census.transparency_groups,
        AllowedFeature::MultiplyBlend => census.non_normal_blend_modes,
        AllowedFeature::SixteenBitImage => census.sixteen_bit_images,
        AllowedFeature::StaticOptionalContentGroup => census.optional_content_groups,
    }
}

// JPEG 2000 remains corpus-only evidence here. A syntactically labelled
// /JPXDecode stream with invented bytes would test parser tolerance, not valid
// JPEG 2000 acceptance, and would therefore overstate the evidence.
