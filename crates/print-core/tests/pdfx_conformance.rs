#![cfg(feature = "pdf-oxide-validator")]

use pdf_writer::types::{
    AnnotationType, CidFontType, OutputIntentSubtype, SystemInfo, TrappingStatus,
};
use pdf_writer::writers::ColorSpace;
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use print_core::{
    ArtifactHash, EvaluationKey, PdfOxideValidator, PrintStandard, ValidationRequest,
    ValidationVerdict, audit_active_icc_profiles, audit_public_pdfx_objects, inspect_output_intent,
    run_independent_validation,
};

const XMP: &[u8] = br#"<?xpacket begin="" id="publisher-v2-fixture"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/">
      <pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

const XMP_WITHOUT_PDFX_ID: &[u8] = br#"<?xpacket begin="" id="publisher-v2-fixture"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description />
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

#[derive(Clone, Copy)]
enum PageBoxFault {
    None,
    MissingMedia,
    MissingTrim,
    BleedOutsideMedia,
    TrimOutsideBleed,
}

#[derive(Clone, Copy)]
enum ActionFault {
    JavaScript,
    Launch,
}

#[derive(Clone, Copy)]
enum FixtureFault {
    None,
    MissingOutputIntent,
    MissingDestinationProfile,
    UnembeddedFont,
    InheritedUnembeddedFont,
    NestedUnembeddedFont,
    AnnotationAppearanceUnembeddedFont,
    AnnotationAppearanceStateUnembeddedFont,
    CorruptEmbeddedTrueTypeFont,
    MissingUsedIdentityGlyph,
    NestedMissingUsedIdentityGlyph,
    AppearanceMissingUsedIdentityGlyph,
    ValidDefaultIdentityType2,
    ValidOpenTypeIdentityType2,
    CffOpenTypeIdentityType2,
    UsedNotdefIdentityType2,
    ValidStreamCidToGidType2,
    ValidFlateStreamCidToGidType2,
    ZeroStreamCidToGidType2,
    OutOfRangeStreamCidToGidType2,
    ShortStreamCidToGidType2,
    OddStreamCidToGidType2,
    MalformedFlateCidToGidType2,
    ValidIdentityCffFont,
    MissingUsedIdentityCffGlyph,
    UsedNotdefIdentityCff,
    MalformedIdentityCffCharstring,
    ValidSimpleCffFont,
    MissingUsedSimpleCffGlyph,
    MalformedSimpleCffCharstring,
    ConflictingCffInternalEncoding,
    ValidPredefinedCharsetCffFont,
    MissingUsedPredefinedCharsetCffGlyph,
    ValidPredefinedWinAnsiCffFont,
    MissingUsedPredefinedWinAnsiCffGlyph,
    ValidSimpleTrueTypeFont,
    MissingUsedSimpleTrueTypeGlyph,
    SymbolicSimpleTrueTypeFont,
    ValidType3Font,
    MissingUsedType3Glyph,
    MalformedType3CharProc,
    MissingType3FirstChar,
    Type3OutOfRangeGlyph,
    Type3NestedUnembeddedFont,
    UnusedMalformedType3Encoding,
    PageBox(PageBoxFault),
    Action(ActionFault),
    Annotation,
    MalformedPageIcc,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Type3FixtureKind {
    Valid,
    MissingGlyph,
    MalformedCharProc,
    MissingFirstChar,
    OutOfRangeGlyph,
    NestedUnembeddedFont,
    UnusedMalformedEncoding,
}

fn type3_fixture_kind(fault: FixtureFault) -> Option<Type3FixtureKind> {
    match fault {
        FixtureFault::ValidType3Font => Some(Type3FixtureKind::Valid),
        FixtureFault::MissingUsedType3Glyph => Some(Type3FixtureKind::MissingGlyph),
        FixtureFault::MalformedType3CharProc => Some(Type3FixtureKind::MalformedCharProc),
        FixtureFault::MissingType3FirstChar => Some(Type3FixtureKind::MissingFirstChar),
        FixtureFault::Type3OutOfRangeGlyph => Some(Type3FixtureKind::OutOfRangeGlyph),
        FixtureFault::Type3NestedUnembeddedFont => Some(Type3FixtureKind::NestedUnembeddedFont),
        FixtureFault::UnusedMalformedType3Encoding => {
            Some(Type3FixtureKind::UnusedMalformedEncoding)
        }
        _ => None,
    }
}

fn synthetic_cmyk_output_profile() -> Vec<u8> {
    let mut profile = vec![0_u8; 128];
    profile[0..4].copy_from_slice(&128_u32.to_be_bytes());
    profile[12..16].copy_from_slice(b"prtr");
    profile[16..20].copy_from_slice(b"CMYK");
    profile[20..24].copy_from_slice(b"Lab ");
    profile[36..40].copy_from_slice(b"acsp");
    profile
}

#[allow(clippy::too_many_lines)]
fn differential_fixture(fault: FixtureFault, xmp: &[u8]) -> (Vec<u8>, ArtifactHash) {
    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);
    let page_id = Ref::new(3);
    let metadata_id = Ref::new(4);
    let profile_id = Ref::new(5);
    let info_id = Ref::new(6);
    let font_id = Ref::new(7);
    let content_id = Ref::new(8);
    let action_id = Ref::new(9);
    let annotation_id = Ref::new(10);
    let bad_page_profile_id = Ref::new(11);
    let form_id = Ref::new(12);
    let profile = synthetic_cmyk_output_profile();
    let profile_hash = ArtifactHash::of(&profile);

