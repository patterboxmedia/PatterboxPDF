//! Read-only PDF/X-4 validation through the permissively licensed `PDFOxide` crate.
//!
//! This in-process adapter is the U4A2 feasibility boundary. The packaged
//! application must call the same validator through the constrained worker
//! defined by U4B before it can enable a PDF/X-4 compliance label.

use pdf_oxide::{
    compliance::{PdfXLevel, XErrorCode, validate_pdf_x},
    content::{Operator, parse_content_stream},
    document::PdfDocument,
    object::{Object, ObjectRef},
};
use std::collections::HashSet;
use validator_boundary::{
    DIAGNOSTIC_NAME, DIAGNOSTIC_RULES_IDENTITY, DIAGNOSTIC_SOURCE_IDENTITY, DIAGNOSTIC_VERSION,
};

use crate::{
    ArtifactHash, IndependentValidator, PrintStandard, ValidationError, ValidationRequest,
    ValidationVerdict, ValidatorIdentity, ValidatorResponse,
    validator_cff::{CffCharsetProof, inspect_cff_program},
    validator_glyphs::audit_used_glyphs,
};

const MAX_FINDINGS: usize = 512;
const MAX_AUDITED_FONTS: usize = 4_096;
const MAX_RESOURCE_SCOPES: usize = 4_096;
const MAX_APPEARANCE_OBJECTS: usize = 100_000;
const MAX_REFERENCE_DEPTH: usize = 16;
const MAX_REPORTED_FONT_NAME_CHARS: usize = 64;
const MAX_TYPE3_CHAR_PROCS: usize = 256;
const MAX_TYPE3_DIFFERENCES: usize = 512;

#[derive(Debug, Default)]
struct EmbeddedFontAudit {
    fonts_checked: usize,
    resource_scopes_checked: usize,
    problems: Vec<String>,
}

fn audit_embedded_fonts(document: &PdfDocument) -> Result<EmbeddedFontAudit, ValidationError> {
    let page_count = document
        .page_count()
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let mut audit = EmbeddedFontAudit::default();

    for page_index in 0..page_count {
        let page_number = page_index + 1;
        let Ok(resources) = inherited_page_resources(document, page_index) else {
            add_font_problem(
                &mut audit,
                format!("blocking local rule: page {page_number} resources are unreadable"),
            )?;
            continue;
        };
        let mut active_resource_references = HashSet::new();
        if let Some(resources) = resources {
            audit_resource_scope(
                document,
                &resources,
                page_number,
                "page resources",
                0,
                &mut active_resource_references,
                &mut audit,
            )?;
        }
        audit_annotation_appearance_fonts(
            document,
            page_index,
            page_number,
            &mut active_resource_references,
            &mut audit,
        )?;
    }

    Ok(audit)
}