    let mut pdf = Pdf::new();
    pdf.set_version(1, 6);
    pdf.set_file_id((vec![7; 16], vec![7; 16]));
    {
        let mut catalog = pdf.catalog(catalog_id);
        catalog.pages(page_tree_id).metadata(metadata_id);
        if matches!(fault, FixtureFault::Action(_)) {
            catalog.pair(Name(b"OpenAction"), action_id);
        }
        if !matches!(fault, FixtureFault::MissingOutputIntent) {
            let mut intents = catalog.output_intents();
            let mut intent = intents.push();
            intent
                .subtype(OutputIntentSubtype::PDFX)
                .output_condition_identifier(TextStr("Publisher V2 test CMYK"))
                .info(TextStr(
                    "Synthetic validation fixture; not a production profile",
                ));
            if !matches!(fault, FixtureFault::MissingDestinationProfile) {
                intent.dest_output_profile(profile_id);
            }
        }
    }
    write_page_tree(&mut pdf, fault, page_tree_id, page_id, font_id);
    {
        let mut page = pdf.page(page_id);
        page.parent(page_tree_id);
        let page_box_fault = match fault {
            FixtureFault::PageBox(page_box_fault) => page_box_fault,
            _ => PageBoxFault::None,
        };
        if !matches!(page_box_fault, PageBoxFault::MissingMedia) {
            page.media_box(Rect::new(0.0, 0.0, 612.0, 792.0));
        }
        page.bleed_box(match page_box_fault {
            PageBoxFault::BleedOutsideMedia => Rect::new(-1.0, -1.0, 613.0, 793.0),
            PageBoxFault::TrimOutsideBleed => Rect::new(9.0, 9.0, 603.0, 783.0),
            _ => Rect::new(0.0, 0.0, 612.0, 792.0),
        });
        if !matches!(page_box_fault, PageBoxFault::MissingTrim) {
            page.trim_box(match page_box_fault {
                PageBoxFault::TrimOutsideBleed => Rect::new(8.0, 8.0, 604.0, 784.0),
                _ => Rect::new(9.0, 9.0, 603.0, 783.0),
            });
        }
        if matches!(
            fault,
            FixtureFault::UnembeddedFont
                | FixtureFault::CorruptEmbeddedTrueTypeFont
                | FixtureFault::MissingUsedIdentityGlyph
                | FixtureFault::ValidDefaultIdentityType2
                | FixtureFault::ValidOpenTypeIdentityType2
                | FixtureFault::CffOpenTypeIdentityType2
                | FixtureFault::UsedNotdefIdentityType2
                | FixtureFault::ValidStreamCidToGidType2
                | FixtureFault::ValidFlateStreamCidToGidType2
                | FixtureFault::ZeroStreamCidToGidType2
                | FixtureFault::OutOfRangeStreamCidToGidType2
                | FixtureFault::ShortStreamCidToGidType2
                | FixtureFault::OddStreamCidToGidType2
                | FixtureFault::MalformedFlateCidToGidType2
                | FixtureFault::ValidIdentityCffFont
                | FixtureFault::MissingUsedIdentityCffGlyph
                | FixtureFault::UsedNotdefIdentityCff
                | FixtureFault::MalformedIdentityCffCharstring
                | FixtureFault::ValidSimpleCffFont
                | FixtureFault::MissingUsedSimpleCffGlyph
                | FixtureFault::MalformedSimpleCffCharstring
                | FixtureFault::ConflictingCffInternalEncoding
                | FixtureFault::ValidPredefinedCharsetCffFont
                | FixtureFault::MissingUsedPredefinedCharsetCffGlyph
                | FixtureFault::ValidPredefinedWinAnsiCffFont
                | FixtureFault::MissingUsedPredefinedWinAnsiCffGlyph
                | FixtureFault::ValidSimpleTrueTypeFont
                | FixtureFault::MissingUsedSimpleTrueTypeGlyph
                | FixtureFault::SymbolicSimpleTrueTypeFont
        ) {
            page.contents(content_id);
            page.resources().fonts().pair(
                if matches!(
                    fault,
                    FixtureFault::MissingUsedIdentityGlyph
                        | FixtureFault::ValidDefaultIdentityType2
                        | FixtureFault::ValidOpenTypeIdentityType2
                        | FixtureFault::CffOpenTypeIdentityType2
                        | FixtureFault::UsedNotdefIdentityType2
                        | FixtureFault::ValidStreamCidToGidType2
                        | FixtureFault::ValidFlateStreamCidToGidType2
                        | FixtureFault::ZeroStreamCidToGidType2
                        | FixtureFault::OutOfRangeStreamCidToGidType2
                        | FixtureFault::ShortStreamCidToGidType2
                        | FixtureFault::OddStreamCidToGidType2
                        | FixtureFault::MalformedFlateCidToGidType2
                ) {
                    Name(b"GapFont")
                } else if matches!(
                    fault,
                    FixtureFault::ValidIdentityCffFont
                        | FixtureFault::MissingUsedIdentityCffGlyph
                        | FixtureFault::UsedNotdefIdentityCff
                        | FixtureFault::MalformedIdentityCffCharstring
                ) {
                    Name(b"CffFont")
                } else if matches!(
                    fault,
                    FixtureFault::ValidSimpleCffFont
                        | FixtureFault::MissingUsedSimpleCffGlyph
                        | FixtureFault::MalformedSimpleCffCharstring
                        | FixtureFault::ConflictingCffInternalEncoding
                        | FixtureFault::ValidPredefinedCharsetCffFont
                        | FixtureFault::MissingUsedPredefinedCharsetCffGlyph
                        | FixtureFault::ValidPredefinedWinAnsiCffFont
                        | FixtureFault::MissingUsedPredefinedWinAnsiCffGlyph
                ) {
                    Name(b"SimpleCff")
                } else if matches!(
                    fault,
                    FixtureFault::ValidSimpleTrueTypeFont
                        | FixtureFault::MissingUsedSimpleTrueTypeGlyph
                        | FixtureFault::SymbolicSimpleTrueTypeFont
                ) {
                    Name(b"SimpleTrueType")
                } else {
                    Name(b"BadFont")
                },
                font_id,
            );
        }
        if let Some(kind) = type3_fixture_kind(fault) {
            if kind != Type3FixtureKind::UnusedMalformedEncoding {
                page.contents(content_id);
            }
            page.resources().fonts().pair(Name(b"Type3Font"), font_id);
        }
        if matches!(fault, FixtureFault::InheritedUnembeddedFont) {
            page.contents(content_id);
        }
        if matches!(fault, FixtureFault::NestedUnembeddedFont) {
            page.contents(content_id);
            page.resources()
                .x_objects()
                .pair(Name(b"BadFontForm"), form_id);
        }
        if matches!(fault, FixtureFault::NestedMissingUsedIdentityGlyph) {
            page.contents(content_id);
            page.resources().x_objects().pair(Name(b"GapForm"), form_id);
        }
        if matches!(
            fault,
            FixtureFault::AnnotationAppearanceUnembeddedFont
                | FixtureFault::AnnotationAppearanceStateUnembeddedFont
                | FixtureFault::AppearanceMissingUsedIdentityGlyph
        ) {
            page.annotations([annotation_id]);
        }
        if matches!(fault, FixtureFault::Annotation) {
            page.annotations([annotation_id]);
        }
        if matches!(fault, FixtureFault::MalformedPageIcc) {
            page.contents(content_id);
            page.resources()
                .color_spaces()
                .insert(Name(b"BadIcc"))
                .start::<ColorSpace>()
                .icc_based(bad_page_profile_id);
        }
        page.finish();
    }
    write_font_faults(&mut pdf, fault, font_id, content_id, form_id, annotation_id);
    write_malformed_page_icc_fault(&mut pdf, fault, content_id, bad_page_profile_id);
    write_action_and_annotation_faults(&mut pdf, fault, action_id, annotation_id);
    pdf.metadata(metadata_id, xmp);
    pdf.icc_profile(profile_id, &profile).n(4);
    pdf.document_info(info_id)
        .producer(TextStr("Publisher V2 differential fixture"))
        .trapped(TrappingStatus::NotTrapped)
        .pair(Name(b"GTS_PDFXVersion"), TextStr("PDF/X-4"));

    (pdf.finish(), profile_hash)
}

fn write_page_tree(
    pdf: &mut Pdf,
    fault: FixtureFault,
    page_tree_id: Ref,
    page_id: Ref,
    font_id: Ref,
) {
    let mut pages = pdf.pages(page_tree_id);
    pages.kids([page_id]).count(1);
    if matches!(fault, FixtureFault::InheritedUnembeddedFont) {
        pages.resources().fonts().pair(Name(b"TreeFont"), font_id);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the differential fixture router keeps each font fault explicit and auditable"
)]
fn write_font_faults(
    pdf: &mut Pdf,
    fault: FixtureFault,
    font_id: Ref,
    content_id: Ref,
    form_id: Ref,
    annotation_id: Ref,
) {
    if matches!(fault, FixtureFault::UnembeddedFont) {
        write_unembedded_font_fixture(pdf, font_id, content_id);
    }
    if matches!(fault, FixtureFault::CorruptEmbeddedTrueTypeFont) {
        write_corrupt_truetype_fault(pdf, font_id, content_id);
    }
    if matches!(fault, FixtureFault::MissingUsedIdentityGlyph) {
        write_identity_missing_glyph_fault(pdf, font_id, content_id);
    }
    if matches!(
        fault,
        FixtureFault::ValidDefaultIdentityType2
            | FixtureFault::ValidOpenTypeIdentityType2
            | FixtureFault::CffOpenTypeIdentityType2
            | FixtureFault::UsedNotdefIdentityType2
    ) {
        write_default_identity_type2_fixture(
            pdf,
            font_id,
            content_id,
            matches!(fault, FixtureFault::ValidOpenTypeIdentityType2),
            matches!(fault, FixtureFault::CffOpenTypeIdentityType2),
            matches!(fault, FixtureFault::UsedNotdefIdentityType2),
        );
    }
    if let Some(kind) = match fault {
        FixtureFault::ValidStreamCidToGidType2 => Some(StreamCidToGidFixture::Valid),
        FixtureFault::ValidFlateStreamCidToGidType2 => Some(StreamCidToGidFixture::ValidFlate),
        FixtureFault::ZeroStreamCidToGidType2 => Some(StreamCidToGidFixture::Zero),
        FixtureFault::OutOfRangeStreamCidToGidType2 => Some(StreamCidToGidFixture::OutOfRange),
        FixtureFault::ShortStreamCidToGidType2 => Some(StreamCidToGidFixture::Short),
        FixtureFault::OddStreamCidToGidType2 => Some(StreamCidToGidFixture::Odd),
        FixtureFault::MalformedFlateCidToGidType2 => Some(StreamCidToGidFixture::MalformedFlate),
        _ => None,
    } {
        write_stream_cid_to_gid_type2_fixture(pdf, font_id, content_id, kind);
    }
    if matches!(
        fault,
        FixtureFault::ValidIdentityCffFont
            | FixtureFault::MissingUsedIdentityCffGlyph
            | FixtureFault::UsedNotdefIdentityCff
            | FixtureFault::MalformedIdentityCffCharstring
    ) {
        write_identity_cff_fixture(
            pdf,
            font_id,
            content_id,
            matches!(fault, FixtureFault::MissingUsedIdentityCffGlyph),
            matches!(fault, FixtureFault::UsedNotdefIdentityCff),
            matches!(fault, FixtureFault::MalformedIdentityCffCharstring),
        );
    }
    if matches!(
        fault,
        FixtureFault::ValidSimpleCffFont
            | FixtureFault::MissingUsedSimpleCffGlyph
            | FixtureFault::MalformedSimpleCffCharstring
            | FixtureFault::ConflictingCffInternalEncoding
    ) {
        write_simple_cff_fixture(
            pdf,
            font_id,
            content_id,
            matches!(fault, FixtureFault::MissingUsedSimpleCffGlyph),
            matches!(fault, FixtureFault::MalformedSimpleCffCharstring),
            matches!(fault, FixtureFault::ConflictingCffInternalEncoding),
        );
    }
    if matches!(
        fault,
        FixtureFault::ValidPredefinedCharsetCffFont
            | FixtureFault::MissingUsedPredefinedCharsetCffGlyph
    ) {
        write_predefined_charset_cff_fixture(
            pdf,
            font_id,
            content_id,
            matches!(fault, FixtureFault::MissingUsedPredefinedCharsetCffGlyph),
        );
    }
    if matches!(
        fault,
        FixtureFault::ValidPredefinedWinAnsiCffFont
            | FixtureFault::MissingUsedPredefinedWinAnsiCffGlyph
    ) {
        write_predefined_win_ansi_cff_fixture(
            pdf,
            font_id,
            content_id,
            matches!(fault, FixtureFault::MissingUsedPredefinedWinAnsiCffGlyph),
        );
    }
    if matches!(
        fault,
        FixtureFault::ValidSimpleTrueTypeFont
            | FixtureFault::MissingUsedSimpleTrueTypeGlyph
            | FixtureFault::SymbolicSimpleTrueTypeFont
    ) {
        write_simple_truetype_fixture(
            pdf,
            font_id,
            content_id,
            matches!(fault, FixtureFault::MissingUsedSimpleTrueTypeGlyph),
            matches!(fault, FixtureFault::SymbolicSimpleTrueTypeFont),
        );
    }
    if let Some(kind) = type3_fixture_kind(fault) {
        write_type3_font_fixture(pdf, font_id, content_id, kind);
    }
    if matches!(fault, FixtureFault::NestedMissingUsedIdentityGlyph) {
        write_nested_identity_missing_glyph_fault(pdf, font_id, content_id, form_id);
    }
    if matches!(fault, FixtureFault::AppearanceMissingUsedIdentityGlyph) {
        write_appearance_identity_missing_glyph_fault(pdf, font_id, annotation_id, form_id);
    }
    if matches!(fault, FixtureFault::InheritedUnembeddedFont) {
        write_inherited_unembedded_font_fixture(pdf, font_id, content_id);
    }
    if matches!(fault, FixtureFault::NestedUnembeddedFont) {
        write_nested_unembedded_font_fixture(pdf, font_id, content_id, form_id);
    }
    if matches!(
        fault,
        FixtureFault::AnnotationAppearanceUnembeddedFont
            | FixtureFault::AnnotationAppearanceStateUnembeddedFont
    ) {
        write_annotation_unembedded_font_fixture(pdf, fault, font_id, annotation_id, form_id);
    }
}

fn write_unembedded_font_fixture(pdf: &mut Pdf, font_id: Ref, content_id: Ref) {
    pdf.type1_font(font_id).base_font(Name(b"Helvetica"));
    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"BadFont"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(b"Unembedded font fault"));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn write_inherited_unembedded_font_fixture(pdf: &mut Pdf, font_id: Ref, content_id: Ref) {
    pdf.type1_font(font_id).base_font(Name(b"Helvetica"));
    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"TreeFont"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(b"Inherited unembedded font fault"));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn write_nested_unembedded_font_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    form_id: Ref,
) {
    pdf.type1_font(font_id).base_font(Name(b"Helvetica"));

    let mut page_content = Content::new();
    page_content.x_object(Name(b"BadFontForm"));
    pdf.stream(content_id, &page_content.finish());

    let mut form_content = Content::new();
    form_content.begin_text();
    form_content.set_font(Name(b"NestedBadFont"), 12.0);
    form_content.next_line(0.0, 24.0);
    form_content.show(Str(b"Nested unembedded font fault"));
    form_content.end_text();
    let form_content = form_content.finish();
    let mut form = pdf.form_xobject(form_id, &form_content);
    form.bbox(Rect::new(0.0, 0.0, 240.0, 48.0));
    form.resources()
        .fonts()
        .pair(Name(b"NestedBadFont"), font_id);
    form.finish();
}

fn write_annotation_unembedded_font_fixture(
    pdf: &mut Pdf,
    fault: FixtureFault,
    font_id: Ref,
    annotation_id: Ref,
    form_id: Ref,
) {
    pdf.type1_font(font_id).base_font(Name(b"Helvetica"));
    let mut annotation = pdf.indirect(annotation_id).dict();
    annotation.pair(Name(b"Type"), Name(b"Annot"));
    annotation.pair(Name(b"Subtype"), Name(b"Stamp"));
    annotation.pair(Name(b"Rect"), Rect::new(0.0, 0.0, 48.0, 48.0));
    let mut appearances = annotation.insert(Name(b"AP")).dict();
    if matches!(fault, FixtureFault::AnnotationAppearanceStateUnembeddedFont) {
        appearances
            .insert(Name(b"N"))
            .dict()
            .pair(Name(b"On"), form_id);
    } else {
        appearances.pair(Name(b"N"), form_id);
    }
    appearances.finish();
    annotation.finish();

    let mut appearance_content = Content::new();
    appearance_content.begin_text();
    appearance_content.set_font(Name(b"AppearanceBadFont"), 12.0);
    appearance_content.next_line(0.0, 24.0);
    appearance_content.show(Str(b"Appearance font fault"));
    appearance_content.end_text();
    let appearance_content = appearance_content.finish();
    let mut appearance = pdf.form_xobject(form_id, &appearance_content);
    appearance.bbox(Rect::new(0.0, 0.0, 48.0, 48.0));
    appearance
        .resources()
        .fonts()
        .pair(Name(b"AppearanceBadFont"), font_id);
    appearance.finish();
}

fn write_type3_font_fixture(pdf: &mut Pdf, font_id: Ref, content_id: Ref, kind: Type3FixtureKind) {
    let char_proc_id = Ref::new(13);
    let nested_font_id = Ref::new(14);
    let mut font = pdf.indirect(font_id).dict();
    font.pair(Name(b"Type"), Name(b"Font"));
    font.pair(Name(b"Subtype"), Name(b"Type3"));
    font.pair(Name(b"Name"), Name(b"FixtureType3"));
    font.insert(Name(b"FontBBox"))
        .array()
        .items([0, 0, 1_000, 1_000]);
    font.insert(Name(b"FontMatrix"))
        .array()
        .items([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
    font.insert(Name(b"CharProcs"))
        .dict()
        .pair(Name(b"A"), char_proc_id);
    {
        let mut encoding = font.insert(Name(b"Encoding")).dict();
        let mut differences = encoding.insert(Name(b"Differences")).array();
        if kind != Type3FixtureKind::UnusedMalformedEncoding {
            differences.item(if kind == Type3FixtureKind::OutOfRangeGlyph {
                64
            } else {
                65
            });
        }
        differences.item(Name(b"A"));
        if kind == Type3FixtureKind::MissingGlyph {
            differences.item(Name(b"B"));
        }
        differences.finish();
        encoding.finish();
    }
    if kind != Type3FixtureKind::MissingFirstChar {
        font.pair(Name(b"FirstChar"), 65);
    }
    font.pair(Name(b"LastChar"), 66);
    font.insert(Name(b"Widths")).array().items([600, 600]);
    if kind == Type3FixtureKind::NestedUnembeddedFont {
        font.insert(Name(b"Resources"))
            .dict()
            .insert(Name(b"Font"))
            .dict()
            .pair(Name(b"NestedBad"), nested_font_id);
    } else {
        font.insert(Name(b"Resources")).dict();
    }
    font.finish();
    pdf.stream(
        char_proc_id,
        if kind == Type3FixtureKind::MalformedCharProc {
            b"not a valid Type 3 glyph program".as_slice()
        } else if kind == Type3FixtureKind::NestedUnembeddedFont {
            b"600 0 d0 BT /NestedBad 10 Tf (x) Tj ET".as_slice()
        } else {
            b"600 0 d0".as_slice()
        },
    );
    if kind == Type3FixtureKind::NestedUnembeddedFont {
        pdf.type1_font(nested_font_id).base_font(Name(b"Helvetica"));
    }

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"Type3Font"), 12.0);
    content.next_line(72.0, 720.0);
    let shown_codes = if kind == Type3FixtureKind::OutOfRangeGlyph {
        vec![64]
    } else if kind == Type3FixtureKind::MissingGlyph {
        vec![66; 4_096]
    } else {
        vec![65]
    };
    content.show(Str(&shown_codes));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn write_corrupt_truetype_fault(pdf: &mut Pdf, font_id: Ref, content_id: Ref) {
    let descriptor_id = Ref::new(13);
    let program_id = Ref::new(14);
    let mut font = pdf.indirect(font_id).dict();
    font.pair(Name(b"Type"), Name(b"Font"));
    font.pair(Name(b"Subtype"), Name(b"TrueType"));
    font.pair(Name(b"BaseFont"), Name(b"BrokenEmbedded"));
    font.pair(Name(b"FontDescriptor"), descriptor_id);
    font.finish();
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"BrokenEmbedded"));
    descriptor.pair(Name(b"FontFile2"), program_id);
    descriptor.finish();
    pdf.stream(program_id, b"not a TrueType font");

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"BadFont"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(b"Corrupt embedded font fault"));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn write_identity_missing_glyph_fault(pdf: &mut Pdf, font_id: Ref, content_id: Ref) {
    write_identity_font_with_gap(pdf, font_id);
    write_missing_glyph_text(pdf, content_id);
}