fn audit_annotation_appearance_fonts(
    document: &PdfDocument,
    page_index: usize,
    page_number: usize,
    active_resource_references: &mut HashSet<ObjectRef>,
    audit: &mut EmbeddedFontAudit,
) -> Result<(), ValidationError> {
    let page = document
        .get_page(page_index)
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let page = page.as_dict().ok_or_else(|| {
        ValidationError::MalformedReport(format!("page {page_number} is not a dictionary"))
    })?;
    let Some(annotations) = page.get("Annots") else {
        return Ok(());
    };
    let Ok(annotations) = resolve_object(document, annotations) else {
        return add_font_problem(
            audit,
            format!("blocking local rule: page {page_number} annotations are unreadable"),
        );
    };
    let Some(annotations) = annotations.as_array() else {
        return add_font_problem(
            audit,
            format!("blocking local rule: page {page_number} annotations are not an array"),
        );
    };

    let mut appearance_objects_checked = 0_usize;
    let mut active_appearance_references = HashSet::new();
    for (annotation_index, annotation) in annotations.iter().enumerate() {
        let Ok(annotation) = resolve_object(document, annotation) else {
            add_font_problem(
                audit,
                format!(
                    "blocking local rule: page {page_number} annotation {} is unreadable",
                    annotation_index + 1
                ),
            )?;
            continue;
        };
        let Some(annotation) = annotation.as_dict() else {
            continue;
        };
        let Some(appearances) = annotation.get("AP") else {
            continue;
        };
        let Ok(appearances) = resolve_object(document, appearances) else {
            add_font_problem(
                audit,
                format!(
                    "blocking local rule: page {page_number} annotation {} appearance dictionary is unreadable",
                    annotation_index + 1
                ),
            )?;
            continue;
        };
        let Some(appearances) = appearances.as_dict() else {
            add_font_problem(
                audit,
                format!(
                    "blocking local rule: page {page_number} annotation {} appearance is not a dictionary",
                    annotation_index + 1
                ),
            )?;
            continue;
        };
        let mut appearance_names: Vec<_> = appearances.keys().collect();
        appearance_names.sort_unstable();
        for appearance_name in appearance_names {
            audit_appearance_value(
                document,
                &appearances[appearance_name],
                page_number,
                annotation_index + 1,
                appearance_name,
                0,
                &mut appearance_objects_checked,
                &mut active_appearance_references,
                active_resource_references,
                audit,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn audit_appearance_value(
    document: &PdfDocument,
    value: &Object,
    page_number: usize,
    annotation_number: usize,
    appearance_name: &str,
    depth: usize,
    appearance_objects_checked: &mut usize,
    active_appearance_references: &mut HashSet<ObjectRef>,
    active_resource_references: &mut HashSet<ObjectRef>,
    audit: &mut EmbeddedFontAudit,
) -> Result<(), ValidationError> {
    if depth >= MAX_REFERENCE_DEPTH {
        return Err(ValidationError::OutputLimitExceeded);
    }
    *appearance_objects_checked = appearance_objects_checked
        .checked_add(1)
        .filter(|count| *count <= MAX_APPEARANCE_OBJECTS)
        .ok_or(ValidationError::OutputLimitExceeded)?;
    let reference = match value {
        Object::Reference(reference) => Some(*reference),
        _ => None,
    };
    if reference.is_some_and(|reference| !active_appearance_references.insert(reference)) {
        return Ok(());
    }

    let result = (|| {
        let Ok(value) = resolve_object(document, value) else {
            return add_font_problem(
                audit,
                format!(
                    "blocking local rule: page {page_number} annotation {annotation_number} appearance /{} is unreadable",
                    bounded_font_name(appearance_name)
                ),
            );
        };
        match &value {
            Object::Stream { dict, .. } => {
                if let Some(resources) = dict.get("Resources") {
                    audit_resource_scope(
                        document,
                        resources,
                        page_number,
                        &format!(
                            "annotation {annotation_number} appearance /{} resources",
                            bounded_font_name(appearance_name)
                        ),
                        depth + 1,
                        active_resource_references,
                        audit,
                    )?;
                }
            }
            Object::Dictionary(states) => {
                let mut state_names: Vec<_> = states.keys().collect();
                state_names.sort_unstable();
                for state_name in state_names {
                    audit_appearance_value(
                        document,
                        &states[state_name],
                        page_number,
                        annotation_number,
                        state_name,
                        depth + 1,
                        appearance_objects_checked,
                        active_appearance_references,
                        active_resource_references,
                        audit,
                    )?;
                }
            }
            _ => {
                add_font_problem(
                    audit,
                    format!(
                        "blocking local rule: page {page_number} annotation {annotation_number} appearance /{} is not a stream or state dictionary",
                        bounded_font_name(appearance_name)
                    ),
                )?;
            }
        }
        Ok(())
    })();

    if let Some(reference) = reference {
        active_appearance_references.remove(&reference);
    }
    result
}

fn inherited_page_resources(
    document: &PdfDocument,
    page_index: usize,
) -> Result<Option<Object>, ()> {
    let mut current = document.get_page(page_index).map_err(|_| ())?;
    let mut seen = HashSet::new();
    for _ in 0..MAX_REFERENCE_DEPTH {
        let dictionary = current.as_dict().ok_or(())?;
        if let Some(resources) = dictionary.get("Resources") {
            return Ok(Some(resources.clone()));
        }
        let Some(parent) = dictionary.get("Parent") else {
            return Ok(None);
        };
        if let Object::Reference(reference) = parent
            && !seen.insert(*reference)
        {
            return Err(());
        }
        current = resolve_object(document, parent)?;
    }
    Err(())
}

#[allow(clippy::too_many_arguments)]
fn audit_resource_scope(
    document: &PdfDocument,
    resources: &Object,
    page_number: usize,
    owner: &str,
    depth: usize,
    active_resource_references: &mut HashSet<ObjectRef>,
    audit: &mut EmbeddedFontAudit,
) -> Result<(), ValidationError> {
    if depth >= MAX_REFERENCE_DEPTH {
        return Err(ValidationError::OutputLimitExceeded);
    }
    audit.resource_scopes_checked = audit.resource_scopes_checked.saturating_add(1);
    if audit.resource_scopes_checked > MAX_RESOURCE_SCOPES {
        return Err(ValidationError::OutputLimitExceeded);
    }

    let reference = match resources {
        Object::Reference(reference) => Some(*reference),
        _ => None,
    };
    if reference.is_some_and(|reference| !active_resource_references.insert(reference)) {
        return Ok(());
    }

    let result = (|| {
        let Ok(resources) = resolve_object(document, resources) else {
            return add_font_problem(
                audit,
                format!("blocking local rule: page {page_number} {owner} are unreadable"),
            );
        };
        let Some(resources) = resources.as_dict() else {
            return add_font_problem(
                audit,
                format!("blocking local rule: page {page_number} {owner} are not a dictionary"),
            );
        };

        audit_fonts_in_scope(
            document,
            resources.get("Font"),
            page_number,
            owner,
            depth,
            active_resource_references,
            audit,
        )?;
        audit_nested_resource_category(
            document,
            resources.get("XObject"),
            "XObject",
            page_number,
            owner,
            depth,
            active_resource_references,
            audit,
        )?;
        audit_nested_resource_category(
            document,
            resources.get("Pattern"),
            "Pattern",
            page_number,
            owner,
            depth,
            active_resource_references,
            audit,
        )
    })();

    if let Some(reference) = reference {
        active_resource_references.remove(&reference);
    }
    result
}

fn audit_fonts_in_scope(
    document: &PdfDocument,
    fonts: Option<&Object>,
    page_number: usize,
    owner: &str,
    depth: usize,
    active_resource_references: &mut HashSet<ObjectRef>,
    audit: &mut EmbeddedFontAudit,
) -> Result<(), ValidationError> {
    let Some(fonts) = fonts else {
        return Ok(());
    };
    let Ok(fonts) = resolve_object(document, fonts) else {
        return add_font_problem(
            audit,
            format!(
                "blocking local rule: page {page_number} {owner} font resources are unreadable"
            ),
        );
    };
    let Some(fonts) = fonts.as_dict() else {
        return add_font_problem(
            audit,
            format!(
                "blocking local rule: page {page_number} {owner} font resources are not a dictionary"
            ),
        );
    };

    let mut font_names: Vec<_> = fonts.keys().collect();
    font_names.sort_unstable();
    for name in font_names {
        audit.fonts_checked = audit.fonts_checked.saturating_add(1);
        if audit.fonts_checked > MAX_AUDITED_FONTS {
            return Err(ValidationError::OutputLimitExceeded);
        }
        if !matches!(font_has_embedded_program(document, &fonts[name]), Ok(true)) {
            let bounded_name = bounded_font_name(name);
            add_font_problem(
                audit,
                format!(
                    "blocking local rule: page {page_number} {owner} font /{bounded_name} has no readable embedded font program"
                ),
            )?;
        }
        let Ok(font) = resolve_object(document, &fonts[name]) else {
            continue;
        };
        let Some(font) = font.as_dict() else {
            continue;
        };
        if font.get("Subtype").and_then(Object::as_name) != Some("Type3") {
            continue;
        }
        let Some(resources) = font.get("Resources") else {
            continue;
        };
        let nested_owner = format!("{owner} Type 3 font /{} resources", bounded_font_name(name));
        audit_resource_scope(
            document,
            resources,
            page_number,
            &nested_owner,
            depth + 1,
            active_resource_references,
            audit,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn audit_nested_resource_category(
    document: &PdfDocument,
    category: Option<&Object>,
    category_name: &str,
    page_number: usize,
    owner: &str,
    depth: usize,
    active_resource_references: &mut HashSet<ObjectRef>,
    audit: &mut EmbeddedFontAudit,
) -> Result<(), ValidationError> {
    let Some(category) = category else {
        return Ok(());
    };
    let Ok(category) = resolve_object(document, category) else {
        return add_font_problem(
            audit,
            format!(
                "blocking local rule: page {page_number} {owner} {category_name} resources are unreadable"
            ),
        );
    };
    let Some(category) = category.as_dict() else {
        return add_font_problem(
            audit,
            format!(
                "blocking local rule: page {page_number} {owner} {category_name} resources are not a dictionary"
            ),
        );
    };

    let mut names: Vec<_> = category.keys().collect();
    names.sort_unstable();
    for name in names {
        let nested_owner = format!("{category_name} /{} resources", bounded_font_name(name));
        let Ok(object) = resolve_object(document, &category[name]) else {
            add_font_problem(
                audit,
                format!(
                    "blocking local rule: page {page_number} {owner} {nested_owner} are unreadable"
                ),
            )?;
            continue;
        };
        let Some(dictionary) = object_dictionary(&object) else {
            continue;
        };
        if category_name == "XObject"
            && dictionary.get("Subtype").and_then(Object::as_name) != Some("Form")
        {
            continue;
        }
        let Some(resources) = dictionary.get("Resources") else {
            continue;
        };
        audit_resource_scope(
            document,
            resources,
            page_number,
            &nested_owner,
            depth + 1,
            active_resource_references,
            audit,
        )?;
    }
    Ok(())
}

fn object_dictionary(object: &Object) -> Option<&std::collections::HashMap<String, Object>> {
    match object {
        Object::Dictionary(dictionary) => Some(dictionary),
        Object::Stream { dict, .. } => Some(dict),
        _ => None,
    }
}

fn add_font_problem(audit: &mut EmbeddedFontAudit, problem: String) -> Result<(), ValidationError> {
    if audit.problems.len() >= MAX_FINDINGS {
        return Err(ValidationError::OutputLimitExceeded);
    }
    audit.problems.push(problem);
    Ok(())
}

fn bounded_font_name(name: &str) -> String {
    let mut characters = name.chars();
    let mut bounded = String::with_capacity(MAX_REPORTED_FONT_NAME_CHARS + 3);
    for character in characters.by_ref().take(MAX_REPORTED_FONT_NAME_CHARS) {
        bounded.push(if character.is_ascii_graphic() {
            character
        } else {
            '?'
        });
    }
    if characters.next().is_some() {
        bounded.push_str("...");
    }
    bounded
}

fn font_has_embedded_program(document: &PdfDocument, font: &Object) -> Result<bool, ()> {
    let font = resolve_object(document, font)?;
    let font = font.as_dict().ok_or(())?;

    if font.get("Subtype").and_then(Object::as_name) == Some("Type3") {
        return type3_font_is_structurally_valid(document, font);
    }

    if font.get("Subtype").and_then(Object::as_name) == Some("Type0") {
        let descendants = font.get("DescendantFonts").ok_or(())?;
        let descendants = resolve_object(document, descendants)?;
        let descendants = descendants.as_array().ok_or(())?;
        if descendants.is_empty() {
            return Ok(false);
        }
        for descendant in descendants {
            let descendant = resolve_object(document, descendant)?;
            let descendant = descendant.as_dict().ok_or(())?;
            if !dictionary_has_embedded_program(document, descendant)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    dictionary_has_embedded_program(document, font)
}

fn type3_font_is_structurally_valid(
    document: &PdfDocument,
    font: &std::collections::HashMap<String, Object>,
) -> Result<bool, ()> {
    if !resolved_numeric_array_has_len(document, font.get("FontBBox").ok_or(())?, 4)?
        || !resolved_numeric_array_has_len(document, font.get("FontMatrix").ok_or(())?, 6)?
    {
        return Ok(false);
    }

    let first_char = resolve_object(document, font.get("FirstChar").ok_or(())?)?
        .as_integer()
        .ok_or(())?;
    let last_char = resolve_object(document, font.get("LastChar").ok_or(())?)?
        .as_integer()
        .ok_or(())?;
    if !(0..=255).contains(&first_char) || !(first_char..=255).contains(&last_char) {
        return Ok(false);
    }
    let expected_widths = usize::try_from(last_char - first_char + 1).map_err(|_| ())?;
    if !resolved_numeric_array_has_len(document, font.get("Widths").ok_or(())?, expected_widths)? {
        return Ok(false);
    }

    let char_procs = font.get("CharProcs").ok_or(())?;
    let char_procs = resolve_object(document, char_procs)?;
    let char_procs = char_procs.as_dict().ok_or(())?;
    if char_procs.is_empty() || char_procs.len() > MAX_TYPE3_CHAR_PROCS {
        return Ok(false);
    }
    for char_proc in char_procs.values() {
        if !type3_charproc_is_structurally_valid(document, char_proc)? {
            return Ok(false);
        }
    }
    type3_encoding_maps_charprocs(
        document,
        font.get("Encoding").ok_or(())?,
        char_procs,
        first_char,
        last_char,
    )
}

fn type3_encoding_maps_charprocs(
    document: &PdfDocument,
    encoding: &Object,
    char_procs: &std::collections::HashMap<String, Object>,
    first_char: i64,
    last_char: i64,
) -> Result<bool, ()> {
    let encoding = resolve_object(document, encoding)?;
    let encoding = encoding.as_dict().ok_or(())?;
    let differences = resolve_object(document, encoding.get("Differences").ok_or(())?)?;
    let differences = differences.as_array().ok_or(())?;
    if differences.is_empty() || differences.len() > MAX_TYPE3_DIFFERENCES {
        return Ok(false);
    }

    let mut next_code = None;
    let mut mapped_glyph = false;
    for entry in differences {
        let entry = resolve_object(document, entry)?;
        match entry {
            Object::Integer(code) if (0..=255).contains(&code) => next_code = Some(code),
            Object::Name(name) => {
                let Some(code) = next_code else {
                    return Ok(false);
                };
                if code < first_char || code > last_char || !char_procs.contains_key(&name) {
                    return Ok(false);
                }
                mapped_glyph = true;
                next_code = code.checked_add(1).filter(|next| *next <= 255);
            }
            _ => return Ok(false),
        }
    }
    Ok(mapped_glyph)
}

fn resolved_numeric_array_has_len(
    document: &PdfDocument,
    object: &Object,
    expected_len: usize,
) -> Result<bool, ()> {
    let object = resolve_object(document, object)?;
    let values = object.as_array().ok_or(())?;
    if values.len() != expected_len {
        return Ok(false);
    }
    for value in values {
        let value = resolve_object(document, value)?;
        if !matches!(value, Object::Integer(_) | Object::Real(_)) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn type3_charproc_is_structurally_valid(
    document: &PdfDocument,
    char_proc: &Object,
) -> Result<bool, ()> {
    let char_proc = resolve_object(document, char_proc)?;
    if !matches!(char_proc, Object::Stream { .. }) {
        return Ok(false);
    }
    let decoded = char_proc.decode_stream_data().map_err(|_| ())?;
    let operators = parse_content_stream(&decoded).map_err(|_| ())?;
    let Some(Operator::Other { name, operands }) = operators.first() else {
        return Ok(false);
    };
    let expected_operands = match name.as_str() {
        "d0" => 2,
        "d1" => 6,
        _ => return Ok(false),
    };
    Ok(operands.len() == expected_operands
        && operands
            .iter()
            .all(|operand| matches!(operand, Object::Integer(_) | Object::Real(_))))
}

fn dictionary_has_embedded_program(
    document: &PdfDocument,
    dictionary: &std::collections::HashMap<String, Object>,
) -> Result<bool, ()> {
    if font_program_entries_are_readable(document, dictionary) {
        return Ok(true);
    }
    let Some(descriptor) = dictionary.get("FontDescriptor") else {
        return Ok(false);
    };
    let descriptor = resolve_object(document, descriptor)?;
    let descriptor = descriptor.as_dict().ok_or(())?;
    Ok(font_program_entries_are_readable(document, descriptor))
}

fn font_program_entries_are_readable(
    document: &PdfDocument,
    dictionary: &std::collections::HashMap<String, Object>,
) -> bool {
    ["FontFile", "FontFile2", "FontFile3"]
        .iter()
        .filter_map(|key| dictionary.get(*key).map(|program| (*key, program)))
        .any(|(key, program)| embedded_font_program_is_valid(document, key, program))
}

fn embedded_font_program_is_valid(document: &PdfDocument, key: &str, program: &Object) -> bool {
    let Ok(program) = resolve_object(document, program) else {
        return false;
    };
    let Object::Stream { dict, data } = &program else {
        return false;
    };
    if data.is_empty() {
        return false;
    }
    let Ok(decoded) = program.decode_stream_data() else {
        return false;
    };
    if decoded.is_empty() {
        return false;
    }

    let subtype = dict.get("Subtype").and_then(|subtype| {
        resolve_object(document, subtype)
            .ok()
            .and_then(|subtype| subtype.as_name().map(str::to_owned))
    });
    if key == "FontFile3" {
        return match subtype.as_deref() {
            Some("OpenType") => ttf_parser::Face::parse(&decoded, 0).is_ok(),
            Some("Type1C") => inspect_cff_program(&decoded, usize::MAX)
                .is_ok_and(|proof| matches!(proof.charset, CffCharsetProof::Named(_))),
            Some("CIDFontType0C") => inspect_cff_program(&decoded, usize::MAX)
                .is_ok_and(|proof| matches!(proof.charset, CffCharsetProof::Cid(_))),
            _ => false,
        };
    }
    if key == "FontFile2" {
        return ttf_parser::Face::parse(&decoded, 0).is_ok();
    }
    key == "FontFile"
}

fn resolve_object(document: &PdfDocument, object: &Object) -> Result<Object, ()> {
    let mut current = object.clone();
    let mut seen = HashSet::new();
    for _ in 0..MAX_REFERENCE_DEPTH {
        let Object::Reference(reference) = current else {
            return Ok(current);
        };
        if !seen.insert(reference) {
            return Err(());
        }
        current = document.load_object(reference).map_err(|_| ())?;
    }
    if matches!(current, Object::Reference(_)) {
        Err(())
    } else {
        Ok(current)
    }
}

/// `PDFOxide`'s read-only PDF/X-4 adapter for the feasibility and differential
/// fixture gate.
///
/// The exact output-intent profile hash is supplied by the caller so validator
/// evidence cannot be reused after a profile change.
#[derive(Clone, Copy, Debug)]
pub struct PdfOxideValidator {
    profile_hash: ArtifactHash,
}

impl PdfOxideValidator {
    #[must_use]
    pub const fn new(profile_hash: ArtifactHash) -> Self {
        Self { profile_hash }
    }
}

impl IndependentValidator for PdfOxideValidator {
    fn identity(&self) -> ValidatorIdentity {
        ValidatorIdentity {
            name: DIAGNOSTIC_NAME.into(),
            version: DIAGNOSTIC_VERSION.into(),
            executable_or_source_hash: ArtifactHash::of(DIAGNOSTIC_SOURCE_IDENTITY.as_bytes()),
            rules_hash: ArtifactHash::of(DIAGNOSTIC_RULES_IDENTITY.as_bytes()),
            profile_hash: self.profile_hash,
        }
    }

    fn validate(
        &self,
        request: ValidationRequest<'_>,
    ) -> Result<ValidatorResponse, ValidationError> {
        let level = match request.standard() {
            PrintStandard::PdfX4 => PdfXLevel::X4,
        };
        let mut document = PdfDocument::from_bytes(request.candidate_bytes().to_vec())
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        let result = validate_pdf_x(&mut document, level)
            .map_err(|error| ValidationError::Unavailable(error.to_string()))?;
        let font_audit = audit_embedded_fonts(&document)?;
        let glyph_problems = match audit_used_glyphs(&document) {
            Ok(audit) => audit.into_problems(),
            Err(ValidationError::OutputLimitExceeded) => {
                return Err(ValidationError::OutputLimitExceeded);
            }
            Err(_) => vec![
                "blocking local rule: used-glyph coverage cannot be read from this candidate"
                    .to_owned(),
            ],
        };

        // PDFOxide 0.3.73 reports an absent, mismatched, or unreadable
        // pdfxid:GTS_PDFXVersion value as XMETA-006 with warning severity.
        // Publisher cannot treat those incomplete identity facts as proof of
        // PDF/X-4, so this narrow, source-audited rule is promoted locally.
        let has_blocking_xmp_identity_warning = result
            .warnings
            .iter()
            .any(|finding| matches!(finding.code, XErrorCode::XmpMetadataInvalid));

        // PDFOxide 0.3.73's PDF/X path checks only the top-level Type 0
        // dictionary and rejects Type 3 fonts categorically. The former misses
        // descendant programs; the latter contradicts the official hash-pinned
        // GOS5 PDF/X-4 known-good corpus. Replace only those two upstream font
        // findings, and only after the separate traversal proves every declared
        // page font has a readable program and every actively shown supported
        // character code resolves to a glyph program.
        let can_replace_font_embedding_findings = font_audit.fonts_checked > 0
            && font_audit.problems.is_empty()
            && glyph_problems.is_empty();
        let has_font_audit_problems = !font_audit.problems.is_empty() || !glyph_problems.is_empty();
        let mut errors = result.errors;
        if can_replace_font_embedding_findings {
            errors.retain(|finding| {
                !matches!(
                    finding.code,
                    XErrorCode::FontNotEmbedded | XErrorCode::Type3FontNotAllowed
                )
            });
        }
        let has_remaining_errors = !errors.is_empty();

        // PDFOxide's general PDF/X routine checks the OutputIntent declaration
        // but does not require a usable DestOutputProfile. Its independent
        // parser can decode that profile, so require its exact bytes to match
        // the profile identity selected by the host.
        let output_profile_problem = match document.output_intent_cmyk_profile() {
            None => Some(
                "blocking local rule: OutputIntent must contain a readable N=4 CMYK DestOutputProfile"
                    .to_owned(),
            ),
            Some(profile) if ArtifactHash::of(profile.bytes()) != self.profile_hash => Some(
                "blocking local rule: embedded DestOutputProfile bytes do not match the selected printer profile"
                    .to_owned(),
            ),
            Some(_) => None,
        };

        let total_findings = errors
            .len()
            .saturating_add(result.warnings.len())
            .saturating_add(font_audit.problems.len())
            .saturating_add(glyph_problems.len())
            .saturating_add(usize::from(output_profile_problem.is_some()));
        if total_findings > MAX_FINDINGS {
            return Err(ValidationError::OutputLimitExceeded);
        }
        let mut findings = Vec::with_capacity(total_findings);
        findings.extend(errors.into_iter().map(|finding| finding.to_string()));
        findings.extend(result.warnings.into_iter().map(|finding| {
            if matches!(finding.code, XErrorCode::XmpMetadataInvalid) {
                format!("blocking warning: {finding}")
            } else {
                format!("warning: {finding}")
            }
        }));
        findings.extend(output_profile_problem.iter().cloned());
        findings.extend(font_audit.problems);
        findings.extend(glyph_problems);

        Ok(ValidatorResponse {
            verdict: if !has_remaining_errors
                && !has_blocking_xmp_identity_warning
                && output_profile_problem.is_none()
                && !has_font_audit_problems
            {
                ValidationVerdict::Passed
            } else {
                ValidationVerdict::Failed
            },
            findings,
        })
    }
}

#[cfg(test)]
mod tests {
    use document_core::Revision;
    use layout_core::ResolvedPublication;

    use super::PdfOxideValidator;
    use crate::{
        ArtifactHash, CandidatePdfWriter, EvaluationKey, FixtureContract, FontBoundary,
        IccColorSpace, IccDeviceClass, IndependentValidator, OutputIntentFacts, PreflightBasis,
        PrintStandard, ProfileHash, ValidationError, ValidationRequest, ValidationVerdict,
        run_independent_validation,
    };

    fn structural_probe() -> (EvaluationKey, Vec<u8>) {
        let fixture = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            OutputIntentFacts {
                sha256: ProfileHash::from_bytes([5; 32]),
                byte_len: 132,
                declared_size: 132,
                device_class: IccDeviceClass::Printer,
                color_space: IccColorSpace::Cmyk,
                profile_connection_space: *b"Lab ",
            },
            FontBoundary::NotExercised,
        );
        let candidate = CandidatePdfWriter::write_structural_probe(&fixture).expect("valid probe");
        (fixture.evaluation_key(), candidate.bytes().to_vec())
    }

    #[test]
    fn malformed_pdf_is_an_adapter_error_not_a_failed_conformance_verdict() {
        let validator = PdfOxideValidator::new(ArtifactHash::of(b"profile"));
        let result = validator.validate(ValidationRequest::new(
            EvaluationKey::from_bytes([8; 32]),
            b"not a pdf",
            PrintStandard::PdfX4,
        ));

        assert!(matches!(result, Err(ValidationError::MalformedReport(_))));
    }

    #[test]
    fn parseable_structural_probe_is_rejected_as_nonconformant() {
        let (key, bytes) = structural_probe();
        let validator = PdfOxideValidator::new(ArtifactHash::of(b"profile"));
        let result = run_independent_validation(
            &validator,
            ValidationRequest::new(key, &bytes, PrintStandard::PdfX4),
        )
        .expect("parseable PDF receives a conformance result");

        assert_eq!(result.verdict(), ValidationVerdict::Failed);
        assert!(!result.findings().is_empty());
        assert_eq!(result.artifact_hash(), ArtifactHash::of(&bytes));
    }

    #[test]
    fn identity_is_pinned_to_dependency_rules_and_profile() {
        let first_profile = ArtifactHash::of(b"first-profile");
        let second_profile = ArtifactHash::of(b"second-profile");
        let first = PdfOxideValidator::new(first_profile).identity();
        let second = PdfOxideValidator::new(second_profile).identity();

        assert_eq!(first.version, "0.3.73");
        assert_eq!(
            first.executable_or_source_hash,
            second.executable_or_source_hash
        );
        assert_eq!(first.rules_hash, second.rules_hash);
        assert_eq!(first.profile_hash, first_profile);
        assert_eq!(second.profile_hash, second_profile);
    }
}