fn write_identity_font_with_gap(pdf: &mut Pdf, font_id: Ref) {
    let descendant_id = Ref::new(13);
    let descriptor_id = Ref::new(14);
    let program_id = Ref::new(15);
    let cid_set_id = Ref::new(16);
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
    pdf.type0_font(font_id)
        .base_font(Name(b"FixtureDejaVuSans"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(descendant_id);
    pdf.cid_font(descendant_id)
        .subtype(CidFontType::Type2)
        .base_font(Name(b"FixtureDejaVuSans"))
        .system_info(SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        })
        .font_descriptor(descriptor_id)
        .default_width(1_000.0)
        .cid_to_gid_map_predefined(Name(b"Identity"));
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixtureDejaVuSans"));
    descriptor.pair(Name(b"FontFile2"), program_id);
    descriptor.pair(Name(b"CIDSet"), cid_set_id);
    descriptor.finish();
    pdf.stream(program_id, font_bytes);
    pdf.stream(cid_set_id, &[0xFF]);
}

fn write_default_identity_type2_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    use_open_type_stream: bool,
    use_cff_outlines: bool,
    use_notdef: bool,
) {
    let descendant_id = Ref::new(13);
    let descriptor_id = Ref::new(14);
    let program_id = Ref::new(15);
    let font_bytes: &[u8] = if use_cff_outlines {
        include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf")
    } else {
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf")
    };
    pdf.type0_font(font_id)
        .base_font(Name(b"FixtureDejaVuSans"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(descendant_id);
    pdf.cid_font(descendant_id)
        .subtype(CidFontType::Type2)
        .base_font(Name(b"FixtureDejaVuSans"))
        .system_info(SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        })
        .font_descriptor(descriptor_id)
        .default_width(1_000.0);
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixtureDejaVuSans"));
    descriptor.pair(
        if use_open_type_stream || use_cff_outlines {
            Name(b"FontFile3")
        } else {
            Name(b"FontFile2")
        },
        program_id,
    );
    descriptor.finish();
    let mut program = pdf.stream(program_id, font_bytes);
    if use_open_type_stream || use_cff_outlines {
        program.pair(Name(b"Subtype"), Name(b"OpenType"));
    }
    program.finish();

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"GapFont"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(if use_notdef { &[0, 0] } else { &[0, 65] }));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

#[derive(Clone, Copy)]
enum StreamCidToGidFixture {
    Valid,
    ValidFlate,
    Zero,
    OutOfRange,
    Short,
    Odd,
    MalformedFlate,
}

fn write_stream_cid_to_gid_type2_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    kind: StreamCidToGidFixture,
) {
    let descendant_id = Ref::new(13);
    let descriptor_id = Ref::new(14);
    let program_id = Ref::new(15);
    let map_id = Ref::new(16);
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
    let face = ttf_parser::Face::parse(font_bytes, 0).expect("fixture font parses");
    let a_gid = face.glyph_index('A').expect("fixture contains A").0;

    pdf.type0_font(font_id)
        .base_font(Name(b"FixtureDejaVuSans"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(descendant_id);
    pdf.cid_font(descendant_id)
        .subtype(CidFontType::Type2)
        .base_font(Name(b"FixtureDejaVuSans"))
        .system_info(SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        })
        .font_descriptor(descriptor_id)
        .default_width(1_000.0)
        .cid_to_gid_map_stream(map_id);
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixtureDejaVuSans"));
    descriptor.pair(Name(b"FontFile2"), program_id);
    descriptor.finish();
    pdf.stream(program_id, font_bytes);

    let mut map = vec![0_u8; 132];
    let mapped_gid = match kind {
        StreamCidToGidFixture::Valid | StreamCidToGidFixture::ValidFlate => a_gid,
        StreamCidToGidFixture::OutOfRange => u16::MAX,
        StreamCidToGidFixture::Zero
        | StreamCidToGidFixture::Short
        | StreamCidToGidFixture::Odd
        | StreamCidToGidFixture::MalformedFlate => 0,
    };
    map[130..132].copy_from_slice(&mapped_gid.to_be_bytes());
    if matches!(kind, StreamCidToGidFixture::Short) {
        map.truncate(2);
    } else if matches!(kind, StreamCidToGidFixture::Odd) {
        map.truncate(131);
    }
    match kind {
        StreamCidToGidFixture::ValidFlate => {
            let compressed = deflate(&map);
            pdf.stream(map_id, &compressed).filter(Filter::FlateDecode);
        }
        StreamCidToGidFixture::MalformedFlate => {
            pdf.stream(map_id, b"not a zlib stream")
                .filter(Filter::FlateDecode);
        }
        StreamCidToGidFixture::Valid
        | StreamCidToGidFixture::Zero
        | StreamCidToGidFixture::OutOfRange
        | StreamCidToGidFixture::Short
        | StreamCidToGidFixture::Odd => {
            pdf.stream(map_id, &map);
        }
    }

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"GapFont"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(&[0, 65]));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("compress fixture bytes");
    encoder.finish().expect("finish fixture compression")
}

fn write_missing_glyph_text(pdf: &mut Pdf, content_id: Ref) {
    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"GapFont"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(&[0xFF, 0xFF]));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn write_identity_cff_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    missing_used_cid: bool,
    use_notdef: bool,
    malformed_charstring: bool,
) {
    let descendant_id = Ref::new(13);
    let descriptor_id = Ref::new(14);
    let program_id = Ref::new(15);
    let cid_set_id = Ref::new(16);
    let cff = minimal_cid_cff(if missing_used_cid { 64 } else { 65 }, malformed_charstring);
    pdf.type0_font(font_id)
        .base_font(Name(b"FixtureCff"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(descendant_id);
    pdf.cid_font(descendant_id)
        .subtype(CidFontType::Type0)
        .base_font(Name(b"FixtureCff"))
        .system_info(SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        })
        .font_descriptor(descriptor_id)
        .default_width(1_000.0);
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixtureCff"));
    descriptor.pair(Name(b"FontFile3"), program_id);
    descriptor.pair(Name(b"CIDSet"), cid_set_id);
    descriptor.finish();
    let mut program = pdf.stream(program_id, &cff);
    program.pair(Name(b"Subtype"), Name(b"CIDFontType0C"));
    program.finish();
    pdf.stream(cid_set_id, &[0xFF; 9]);

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"CffFont"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(if use_notdef { &[0, 0] } else { &[0, 65] }));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn minimal_cid_cff(mapped_cid: u16, malformed_charstring: bool) -> Vec<u8> {
    let mut data = vec![1, 0, 4, 1];
    append_cff_index(&mut data, &[b"Fixture"]);

    // Fixed-width DICT operands keep these offsets stable while we calculate them.
    let top_dict_len = 43;
    let name_index_len = data.len() - 4;
    let top_index_len = 2 + 1 + 2 + top_dict_len;
    let string_index_len = 2 + 1 + 3 + b"AdobeIdentity".len();
    let global_subr_len = 2;
    let charset_offset = 4 + name_index_len + top_index_len + string_index_len + global_subr_len;
    let fd_array_offset = charset_offset + 3;
    let font_dict_len = 11;
    let fd_array_len = 2 + 1 + 2 + font_dict_len;
    let charstrings_offset = fd_array_offset + fd_array_len;
    let charstrings_len = 2 + 1 + 3 + 2;
    let private_dict_offset = charstrings_offset + charstrings_len;

    let mut top_dict = Vec::with_capacity(top_dict_len);
    for value in [391_i32, 392, 0] {
        encode_cff_dict_i32(&mut top_dict, value);
    }
    top_dict.extend_from_slice(&[12, 30]); // ROS: Adobe, Identity, supplement 0.
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(charset_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(15); // charset
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(charstrings_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(17); // CharStrings
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(fd_array_offset).expect("small CFF fixture offset"),
    );
    top_dict.extend_from_slice(&[12, 36]); // FDArray
    encode_cff_dict_i32(&mut top_dict, 2);
    top_dict.extend_from_slice(&[12, 34]); // CIDCount
    assert_eq!(top_dict.len(), top_dict_len);
    append_cff_index(&mut data, &[&top_dict]);
    append_cff_index(&mut data, &[b"Adobe", b"Identity"]);
    append_cff_index(&mut data, &[]);
    assert_eq!(data.len(), charset_offset);
    data.push(0); // charset format 0
    data.extend_from_slice(&mapped_cid.to_be_bytes()); // GID 1 -> chosen CID
    assert_eq!(data.len(), fd_array_offset);
    let mut font_dict = Vec::with_capacity(font_dict_len);
    encode_cff_dict_i32(&mut font_dict, 0); // empty Private DICT
    encode_cff_dict_i32(
        &mut font_dict,
        i32::try_from(private_dict_offset).expect("small CFF fixture offset"),
    );
    font_dict.push(18); // Private
    assert_eq!(font_dict.len(), font_dict_len);
    append_cff_index(&mut data, &[&font_dict]);
    assert_eq!(data.len(), charstrings_offset);
    append_cff_index(
        &mut data,
        &[&[14], if malformed_charstring { &[255] } else { &[14] }],
    );
    assert_eq!(data.len(), private_dict_offset);
    data
}

fn write_simple_cff_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    missing_used_code: bool,
    malformed_charstring: bool,
    conflicting_internal_encoding: bool,
) {
    let descriptor_id = Ref::new(13);
    let program_id = Ref::new(14);
    let cff = if conflicting_internal_encoding {
        minimal_conflicting_internal_encoding_cff()
    } else {
        minimal_simple_cff(
            if missing_used_code { 35 } else { 34 },
            malformed_charstring,
        )
    };
    let mut font = pdf.indirect(font_id).dict();
    font.pair(Name(b"Type"), Name(b"Font"));
    font.pair(Name(b"Subtype"), Name(b"Type1"));
    font.pair(Name(b"BaseFont"), Name(b"FixtureSimpleCff"));
    font.pair(Name(b"Encoding"), Name(b"WinAnsiEncoding"));
    font.pair(Name(b"FirstChar"), 65);
    font.pair(Name(b"LastChar"), 65);
    font.insert(Name(b"Widths")).array().item(600);
    font.pair(Name(b"FontDescriptor"), descriptor_id);
    font.finish();
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixtureSimpleCff"));
    descriptor.pair(Name(b"FontFile3"), program_id);
    descriptor.finish();
    let mut program = pdf.stream(program_id, &cff);
    program.pair(Name(b"Subtype"), Name(b"Type1C"));
    program.finish();

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"SimpleCff"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(b"A"));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn minimal_simple_cff(mapped_sid: u16, malformed_charstring: bool) -> Vec<u8> {
    let mut data = vec![1, 0, 4, 1];
    append_cff_index(&mut data, &[b"Fixture"]);
    let top_dict_len = 12;
    let name_index_len = data.len() - 4;
    let top_index_len = 2 + 1 + 2 + top_dict_len;
    let string_index_len = 2;
    let global_subr_len = 2;
    let charset_offset = 4 + name_index_len + top_index_len + string_index_len + global_subr_len;
    let charstrings_offset = charset_offset + 3;
    let mut top_dict = Vec::with_capacity(top_dict_len);
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(charset_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(15);
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(charstrings_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(17);
    append_cff_index(&mut data, &[&top_dict]);
    append_cff_index(&mut data, &[]);
    append_cff_index(&mut data, &[]);
    assert_eq!(data.len(), charset_offset);
    data.push(0);
    data.extend_from_slice(&mapped_sid.to_be_bytes());
    assert_eq!(data.len(), charstrings_offset);
    append_cff_index(
        &mut data,
        &[&[14], if malformed_charstring { &[255] } else { &[14] }],
    );
    data
}

fn minimal_conflicting_internal_encoding_cff() -> Vec<u8> {
    let mut data = vec![1, 0, 4, 1];
    append_cff_index(&mut data, &[b"Fixture"]);
    let top_dict_len = 18;
    let name_index_len = data.len() - 4;
    let top_index_len = 2 + 1 + 2 + top_dict_len;
    let string_index_len = 2 + 1 + 2 + b"foo".len();
    let global_subr_len = 2;
    let charset_offset = 4 + name_index_len + top_index_len + string_index_len + global_subr_len;
    let encoding_offset = charset_offset + 3;
    let charstrings_offset = encoding_offset + 3;
    let mut top_dict = Vec::with_capacity(top_dict_len);
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(charset_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(15); // charset
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(encoding_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(16); // Encoding
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(charstrings_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(17); // CharStrings
    assert_eq!(top_dict.len(), top_dict_len);
    append_cff_index(&mut data, &[&top_dict]);
    append_cff_index(&mut data, &[b"foo"]); // SID 391
    append_cff_index(&mut data, &[]);
    assert_eq!(data.len(), charset_offset);
    data.push(0); // charset format 0
    data.extend_from_slice(&391_u16.to_be_bytes()); // GID 1 -> /foo
    assert_eq!(data.len(), encoding_offset);
    data.extend_from_slice(&[0, 1, 65]); // Encoding format 0: code 65 -> GID 1
    assert_eq!(data.len(), charstrings_offset);
    append_cff_index(&mut data, &[&[14], &[14]]);
    data
}

fn write_predefined_charset_cff_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    missing_used_code: bool,
) {
    let descriptor_id = Ref::new(13);
    let program_id = Ref::new(14);
    let cff = minimal_predefined_charset_cff();
    let mut font = pdf.indirect(font_id).dict();
    font.pair(Name(b"Type"), Name(b"Font"));
    font.pair(Name(b"Subtype"), Name(b"MMType1"));
    font.pair(Name(b"BaseFont"), Name(b"FixturePredefinedCff"));
    font.pair(Name(b"Encoding"), Name(b"MacRomanEncoding"));
    font.pair(Name(b"FirstChar"), 65);
    font.pair(Name(b"LastChar"), 66);
    font.insert(Name(b"Widths")).array().items([600, 600]);
    font.pair(Name(b"FontDescriptor"), descriptor_id);
    font.finish();
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixturePredefinedCff"));
    descriptor.pair(Name(b"FontFile3"), program_id);
    descriptor.finish();
    let mut program = pdf.stream(program_id, &cff);
    program.pair(Name(b"Subtype"), Name(b"Type1C"));
    program.finish();

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"SimpleCff"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(if missing_used_code { b"B" } else { b"A" }));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn minimal_predefined_charset_cff() -> Vec<u8> {
    minimal_iso_adobe_cff(35) // .notdef through A in the ISOAdobe charset.
}

fn write_predefined_win_ansi_cff_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    missing_used_code: bool,
) {
    let descriptor_id = Ref::new(13);
    let program_id = Ref::new(14);
    let cff = minimal_iso_adobe_cff(203); // Includes adieresis (GID/SID 202), not aring.
    let mut font = pdf.indirect(font_id).dict();
    font.pair(Name(b"Type"), Name(b"Font"));
    font.pair(Name(b"Subtype"), Name(b"Type1"));
    font.pair(Name(b"BaseFont"), Name(b"FixturePredefinedWinAnsiCff"));
    font.pair(Name(b"Encoding"), Name(b"WinAnsiEncoding"));
    font.pair(Name(b"FirstChar"), 228);
    font.pair(Name(b"LastChar"), 229);
    font.insert(Name(b"Widths")).array().items([600, 600]);
    font.pair(Name(b"FontDescriptor"), descriptor_id);
    font.finish();
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixturePredefinedWinAnsiCff"));
    descriptor.pair(Name(b"FontFile3"), program_id);
    descriptor.finish();
    let mut program = pdf.stream(program_id, &cff);
    program.pair(Name(b"Subtype"), Name(b"Type1C"));
    program.finish();

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"SimpleCff"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(if missing_used_code { b"\xe5" } else { b"\xe4" }));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn minimal_iso_adobe_cff(glyph_count: usize) -> Vec<u8> {
    assert!((1..=229).contains(&glyph_count));

    let mut data = vec![1, 0, 4, 1];
    append_cff_index(&mut data, &[b"Fixture"]);
    let top_dict_len = 6;
    let name_index_len = data.len() - 4;
    let top_index_len = 2 + 1 + 2 + top_dict_len;
    let string_index_len = 2;
    let global_subr_len = 2;
    let charstrings_offset =
        4 + name_index_len + top_index_len + string_index_len + global_subr_len;
    let mut top_dict = Vec::with_capacity(top_dict_len);
    encode_cff_dict_i32(
        &mut top_dict,
        i32::try_from(charstrings_offset).expect("small CFF fixture offset"),
    );
    top_dict.push(17); // CharStrings; omitted charset defaults to ISOAdobe.
    append_cff_index(&mut data, &[&top_dict]);
    append_cff_index(&mut data, &[]);
    append_cff_index(&mut data, &[]);
    assert_eq!(data.len(), charstrings_offset);
    let programs = vec![vec![14_u8]; glyph_count];
    let program_refs = programs.iter().map(Vec::as_slice).collect::<Vec<_>>();
    append_cff_index(&mut data, &program_refs);
    data
}

fn write_simple_truetype_fixture(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    missing_used_code: bool,
    symbolic: bool,
) {
    let descriptor_id = Ref::new(13);
    let program_id = Ref::new(14);
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
    let mut font = pdf.indirect(font_id).dict();
    font.pair(Name(b"Type"), Name(b"Font"));
    font.pair(Name(b"Subtype"), Name(b"TrueType"));
    font.pair(Name(b"BaseFont"), Name(b"FixtureSimpleTrueType"));
    if missing_used_code {
        let mut encoding = font.insert(Name(b"Encoding")).dict();
        encoding.pair(Name(b"BaseEncoding"), Name(b"WinAnsiEncoding"));
        let mut differences = encoding.insert(Name(b"Differences")).array();
        differences.item(65);
        differences.item(Name(b"uni4E00"));
        differences.finish();
        encoding.finish();
    } else {
        font.pair(Name(b"Encoding"), Name(b"WinAnsiEncoding"));
    }
    font.pair(Name(b"FirstChar"), 65);
    font.pair(Name(b"LastChar"), 65);
    font.insert(Name(b"Widths")).array().item(600);
    font.pair(Name(b"FontDescriptor"), descriptor_id);
    font.finish();
    let mut descriptor = pdf.indirect(descriptor_id).dict();
    descriptor.pair(Name(b"Type"), Name(b"FontDescriptor"));
    descriptor.pair(Name(b"FontName"), Name(b"FixtureSimpleTrueType"));
    if symbolic {
        descriptor.pair(Name(b"Flags"), 4);
    }
    descriptor.pair(Name(b"FontFile2"), program_id);
    descriptor.finish();
    pdf.stream(program_id, font_bytes);

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"SimpleTrueType"), 12.0);
    content.next_line(72.0, 720.0);
    content.show(Str(b"A"));
    content.end_text();
    pdf.stream(content_id, &content.finish());
}

fn append_cff_index(data: &mut Vec<u8>, entries: &[&[u8]]) {
    let count = u16::try_from(entries.len()).expect("small CFF fixture index");
    data.extend_from_slice(&count.to_be_bytes());
    if entries.is_empty() {
        return;
    }
    data.push(1);
    let mut offset = 1_u8;
    data.push(offset);
    for entry in entries {
        let entry_len = u8::try_from(entry.len()).expect("small CFF fixture entry");
        offset = offset.checked_add(entry_len).expect("small CFF fixture");
        data.push(offset);
    }
    for entry in entries {
        data.extend_from_slice(entry);
    }
}

fn encode_cff_dict_i32(output: &mut Vec<u8>, value: i32) {
    output.push(29);
    output.extend_from_slice(&value.to_be_bytes());
}

fn write_nested_identity_missing_glyph_fault(
    pdf: &mut Pdf,
    font_id: Ref,
    content_id: Ref,
    form_id: Ref,
) {
    write_identity_font_with_gap(pdf, font_id);
    let mut page_content = Content::new();
    page_content.x_object(Name(b"GapForm"));
    pdf.stream(content_id, &page_content.finish());

    let mut form_content = Content::new();
    form_content.begin_text();
    form_content.set_font(Name(b"GapFont"), 12.0);
    form_content.show(Str(&[0xFF, 0xFF]));
    form_content.end_text();
    let form_content = form_content.finish();
    let mut form = pdf.form_xobject(form_id, &form_content);
    form.bbox(Rect::new(0.0, 0.0, 48.0, 48.0));
    form.resources().fonts().pair(Name(b"GapFont"), font_id);
    form.finish();
}

fn write_appearance_identity_missing_glyph_fault(
    pdf: &mut Pdf,
    font_id: Ref,
    annotation_id: Ref,
    appearance_id: Ref,
) {
    write_identity_font_with_gap(pdf, font_id);
    let mut annotation = pdf.indirect(annotation_id).dict();
    annotation.pair(Name(b"Type"), Name(b"Annot"));
    annotation.pair(Name(b"Subtype"), Name(b"Stamp"));
    annotation.pair(Name(b"Rect"), Rect::new(0.0, 0.0, 48.0, 48.0));
    annotation
        .insert(Name(b"AP"))
        .dict()
        .pair(Name(b"N"), appearance_id);
    annotation.finish();

    let mut appearance_content = Content::new();
    appearance_content.begin_text();
    appearance_content.set_font(Name(b"GapFont"), 12.0);
    appearance_content.show(Str(&[0xFF, 0xFF]));
    appearance_content.end_text();
    let appearance_content = appearance_content.finish();
    let mut appearance = pdf.form_xobject(appearance_id, &appearance_content);
    appearance.bbox(Rect::new(0.0, 0.0, 48.0, 48.0));
    appearance
        .resources()
        .fonts()
        .pair(Name(b"GapFont"), font_id);
    appearance.finish();
}

fn write_action_and_annotation_faults(
    pdf: &mut Pdf,
    fault: FixtureFault,
    action_id: Ref,
    annotation_id: Ref,
) {
    if let FixtureFault::Action(action_fault) = fault {
        let mut action = pdf.indirect(action_id).dict();
        action.pair(Name(b"Type"), Name(b"Action"));
        action.pair(
            Name(b"S"),
            Name(match action_fault {
                ActionFault::JavaScript => b"JavaScript",
                ActionFault::Launch => b"Launch",
            }),
        );
        if matches!(action_fault, ActionFault::JavaScript) {
            action.pair(Name(b"JS"), TextStr("app.alert('fault')"));
        }
        action.finish();
    }
    if matches!(fault, FixtureFault::Annotation) {
        let mut annotation = pdf.annotation(annotation_id);
        annotation
            .subtype(AnnotationType::Text)
            .rect(Rect::new(0.0, 0.0, 8.0, 8.0))
            .contents(TextStr("Annotation fault"));
        annotation.finish();
    }
}

fn write_malformed_page_icc_fault(
    pdf: &mut Pdf,
    fault: FixtureFault,
    content_id: Ref,
    bad_page_profile_id: Ref,
) {
    if !matches!(fault, FixtureFault::MalformedPageIcc) {
        return;
    }
    let mut content = Content::new();
    content.set_fill_color_space(Name(b"BadIcc"));
    content.set_fill_color([0.1, 0.2, 0.3]);
    content.rect(72.0, 650.0, 100.0, 100.0);
    content.fill_nonzero();
    pdf.stream(content_id, &content.finish());
    pdf.icc_profile(bad_page_profile_id, b"not an ICC profile")
        .n(3);
}

fn with_encryption_trailer_marker(mut candidate: Vec<u8>) -> Vec<u8> {
    const TRAILER_START: &[u8] = b"trailer\n<<";
    const ENCRYPTION_MARKER: &[u8] = b" /Encrypt << /Filter /Standard >>";
    let insertion = candidate
        .windows(TRAILER_START.len())
        .position(|window| window == TRAILER_START)
        .map(|position| position + TRAILER_START.len())
        .expect("pdf-writer emits a plain trailer dictionary");
    candidate.splice(insertion..insertion, ENCRYPTION_MARKER.iter().copied());
    candidate
}

#[test]
fn pdf_oxide_accepts_the_control_and_rejects_the_single_fault_variant() {
    let key = EvaluationKey::from_bytes([11; 32]);
    let (control, profile_hash) = differential_fixture(FixtureFault::None, XMP);
    let (missing_output_intent, second_profile_hash) =
        differential_fixture(FixtureFault::MissingOutputIntent, XMP);
    assert_eq!(profile_hash, second_profile_hash);
    let validator = PdfOxideValidator::new(profile_hash);

    let accepted = run_independent_validation(
        &validator,
        ValidationRequest::new(key, &control, PrintStandard::PdfX4),
    )
    .expect("control fixture is parseable");
    let rejected = run_independent_validation(
        &validator,
        ValidationRequest::new(key, &missing_output_intent, PrintStandard::PdfX4),
    )
    .expect("fault fixture is parseable");

    assert_eq!(accepted.verdict(), ValidationVerdict::Passed);
    assert_eq!(rejected.verdict(), ValidationVerdict::Failed);
    assert!(
        rejected
            .findings()
            .iter()
            .any(|finding| finding.contains("OutputIntents array is required"))
    );
    assert_ne!(accepted.artifact_hash(), rejected.artifact_hash());
}

#[test]
fn local_rule_requires_the_exact_decoded_cmyk_destination_profile() {
    let key = EvaluationKey::from_bytes([14; 32]);
    let (missing_profile, profile_hash) =
        differential_fixture(FixtureFault::MissingDestinationProfile, XMP);
    let (control, _) = differential_fixture(FixtureFault::None, XMP);

    let missing = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(key, &missing_profile, PrintStandard::PdfX4),
    )
    .expect("missing-profile fixture is parseable");
    let mismatched = run_independent_validation(
        &PdfOxideValidator::new(ArtifactHash::of(b"different selected profile")),
        ValidationRequest::new(key, &control, PrintStandard::PdfX4),
    )
    .expect("mismatched-profile fixture is parseable");

    assert_eq!(missing.verdict(), ValidationVerdict::Failed);
    assert!(
        missing
            .findings()
            .iter()
            .any(|finding| finding.contains("readable N=4 CMYK DestOutputProfile"))
    );
    assert_eq!(mismatched.verdict(), ValidationVerdict::Failed);
    assert!(
        mismatched
            .findings()
            .iter()
            .any(|finding| finding.contains("do not match the selected printer profile"))
    );
}

#[test]
fn pdf_oxide_xmp_identity_gap_is_reproduced_as_a_blocking_fixture() {
    let key = EvaluationKey::from_bytes([12; 32]);
    let (missing_xmp_identity, profile_hash) =
        differential_fixture(FixtureFault::None, XMP_WITHOUT_PDFX_ID);
    let validator = PdfOxideValidator::new(profile_hash);

    let result = run_independent_validation(
        &validator,
        ValidationRequest::new(key, &missing_xmp_identity, PrintStandard::PdfX4),
    )
    .expect("fault fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(
        result
            .findings()
            .iter()
            .any(|finding| finding.contains("GTS_PDFXVersion"))
    );
}

#[test]
fn production_profile_gate_rejects_the_header_only_profile_that_pdf_oxide_accepts() {
    let profile = synthetic_cmyk_output_profile();
    assert!(inspect_output_intent(&profile).is_err());

    let (validator_control, profile_hash) = differential_fixture(FixtureFault::None, XMP);
    assert_eq!(profile_hash, ArtifactHash::of(&profile));
    let validator = PdfOxideValidator::new(ArtifactHash::of(&profile));
    let validator_result = run_independent_validation(
        &validator,
        ValidationRequest::new(
            EvaluationKey::from_bytes([13; 32]),
            &validator_control,
            PrintStandard::PdfX4,
        ),
    )
    .expect("validator control report");
    assert_eq!(validator_result.verdict(), ValidationVerdict::Passed);
}

#[test]
fn local_font_audit_rejects_an_unembedded_standard_font() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::UnembeddedFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([15; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("unembedded-font fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(
        result
            .findings()
            .iter()
            .all(|finding| !finding.contains("XFONT-001")),
        "the upstream standard-font exemption should make this an independent local-rule proof"
    );
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /BadFont has no readable embedded font program")
    }));
}

#[test]
fn local_font_audit_rejects_an_unembedded_font_inside_a_form_xobject() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::NestedUnembeddedFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([25; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("nested unembedded-font fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /NestedBadFont has no readable embedded font program")
    }));
}

#[test]
fn local_font_audit_rejects_an_unembedded_font_in_inherited_page_resources() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::InheritedUnembeddedFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([26; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("inherited unembedded-font fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /TreeFont has no readable embedded font program")
    }));
}

#[test]
fn local_font_audit_rejects_an_unembedded_annotation_appearance_font() {
    for (fault, appearance_name) in [
        (FixtureFault::AnnotationAppearanceUnembeddedFont, "/N"),
        (FixtureFault::AnnotationAppearanceStateUnembeddedFont, "/On"),
    ] {
        let (candidate, profile_hash) = differential_fixture(fault, XMP);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(
                EvaluationKey::from_bytes([27; 32]),
                &candidate,
                PrintStandard::PdfX4,
            ),
        )
        .expect("annotation appearance font fixture is parseable");

        assert_eq!(result.verdict(), ValidationVerdict::Failed);
        assert!(result.findings().iter().any(|finding| {
            finding.contains(&format!(
                "appearance {appearance_name} resources font /AppearanceBadFont"
            )) && finding.contains("no readable embedded font program")
        }));
    }
}

#[test]
fn local_font_audit_rejects_a_corrupt_embedded_truetype_program() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::CorruptEmbeddedTrueTypeFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([28; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("corrupt embedded TrueType fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /BadFont has no readable embedded font program")
    }));
}

#[test]
fn local_font_audit_rejects_a_used_identity_cid_missing_from_the_program() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MissingUsedIdentityGlyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([29; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("missing-glyph fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /GapFont uses CID 65535, which is absent from its embedded program")
    }));
}

#[test]
fn local_font_audit_checks_used_identity_cids_in_nested_and_appearance_streams() {
    for (fault, owner) in [
        (
            FixtureFault::NestedMissingUsedIdentityGlyph,
            "form /GapForm",
        ),
        (
            FixtureFault::AppearanceMissingUsedIdentityGlyph,
            "annotation 1 appearance /N",
        ),
    ] {
        let (candidate, profile_hash) = differential_fixture(fault, XMP);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(
                EvaluationKey::from_bytes([30; 32]),
                &candidate,
                PrintStandard::PdfX4,
            ),
        )
        .expect("nested missing-glyph fixture is parseable");
        assert_eq!(result.verdict(), ValidationVerdict::Failed);
        assert!(result.findings().iter().any(|finding| {
            finding.contains(owner)
                && finding.contains(
                    "font /GapFont uses CID 65535, which is absent from its embedded program",
                )
        }));
    }
}

#[test]
fn local_font_audit_accepts_the_default_identity_mapping_for_cidfonttype2() {
    for fault in [
        FixtureFault::ValidDefaultIdentityType2,
        FixtureFault::ValidOpenTypeIdentityType2,
    ] {
        let (candidate, profile_hash) = differential_fixture(fault, XMP);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(
                EvaluationKey::from_bytes([42; 32]),
                &candidate,
                PrintStandard::PdfX4,
            ),
        )
        .expect("default-Identity CIDFontType2 control is parseable");

        assert_eq!(
            result.verdict(),
            ValidationVerdict::Passed,
            "default-Identity CIDFontType2 findings: {:?}",
            result.findings()
        );
    }
}

#[test]
fn local_font_audit_accepts_stream_cid_to_gid_maps_under_identity_encoding() {
    for fault in [
        FixtureFault::ValidStreamCidToGidType2,
        FixtureFault::ValidFlateStreamCidToGidType2,
    ] {
        let (candidate, profile_hash) = differential_fixture(fault, XMP);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(
                EvaluationKey::from_bytes([56; 32]),
                &candidate,
                PrintStandard::PdfX4,
            ),
        )
        .expect("stream CIDToGIDMap control is parseable");

        assert_eq!(
            result.verdict(),
            ValidationVerdict::Passed,
            "stream CIDToGIDMap findings: {:?}",
            result.findings()
        );
    }
}

#[test]
fn local_font_audit_rejects_unavailable_stream_cid_to_gid_mappings() {
    for (fault, expected) in [
        (
            FixtureFault::ZeroStreamCidToGidType2,
            "CID 65, which maps to unavailable GID 0",
        ),
        (
            FixtureFault::OutOfRangeStreamCidToGidType2,
            "CID 65, which maps to unavailable GID 65535",
        ),
        (
            FixtureFault::ShortStreamCidToGidType2,
            "CID 65, which maps to unavailable GID 0",
        ),
    ] {
        let (candidate, profile_hash) = differential_fixture(fault, XMP);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(
                EvaluationKey::from_bytes([57; 32]),
                &candidate,
                PrintStandard::PdfX4,
            ),
        )
        .expect("invalid stream CIDToGIDMap fixture is parseable");

        assert_eq!(result.verdict(), ValidationVerdict::Failed);
        assert!(
            result
                .findings()
                .iter()
                .any(|finding| finding.contains(expected)),
            "stream CIDToGIDMap findings: {:?}",
            result.findings()
        );
    }
}

#[test]
fn local_font_audit_rejects_malformed_stream_cid_to_gid_maps() {
    for (fault, expected) in [
        (
            FixtureFault::OddStreamCidToGidType2,
            "CIDToGIDMap contains an incomplete two-byte GID",
        ),
        (
            FixtureFault::MalformedFlateCidToGidType2,
            "CIDToGIDMap stream cannot be decoded",
        ),
    ] {
        let (candidate, profile_hash) = differential_fixture(fault, XMP);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(
                EvaluationKey::from_bytes([58; 32]),
                &candidate,
                PrintStandard::PdfX4,
            ),
        )
        .expect("malformed stream CIDToGIDMap fixture is parseable");

        assert_eq!(result.verdict(), ValidationVerdict::Failed);
        assert!(
            result
                .findings()
                .iter()
                .any(|finding| finding.contains(expected)),
            "stream CIDToGIDMap findings: {:?}",
            result.findings()
        );
    }
}

#[test]
fn local_font_audit_rejects_notdef_for_cidfonttype2() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::UsedNotdefIdentityType2, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([50; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("CIDFontType2 .notdef fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /GapFont uses CID 0")
            && finding.contains("absent from its embedded program")
    }));
}

#[test]
fn local_font_audit_rejects_cff_outlines_claimed_as_cidfonttype2() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::CffOpenTypeIdentityType2, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([55; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("CFF-flavored OpenType CIDFontType2 fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /GapFont used-glyph coverage is unproved")
            && finding.contains("does not contain TrueType outlines")
    }));
}

#[test]
fn local_font_audit_accepts_a_used_identity_cid_in_a_cff_program() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::ValidIdentityCffFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([38; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("CIDFontType0/CFF control is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "CIDFontType0/CFF control findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_a_used_identity_cid_missing_from_a_cff_charset() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MissingUsedIdentityCffGlyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([39; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("CIDFontType0/CFF missing-glyph fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /CffFont uses CID 65")
            && finding.contains("absent from its embedded CFF program")
    }));
}

#[test]
fn local_font_audit_rejects_notdef_for_cidfonttype0_cff() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::UsedNotdefIdentityCff, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([51; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("CIDFontType0/CFF .notdef fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /CffFont uses CID 0")
            && finding.contains("absent from its embedded CFF program")
    }));
}

#[test]
fn local_font_audit_rejects_a_malformed_identity_cff_charstring() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MalformedIdentityCffCharstring, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([52; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("malformed CIDFontType0/CFF fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /CffFont used-glyph coverage is unproved")
            && finding.contains("malformed Type 2 CharString")
    }));
}

#[test]
fn local_font_audit_accepts_a_used_simple_cff_glyph() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::ValidSimpleCffFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([40; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("simple Type1C control is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "simple Type1C control findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_a_used_simple_code_absent_from_a_cff_charset() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MissingUsedSimpleCffGlyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([41; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("simple Type1C missing-glyph fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /SimpleCff used-glyph coverage is unproved")
            && finding.contains("encoding does not resolve any embedded glyphs")
    }));
}

#[test]
fn local_font_audit_rejects_a_malformed_simple_cff_charstring() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MalformedSimpleCffCharstring, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([53; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("malformed simple Type1C fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /SimpleCff used-glyph coverage is unproved")
            && finding.contains("malformed Type 2 CharString")
    }));
}

#[test]
fn local_font_audit_does_not_let_cff_encoding_override_pdf_encoding() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::ConflictingCffInternalEncoding, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([54; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("conflicting CFF/PDF encoding fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /SimpleCff used-glyph coverage is unproved")
            && finding.contains("encoding does not resolve any embedded glyphs")
    }));
}

#[test]
fn local_font_audit_accepts_mac_roman_text_from_a_predefined_cff_charset() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::ValidPredefinedCharsetCffFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([46; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("predefined-charset Type1C control is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "predefined-charset Type1C findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_a_mac_roman_code_absent_from_a_predefined_cff_charset() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MissingUsedPredefinedCharsetCffGlyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([47; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("predefined-charset Type1C missing-glyph fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /SimpleCff uses character code 66")
            && finding.contains("absent from its embedded CFF program")
    }));
}

#[test]
fn local_font_audit_accepts_extended_win_ansi_from_a_predefined_cff_charset() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::ValidPredefinedWinAnsiCffFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([48; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("extended WinAnsi Type1C control is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "extended WinAnsi Type1C findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_extended_win_ansi_absent_from_a_predefined_cff_charset() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MissingUsedPredefinedWinAnsiCffGlyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([49; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("extended WinAnsi Type1C missing-glyph fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /SimpleCff uses character code 229")
            && finding.contains("absent from its embedded CFF program")
    }));
}

#[test]
fn local_font_audit_accepts_a_used_simple_truetype_glyph() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::ValidSimpleTrueTypeFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([43; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("simple TrueType control is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "simple TrueType control findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_an_unmapped_simple_truetype_code() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::MissingUsedSimpleTrueTypeGlyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([44; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("simple TrueType missing-glyph fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /SimpleTrueType uses character code 65")
            && finding.contains("absent from its embedded TrueType program")
    }));
}

#[test]
fn local_font_audit_fails_closed_for_symbolic_simple_truetype() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::SymbolicSimpleTrueTypeFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([56; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("symbolic simple TrueType fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /SimpleTrueType used-glyph coverage is unproved")
            && finding.contains("symbolic simple TrueType")
    }));
}

#[test]
fn local_font_audit_accepts_a_used_type3_charproc() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::ValidType3Font, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([31; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("Type 3 control is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "Type 3 control findings: {:?}",
        result.findings()
    );
    assert!(
        result
            .findings()
            .iter()
            .all(|finding| !finding.contains("Type3Font"))
    );
}

#[test]
fn local_font_audit_rejects_a_used_type3_code_without_a_charproc() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::MissingUsedType3Glyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([32; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("Type 3 missing-glyph fixture is parseable");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(result.findings().iter().any(|finding| {
        finding.contains("font /Type3Font uses character code 66")
            && finding.contains("absent from its CharProcs")
    }));
    assert_eq!(
        result
            .findings()
            .iter()
            .filter(|finding| finding.contains("font /Type3Font uses character code 66"))
            .count(),
        1,
        "repeated missing Type 3 codes must produce one bounded finding"
    );
}

#[test]
fn local_font_audit_rejects_a_malformed_used_type3_charproc() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::MalformedType3CharProc, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([33; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("malformed Type 3 CharProc fixture is parseable as a PDF");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(
        result.findings().iter().any(|finding| {
            finding.contains("Type3Font") && finding.contains("no readable embedded font program")
        }),
        "malformed Type 3 findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_a_type3_font_missing_required_metrics() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::MissingType3FirstChar, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([34; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("Type 3 missing-metrics fixture is parseable as a PDF");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(
        result.findings().iter().any(|finding| {
            finding.contains("Type3Font") && finding.contains("no readable embedded font program")
        }),
        "Type 3 missing-metrics findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_a_type3_code_outside_declared_range() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::Type3OutOfRangeGlyph, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([35; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("Type 3 out-of-range fixture is parseable as a PDF");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(
        result
            .findings()
            .iter()
            .any(|finding| finding.contains("character code 64") && finding.contains("FirstChar")),
        "Type 3 out-of-range findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_an_unembedded_font_in_type3_resources() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::Type3NestedUnembeddedFont, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([36; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("Type 3 nested-font fixture is parseable as a PDF");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(
        result.findings().iter().any(|finding| {
            finding.contains("NestedBad") && finding.contains("no readable embedded font program")
        }),
        "Type 3 nested-font findings: {:?}",
        result.findings()
    );
}

#[test]
fn local_font_audit_rejects_an_unused_type3_font_with_malformed_encoding() {
    let (candidate, profile_hash) =
        differential_fixture(FixtureFault::UnusedMalformedType3Encoding, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([37; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("unused Type 3 malformed-Encoding fixture is parseable as a PDF");

    assert_eq!(result.verdict(), ValidationVerdict::Failed);
    assert!(
        result.findings().iter().any(|finding| {
            finding.contains("Type3Font") && finding.contains("no readable embedded font program")
        }),
        "unused malformed Type 3 findings: {:?}",
        result.findings()
    );
}

#[test]
fn pdf_oxide_rejects_each_page_box_single_fault() {
    let key = EvaluationKey::from_bytes([16; 32]);
    for (fault, expected_code) in [
        (PageBoxFault::MissingMedia, "XBOX-004"),
        (PageBoxFault::MissingTrim, "XBOX-001"),
        (PageBoxFault::BleedOutsideMedia, "XBOX-002"),
        (PageBoxFault::TrimOutsideBleed, "XBOX-003"),
    ] {
        let (candidate, profile_hash) = differential_fixture(FixtureFault::PageBox(fault), XMP);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(key, &candidate, PrintStandard::PdfX4),
        )
        .expect("page-box fault fixture is parseable");

        assert_eq!(result.verdict(), ValidationVerdict::Failed);
        assert!(
            result
                .findings()
                .iter()
                .any(|finding| finding.contains(expected_code)),
            "expected {expected_code} for the single page-box fault"
        );
    }
}

#[test]
fn pdf_oxide_rejects_encryption_and_prohibited_open_actions() {
    let key = EvaluationKey::from_bytes([17; 32]);
    let (control, profile_hash) = differential_fixture(FixtureFault::None, XMP);
    let encryption = with_encryption_trailer_marker(control);
    let encrypted = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(key, &encryption, PrintStandard::PdfX4),
    )
    .expect("encryption-marker fixture is parseable");
    assert_eq!(encrypted.verdict(), ValidationVerdict::Failed);
    assert!(
        encrypted
            .findings()
            .iter()
            .any(|finding| finding.contains("XCONT-001"))
    );

    for (fault, expected_code) in [
        (ActionFault::JavaScript, "XCONT-002"),
        (ActionFault::Launch, "XACTION-001"),
    ] {
        let (candidate, candidate_profile_hash) =
            differential_fixture(FixtureFault::Action(fault), XMP);
        assert_eq!(candidate_profile_hash, profile_hash);
        let result = run_independent_validation(
            &PdfOxideValidator::new(profile_hash),
            ValidationRequest::new(key, &candidate, PrintStandard::PdfX4),
        )
        .expect("prohibited-action fixture is parseable");
        assert_eq!(result.verdict(), ValidationVerdict::Failed);
        assert!(
            result
                .findings()
                .iter()
                .any(|finding| finding.contains(expected_code)),
            "expected {expected_code} for the prohibited action"
        );
    }
}

#[test]
fn publisher_text_policy_blocks_the_upstream_annotation_warning() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::Annotation, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([18; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("annotation fault fixture is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "the pinned upstream validator currently treats this annotation rule as nonblocking"
    );
    assert!(
        result
            .findings()
            .iter()
            .any(|finding| finding.contains("XANNOT-001"))
    );

    let public_core = audit_public_pdfx_objects(&candidate)
        .expect("the public-core audit can inspect the same annotation fixture");
    assert!(public_core.findings().iter().any(|finding| {
        finding.contains("PUBLISHER-SAFE-ANNOT-003") && finding.contains("page 1 annotation 1")
    }));
    assert!(
        public_core
            .findings()
            .iter()
            .all(|finding| !finding.contains("PUBLISHER-SAFE-ANNOT-001"))
    );
}

#[test]
fn malformed_page_icc_profile_is_not_detected_by_the_upstream_validator() {
    let (candidate, profile_hash) = differential_fixture(FixtureFault::MalformedPageIcc, XMP);
    let result = run_independent_validation(
        &PdfOxideValidator::new(profile_hash),
        ValidationRequest::new(
            EvaluationKey::from_bytes([19; 32]),
            &candidate,
            PrintStandard::PdfX4,
        ),
    )
    .expect("malformed page-ICC fixture is parseable");

    assert_eq!(
        result.verdict(),
        ValidationVerdict::Passed,
        "the pinned upstream validator currently checks only the ICC stream's /N entry"
    );
    assert!(
        result
            .findings()
            .iter()
            .all(|finding| !finding.contains("XCOLOR-005"))
    );
}

#[test]
fn malformed_active_icc_profile_is_rejected_by_the_littlecms_authority() {
    let (candidate, _) = differential_fixture(FixtureFault::MalformedPageIcc, XMP);

    let audit = audit_active_icc_profiles(&candidate).expect("fixture is safely traversable");

    assert_eq!(audit.profiles_checked(), 1);
    assert!(
        audit
            .findings()
            .iter()
            .any(|finding| finding.contains("LittleCMS rejected"))
    );
}
