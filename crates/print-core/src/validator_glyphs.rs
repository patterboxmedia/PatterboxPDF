//! Bounded used-glyph coverage for Publisher's independently parsed PDF/X candidate.

use pdf_oxide::{
    content::{Operator, TextElement, parse_content_stream},
    document::PdfDocument,
    fonts::{Encoding as PdfFontEncoding, FontInfo},
    object::{Object, ObjectRef},
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{
    ValidationError,
    validator_cff::{CffCharsetProof, inspect_cff_program},
};

const MAX_REFERENCE_DEPTH: usize = 32;
const MAX_CONTENT_DEPTH: usize = 32;
const MAX_CONTENT_STREAMS: usize = 4_096;
const MAX_CONTENT_OPERATORS: usize = 1_000_000;
const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_FONT_PROOFS: usize = 4_096;
const MAX_TOTAL_CFF_GLYPHS: usize = 1_000_000;
const MAX_CID_TO_GID_MAP_BYTES: usize = 2 * (u16::MAX as usize + 1);
const MAX_CID_TO_GID_MAP_ENCODED_BYTES: usize = 256 * 1024;
const MAX_TOTAL_CID_TO_GID_MAP_BYTES: usize = 64 * 1024 * 1024;
const MAX_FONT_NAME_BYTES: usize = 256;
const MAX_FINDINGS: usize = 512;
const MAX_REPORTED_NAME_CHARS: usize = 64;

#[derive(Debug, Default)]
pub struct UsedGlyphAudit {
    problems: Vec<String>,
}

impl UsedGlyphAudit {
    #[must_use]
    pub fn into_problems(self) -> Vec<String> {
        self.problems
    }
}

pub fn audit_used_glyphs(document: &PdfDocument) -> Result<UsedGlyphAudit, ValidationError> {
    let page_count = document
        .page_count()
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let mut context = GlyphAuditContext::default();
    for page_index in 0..page_count {
        let page_number = page_index + 1;
        let resources = page_resources(document, page_index)?;
        let page_content = document
            .get_page_content_data(page_index)
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        context.walk_content(document, &page_content, &resources, page_number, "page", 0)?;
        context.walk_annotation_appearances(document, page_index, &resources, page_number)?;
    }
    Ok(UsedGlyphAudit {
        problems: context.problems,
    })
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum FontProofKey {
    Reference(ObjectRef),
    Direct { stream: usize, name: String },
}

#[derive(Clone, Debug)]
enum FontProof {
    IdentityType2 {
        glyph_count: u16,
        cid_to_gid: Option<Arc<[u8]>>,
    },
    IdentityType0Cff {
        cids: Arc<[u16]>,
    },
    Simple {
        program: SimpleFontProgram,
        first_char: u8,
        last_char: u8,
        code_states: Arc<[CodeState; 256]>,
    },
    Type3 {
        first_char: u8,
        last_char: u8,
        code_states: Arc<[CodeState; 256]>,
    },
    Unproved(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodeState {
    Unmapped,
    Present,
    Missing,
    OutOfRange,
}

#[derive(Clone, Copy, Debug)]
enum SimpleFontProgram {
    Cff,
    TrueType,
}

impl SimpleFontProgram {
    const fn label(self) -> &'static str {
        match self {
            Self::Cff => "CFF",
            Self::TrueType => "TrueType",
        }
    }
}

#[derive(Default)]
struct GlyphAuditContext {
    streams_checked: usize,
    operators_checked: usize,
    text_bytes_checked: usize,
    cff_glyphs_checked: usize,
    cid_to_gid_bytes_checked: usize,
    font_proofs: HashMap<FontProofKey, FontProof>,
    reported_unproved_fonts: HashSet<FontProofKey>,
    reported_codes: HashMap<FontProofKey, [bool; 256]>,
    active_streams: HashSet<ObjectRef>,
    reported_problems: HashSet<String>,
    problems: Vec<String>,
}

impl GlyphAuditContext {
    fn walk_content(
        &mut self,
        document: &PdfDocument,
        content: &[u8],
        resources: &Object,
        page_number: usize,
        owner: &str,
        depth: usize,
    ) -> Result<(), ValidationError> {
        if depth > MAX_CONTENT_DEPTH {
            return Err(ValidationError::OutputLimitExceeded);
        }
        self.streams_checked = self
            .streams_checked
            .checked_add(1)
            .filter(|count| *count <= MAX_CONTENT_STREAMS)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        let stream_number = self.streams_checked;
        let operators = parse_content_stream(content)
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        self.operators_checked = self
            .operators_checked
            .checked_add(operators.len())
            .filter(|count| *count <= MAX_CONTENT_OPERATORS)
            .ok_or(ValidationError::OutputLimitExceeded)?;

        let mut current_font: Option<String> = None;
        let mut saved_fonts = Vec::new();
        for operator in operators {
            match operator {
                Operator::Tf { font, .. } => current_font = Some(font),
                Operator::SaveState => saved_fonts.push(current_font.clone()),
                Operator::RestoreState => current_font = saved_fonts.pop().unwrap_or(None),
                Operator::Tj { text }
                | Operator::Quote { text }
                | Operator::DoubleQuote { text, .. } => self.inspect_text(
                    document,
                    resources,
                    current_font.as_deref(),
                    &text,
                    page_number,
                    owner,
                    stream_number,
                )?,
                Operator::TJ { array } => {
                    for element in array {
                        if let TextElement::String(text) = element {
                            self.inspect_text(
                                document,
                                resources,
                                current_font.as_deref(),
                                &text,
                                page_number,
                                owner,
                                stream_number,
                            )?;
                        }
                    }
                }
                Operator::Do { name } => {
                    self.inspect_xobject(document, resources, &name, page_number, owner, depth)?;
                }
                Operator::SetFillColorN {
                    name: Some(name), ..
                }
                | Operator::SetStrokeColorN {
                    name: Some(name), ..
                } => {
                    self.inspect_pattern(document, resources, &name, page_number, owner, depth)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "text proof keeps its exact resource scope, visible owner, page, and bounded stream cache identity explicit"
    )]
    fn inspect_text(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        font_name: Option<&str>,
        text: &[u8],
        page_number: usize,
        owner: &str,
        stream_number: usize,
    ) -> Result<(), ValidationError> {
        if text.is_empty() {
            return Ok(());
        }
        self.text_bytes_checked = self
            .text_bytes_checked
            .checked_add(text.len())
            .filter(|count| *count <= MAX_TEXT_BYTES)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        let Some(font_name) = font_name else {
            return self.add_problem(format!(
                "blocking local rule: page {page_number} {owner} shows text without selecting a font"
            ));
        };
        if font_name.len() > MAX_FONT_NAME_BYTES {
            return self.unproved_font(
                page_number,
                owner,
                font_name,
                "font resource name exceeds the independent proof limit",
            );
        }
        let Some(font) = lookup_resource(document, resources, "Font", font_name)? else {
            return self.add_problem(format!(
                "blocking local rule: page {page_number} {owner} selects unavailable font /{}",
                bounded_name(font_name)
            ));
        };
        let proof_key = font.as_reference().map_or_else(
            || FontProofKey::Direct {
                stream: stream_number,
                name: font_name.to_owned(),
            },
            FontProofKey::Reference,
        );
        let proof = if let Some(proof) = self.font_proofs.get(&proof_key) {
            proof.clone()
        } else {
            if self.font_proofs.len() >= MAX_FONT_PROOFS {
                return Err(ValidationError::OutputLimitExceeded);
            }
            let remaining_cff_glyphs = MAX_TOTAL_CFF_GLYPHS
                .checked_sub(self.cff_glyphs_checked)
                .ok_or(ValidationError::OutputLimitExceeded)?;
            let remaining_cid_to_gid_bytes = MAX_TOTAL_CID_TO_GID_MAP_BYTES
                .checked_sub(self.cid_to_gid_bytes_checked)
                .ok_or(ValidationError::OutputLimitExceeded)?;
            let (proof, cff_glyphs, cid_to_gid_bytes) = font_proof(
                document,
                &font,
                remaining_cff_glyphs,
                remaining_cid_to_gid_bytes,
            )?;
            self.cff_glyphs_checked = self
                .cff_glyphs_checked
                .checked_add(cff_glyphs)
                .filter(|count| *count <= MAX_TOTAL_CFF_GLYPHS)
                .ok_or(ValidationError::OutputLimitExceeded)?;
            self.cid_to_gid_bytes_checked = self
                .cid_to_gid_bytes_checked
                .checked_add(cid_to_gid_bytes)
                .filter(|count| *count <= MAX_TOTAL_CID_TO_GID_MAP_BYTES)
                .ok_or(ValidationError::OutputLimitExceeded)?;
            self.font_proofs.insert(proof_key.clone(), proof.clone());
            proof
        };
        self.inspect_proven_text(proof_key, proof, text, page_number, owner, font_name)
    }

    fn inspect_proven_text(
        &mut self,
        proof_key: FontProofKey,
        proof: FontProof,
        text: &[u8],
        page_number: usize,
        owner: &str,
        font_name: &str,
    ) -> Result<(), ValidationError> {
        match proof {
            FontProof::IdentityType2 {
                glyph_count,
                cid_to_gid,
            } => {
                let (cid_pairs, incomplete) = text.as_chunks::<2>();
                if !incomplete.is_empty() {
                    return self.unproved_font(
                        page_number,
                        owner,
                        font_name,
                        "identity text contains an incomplete two-byte CID",
                    );
                }
                for pair in cid_pairs {
                    let cid = u16::from_be_bytes(*pair);
                    if let Some(map) = &cid_to_gid {
                        let offset = usize::from(cid) * 2;
                        let gid = map
                            .get(offset..offset + 2)
                            .map_or(0, |bytes| u16::from_be_bytes([bytes[0], bytes[1]]));
                        if gid == 0 || gid >= glyph_count {
                            self.add_problem(format!(
                                "blocking local rule: page {page_number} {owner} font /{} uses CID {cid}, which maps to unavailable GID {gid} in its embedded program",
                                bounded_name(font_name)
                            ))?;
                        }
                    } else if cid == 0 || cid >= glyph_count {
                        self.add_problem(format!(
                            "blocking local rule: page {page_number} {owner} font /{} uses CID {cid}, which is absent from its embedded program",
                            bounded_name(font_name)
                        ))?;
                    }
                }
            }
            FontProof::IdentityType0Cff { cids } => {
                let (cid_pairs, incomplete) = text.as_chunks::<2>();
                if !incomplete.is_empty() {
                    return self.unproved_font(
                        page_number,
                        owner,
                        font_name,
                        "identity text contains an incomplete two-byte CID",
                    );
                }
                for pair in cid_pairs {
                    let cid = u16::from_be_bytes(*pair);
                    if cid == 0 || cids.binary_search(&cid).is_err() {
                        self.add_problem(format!(
                            "blocking local rule: page {page_number} {owner} font /{} uses CID {cid}, which is absent from its embedded CFF program",
                            bounded_name(font_name)
                        ))?;
                    }
                }
            }
            FontProof::Simple {
                program,
                first_char,
                last_char,
                code_states,
            } => self.inspect_simple_text(
                &proof_key,
                program,
                first_char,
                last_char,
                &code_states,
                text,
                page_number,
                owner,
                font_name,
            )?,
            FontProof::Type3 {
                first_char,
                last_char,
                code_states,
            } => self.inspect_type3_text(
                &proof_key,
                first_char,
                last_char,
                &code_states,
                text,
                page_number,
                owner,
                font_name,
            )?,
            FontProof::Unproved(reason) => {
                if self.reported_unproved_fonts.insert(proof_key) {
                    return self.unproved_font(page_number, owner, font_name, reason);
                }
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Type 3 findings preserve the exact font proof, byte range, page, and visible resource owner"
    )]
    fn inspect_type3_text(
        &mut self,
        proof_key: &FontProofKey,
        first_char: u8,
        last_char: u8,
        code_states: &[CodeState; 256],
        text: &[u8],
        page_number: usize,
        owner: &str,
        font_name: &str,
    ) -> Result<(), ValidationError> {
        let new_problems = collect_unresolved_codes(
            &mut self.reported_codes,
            proof_key,
            first_char,
            last_char,
            code_states,
            text,
        );
        for (code, state) in new_problems {
            let reason = match state {
                CodeState::Missing => "is absent from its CharProcs",
                CodeState::OutOfRange => "is outside its declared FirstChar/LastChar range",
                CodeState::Unmapped => "has no explicit Type 3 Differences mapping",
                CodeState::Present => unreachable!("present codes are filtered"),
            };
            self.add_problem(format!(
                "blocking local rule: page {page_number} {owner} font /{} uses character code {code}, which {reason}",
                bounded_name(font_name)
            ))?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "simple-font findings preserve the exact font proof, byte range, page, and visible resource owner"
    )]
    fn inspect_simple_text(
        &mut self,
        proof_key: &FontProofKey,
        program: SimpleFontProgram,
        first_char: u8,
        last_char: u8,
        code_states: &[CodeState; 256],
        text: &[u8],
        page_number: usize,
        owner: &str,
        font_name: &str,
    ) -> Result<(), ValidationError> {
        let new_problems = collect_unresolved_codes(
            &mut self.reported_codes,
            proof_key,
            first_char,
            last_char,
            code_states,
            text,
        );
        for (code, state) in new_problems {
            let reason = if state == CodeState::OutOfRange {
                "outside its declared FirstChar/LastChar range".to_owned()
            } else {
                format!("absent from its embedded {} program", program.label())
            };
            self.add_problem(format!(
                "blocking local rule: page {page_number} {owner} font /{} uses character code {code}, which is {reason}",
                bounded_name(font_name)
            ))?;
        }
        Ok(())
    }

    fn unproved_font(
        &mut self,
        page_number: usize,
        owner: &str,
        font_name: &str,
        reason: &str,
    ) -> Result<(), ValidationError> {
        self.add_problem(format!(
            "blocking local rule: page {page_number} {owner} font /{} used-glyph coverage is unproved: {reason}",
            bounded_name(font_name)
        ))
    }

    fn inspect_xobject(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        name: &str,
        page_number: usize,
        owner: &str,
        depth: usize,
    ) -> Result<(), ValidationError> {
        let Some(xobject) = lookup_resource(document, resources, "XObject", name)? else {
            return Ok(());
        };
        self.walk_nested_stream(
            document,
            &xobject,
            resources,
            page_number,
            &format!("{owner} form /{}", bounded_name(name)),
            depth,
            Some("Form"),
        )
    }

    fn inspect_pattern(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        name: &str,
        page_number: usize,
        owner: &str,
        depth: usize,
    ) -> Result<(), ValidationError> {
        let Some(pattern) = lookup_resource(document, resources, "Pattern", name)? else {
            return Ok(());
        };
        self.walk_nested_stream(
            document,
            &pattern,
            resources,
            page_number,
            &format!("{owner} pattern /{}", bounded_name(name)),
            depth,
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "nested stream traversal keeps the exact resource scope, page, owner, depth, and subtype proof explicit"
    )]
    fn walk_nested_stream(
        &mut self,
        document: &PdfDocument,
        stream: &Object,
        inherited_resources: &Object,
        page_number: usize,
        owner: &str,
        depth: usize,
        required_subtype: Option<&str>,
    ) -> Result<(), ValidationError> {
        let reference = stream.as_reference();
        if reference.is_some_and(|reference| !self.active_streams.insert(reference)) {
            return Ok(());
        }
        let result = (|| {
            let stream = resolve_object(document, stream)?;
            let Object::Stream { dict, .. } = &stream else {
                return Ok(());
            };
            if required_subtype.is_some_and(|subtype| {
                dict.get("Subtype").and_then(Object::as_name) != Some(subtype)
            }) {
                return Ok(());
            }
            let resources = dict
                .get("Resources")
                .cloned()
                .unwrap_or_else(|| inherited_resources.clone());
            let content = stream.decode_stream_data().map_err(|error| {
                ValidationError::MalformedReport(format!("{owner} cannot be decoded: {error}"))
            })?;
            self.walk_content(
                document,
                &content,
                &resources,
                page_number,
                owner,
                depth + 1,
            )
        })();
        if let Some(reference) = reference {
            self.active_streams.remove(&reference);
        }
        result
    }

    fn walk_annotation_appearances(
        &mut self,
        document: &PdfDocument,
        page_index: usize,
        page_resources: &Object,
        page_number: usize,
    ) -> Result<(), ValidationError> {
        let page = document
            .get_page(page_index)
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        let Some(annotations) = page.as_dict().and_then(|page| page.get("Annots")) else {
            return Ok(());
        };
        let annotations = resolve_object(document, annotations)?;
        let Some(annotations) = annotations.as_array() else {
            return Ok(());
        };
        for (annotation_index, annotation) in annotations.iter().enumerate() {
            let annotation = resolve_object(document, annotation)?;
            let Some(appearances) = annotation.as_dict().and_then(|value| value.get("AP")) else {
                continue;
            };
            let appearances = resolve_object(document, appearances)?;
            let Some(appearances) = appearances.as_dict() else {
                continue;
            };
            let mut names: Vec<_> = appearances.keys().collect();
            names.sort_unstable();
            for name in names {
                self.walk_appearance_value(
                    document,
                    &appearances[name],
                    page_resources,
                    page_number,
                    &format!(
                        "annotation {} appearance /{}",
                        annotation_index + 1,
                        bounded_name(name)
                    ),
                    0,
                )?;
            }
        }
        Ok(())
    }

    fn walk_appearance_value(
        &mut self,
        document: &PdfDocument,
        appearance: &Object,
        inherited_resources: &Object,
        page_number: usize,
        owner: &str,
        depth: usize,
    ) -> Result<(), ValidationError> {
        if depth > MAX_CONTENT_DEPTH {
            return Err(ValidationError::OutputLimitExceeded);
        }
        let resolved = resolve_object(document, appearance)?;
        match &resolved {
            Object::Stream { .. } => self.walk_nested_stream(
                document,
                appearance,
                inherited_resources,
                page_number,
                owner,
                depth,
                None,
            ),
            Object::Dictionary(states) => {
                let mut names: Vec<_> = states.keys().collect();
                names.sort_unstable();
                for state in names {
                    self.walk_appearance_value(
                        document,
                        &states[state],
                        inherited_resources,
                        page_number,
                        &format!("{owner} state /{}", bounded_name(state)),
                        depth + 1,
                    )?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn add_problem(&mut self, problem: String) -> Result<(), ValidationError> {
        if !self.reported_problems.insert(problem.clone()) {
            return Ok(());
        }
        if self.problems.len() >= MAX_FINDINGS {
            return Err(ValidationError::OutputLimitExceeded);
        }
        self.problems.push(problem);
        Ok(())
    }
}

fn collect_unresolved_codes(
    reported_by_font: &mut HashMap<FontProofKey, [bool; 256]>,
    proof_key: &FontProofKey,
    first_char: u8,
    last_char: u8,
    code_states: &[CodeState; 256],
    text: &[u8],
) -> Vec<(u8, CodeState)> {
    let reported_codes = reported_by_font
        .entry(proof_key.clone())
        .or_insert([false; 256]);
    let mut unresolved = Vec::new();
    for code in text {
        let index = usize::from(*code);
        let state = if *code < first_char || *code > last_char {
            CodeState::OutOfRange
        } else {
            code_states[index]
        };
        if state == CodeState::Present || reported_codes[index] {
            continue;
        }
        reported_codes[index] = true;
        unresolved.push((*code, state));
    }
    unresolved
}

fn font_proof(
    document: &PdfDocument,
    font: &Object,
    cff_glyph_budget: usize,
    cid_to_gid_byte_budget: usize,
) -> Result<(FontProof, usize, usize), ValidationError> {
    let font = resolve_object(document, font)?;
    let Some(font) = font.as_dict() else {
        return Ok((FontProof::Unproved("font is not a dictionary"), 0, 0));
    };
    let proof = match font.get("Subtype").and_then(Object::as_name) {
        Some("Type0") => {
            return identity_type0_proof(document, font, cff_glyph_budget, cid_to_gid_byte_budget);
        }
        Some("Type3") => type3_proof(document, font)?,
        Some("Type1" | "MMType1") => {
            return simple_cff_proof(document, font, cff_glyph_budget)
                .map(|(proof, glyphs)| (proof, glyphs, 0));
        }
        Some("TrueType") => {
            return simple_truetype_proof(document, font).map(|(proof, glyphs)| (proof, glyphs, 0));
        }
        _ => FontProof::Unproved(
            "only Type 0 identity and explicitly encoded simple, Type 1C, and Type 3 fonts have independent used-glyph proof",
        ),
    };
    Ok((proof, 0, 0))
}

fn simple_font_code_range(
    document: &PdfDocument,
    font: &HashMap<String, Object>,
    program: SimpleFontProgram,
) -> Result<Result<(u8, u8), &'static str>, ValidationError> {
    let (missing_first, invalid_first, missing_last, invalid_last, reversed) = match program {
        SimpleFontProgram::Cff => (
            "simple CFF font has no FirstChar",
            "simple CFF FirstChar is not a one-byte character code",
            "simple CFF font has no LastChar",
            "simple CFF LastChar is not a one-byte character code",
            "simple CFF FirstChar is greater than LastChar",
        ),
        SimpleFontProgram::TrueType => (
            "simple TrueType font has no FirstChar",
            "simple TrueType FirstChar is not a one-byte character code",
            "simple TrueType font has no LastChar",
            "simple TrueType LastChar is not a one-byte character code",
            "simple TrueType FirstChar is greater than LastChar",
        ),
    };
    let Some(first_char) = font.get("FirstChar") else {
        return Ok(Err(missing_first));
    };
    let first_char = resolve_object(document, first_char)?;
    let Some(first_char) = first_char
        .as_integer()
        .and_then(|value| u8::try_from(value).ok())
    else {
        return Ok(Err(invalid_first));
    };
    let Some(last_char) = font.get("LastChar") else {
        return Ok(Err(missing_last));
    };
    let last_char = resolve_object(document, last_char)?;
    let Some(last_char) = last_char
        .as_integer()
        .and_then(|value| u8::try_from(value).ok())
    else {
        return Ok(Err(invalid_last));
    };
    if first_char > last_char {
        return Ok(Err(reversed));
    }
    Ok(Ok((first_char, last_char)))
}

fn simple_cff_proof(
    document: &PdfDocument,
    font: &HashMap<String, Object>,
    cff_glyph_budget: usize,
) -> Result<(FontProof, usize), ValidationError> {
    let (first_char, last_char) =
        match simple_font_code_range(document, font, SimpleFontProgram::Cff)? {
            Ok(range) => range,
            Err(reason) => return Ok((FontProof::Unproved(reason), 0)),
        };

    let Some(descriptor) = font.get("FontDescriptor") else {
        return Ok((
            FontProof::Unproved("simple CFF font has no FontDescriptor"),
            0,
        ));
    };
    let descriptor = resolve_object(document, descriptor)?;
    let Some(descriptor) = descriptor.as_dict() else {
        return Ok((
            FontProof::Unproved("simple CFF FontDescriptor is not a dictionary"),
            0,
        ));
    };
    let Some(program) = descriptor.get("FontFile3") else {
        return Ok((
            FontProof::Unproved("simple Type 1 font has no CFF FontFile3"),
            0,
        ));
    };
    let program = resolve_object(document, program)?;
    let Ok(bytes) = program.decode_stream_data() else {
        return Ok((
            FontProof::Unproved("embedded CFF font cannot be decoded"),
            0,
        ));
    };
    let cff_proof = match inspect_cff_program(&bytes, cff_glyph_budget) {
        Ok(proof) => proof,
        Err(error) if error.exceeded_budget() => {
            return Err(ValidationError::OutputLimitExceeded);
        }
        Err(error) => {
            return Ok((
                FontProof::Unproved(error.reason()),
                error.glyph_count() as usize,
            ));
        }
    };
    let cff_glyphs = cff_proof.glyph_count as usize;
    if !matches!(cff_proof.charset, CffCharsetProof::Named(_)) {
        return Ok((
            FontProof::Unproved("simple FontFile3 is a CID-keyed CFF program"),
            cff_glyphs,
        ));
    }
    let encoding = match simple_pdf_encoding(document, font)? {
        Ok(encoding) => encoding,
        Err(reason) => return Ok((FontProof::Unproved(reason), cff_glyphs)),
    };
    let mut code_states = [CodeState::Unmapped; 256];
    let mut resolved_any = false;
    if let CffCharsetProof::Named(named_glyphs) = &cff_proof.charset {
        for code in first_char..=last_char {
            let glyph_name = encoding
                .1
                .get(&code)
                .map(String::as_str)
                .or_else(|| predefined_simple_glyph_name(&encoding.0, code));
            if let Some(glyph_id) = glyph_name.and_then(|name| named_glyphs.get(name)) {
                let present = *glyph_id < cff_proof.glyph_count;
                code_states[usize::from(code)] = if present {
                    CodeState::Present
                } else {
                    CodeState::Unmapped
                };
                resolved_any |= present;
            }
        }
    }
    if !resolved_any {
        return Ok((
            FontProof::Unproved("simple CFF encoding does not resolve any embedded glyphs"),
            cff_glyphs,
        ));
    }
    Ok((
        FontProof::Simple {
            program: SimpleFontProgram::Cff,
            first_char,
            last_char,
            code_states: Arc::new(code_states),
        },
        cff_glyphs,
    ))
}

fn predefined_simple_glyph_name(encoding: &PdfFontEncoding, code: u8) -> Option<&'static str> {
    match encoding {
        PdfFontEncoding::Standard(name) if name == "WinAnsiEncoding" => match code {
            0x27 => Some("quotesingle"),
            0x60 => Some("grave"),
            _ => FontInfo::gid_to_standard_glyph_name(u16::from(code)),
        },
        PdfFontEncoding::Standard(name) if name == "MacRomanEncoding" => mac_roman_glyph_name(code),
        PdfFontEncoding::Standard(name) if name == "StandardEncoding" => {
            standard_encoding_glyph_name(code)
        }
        _ => None,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the complete Annex D MacRoman table stays explicit and directly auditable"
)]
fn mac_roman_glyph_name(code: u8) -> Option<&'static str> {
    if (0x20..=0x7e).contains(&code) {
        return match code {
            0x27 => Some("quotesingle"),
            0x60 => Some("grave"),
            _ => FontInfo::gid_to_standard_glyph_name(u16::from(code)),
        };
    }
    match code {
        0x80 => Some("Adieresis"),
        0x81 => Some("Aring"),
        0x82 => Some("Ccedilla"),
        0x83 => Some("Eacute"),
        0x84 => Some("Ntilde"),
        0x85 => Some("Odieresis"),
        0x86 => Some("Udieresis"),
        0x87 => Some("aacute"),
        0x88 => Some("agrave"),
        0x89 => Some("acircumflex"),
        0x8a => Some("adieresis"),
        0x8b => Some("atilde"),
        0x8c => Some("aring"),
        0x8d => Some("ccedilla"),
        0x8e => Some("eacute"),
        0x8f => Some("egrave"),
        0x90 => Some("ecircumflex"),
        0x91 => Some("edieresis"),
        0x92 => Some("iacute"),
        0x93 => Some("igrave"),
        0x94 => Some("icircumflex"),
        0x95 => Some("idieresis"),
        0x96 => Some("ntilde"),
        0x97 => Some("oacute"),
        0x98 => Some("ograve"),
        0x99 => Some("ocircumflex"),
        0x9a => Some("odieresis"),
        0x9b => Some("otilde"),
        0x9c => Some("uacute"),
        0x9d => Some("ugrave"),
        0x9e => Some("ucircumflex"),
        0x9f => Some("udieresis"),
        0xa0 => Some("dagger"),
        0xa1 => Some("degree"),
        0xa2 => Some("cent"),
        0xa3 => Some("sterling"),
        0xa4 => Some("section"),
        0xa5 => Some("bullet"),
        0xa6 => Some("paragraph"),
        0xa7 => Some("germandbls"),
        0xa8 => Some("registered"),
        0xa9 => Some("copyright"),
        0xaa => Some("trademark"),
        0xab => Some("acute"),
        0xac => Some("dieresis"),
        0xad => Some("notequal"),
        0xae => Some("AE"),
        0xaf => Some("Oslash"),
        0xb0 => Some("infinity"),
        0xb1 => Some("plusminus"),
        0xb2 => Some("lessequal"),
        0xb3 => Some("greaterequal"),
        0xb4 => Some("yen"),
        0xb5 => Some("mu"),
        0xb6 => Some("partialdiff"),
        0xb7 => Some("summation"),
        0xb8 => Some("product"),
        0xb9 => Some("pi"),
        0xba => Some("integral"),
        0xbb => Some("ordfeminine"),
        0xbc => Some("ordmasculine"),
        0xbd => Some("Omega"),
        0xbe => Some("ae"),
        0xbf => Some("oslash"),
        0xc0 => Some("questiondown"),
        0xc1 => Some("exclamdown"),
        0xc2 => Some("logicalnot"),
        0xc3 => Some("radical"),
        0xc4 => Some("florin"),
        0xc5 => Some("approxequal"),
        0xc6 => Some("Delta"),
        0xc7 => Some("guillemotleft"),
        0xc8 => Some("guillemotright"),
        0xc9 => Some("ellipsis"),
        0xca => Some("space"),
        0xcb => Some("Agrave"),
        0xcc => Some("Atilde"),
        0xcd => Some("Otilde"),
        0xce => Some("OE"),
        0xcf => Some("oe"),
        0xd0 => Some("endash"),
        0xd1 => Some("emdash"),
        0xd2 => Some("quotedblleft"),
        0xd3 => Some("quotedblright"),
        0xd4 => Some("quoteleft"),
        0xd5 => Some("quoteright"),
        0xd6 => Some("divide"),
        0xd7 => Some("lozenge"),
        0xd8 => Some("ydieresis"),
        0xd9 => Some("Ydieresis"),
        0xda => Some("fraction"),
        0xdb => Some("currency"),
        0xdc => Some("guilsinglleft"),
        0xdd => Some("guilsinglright"),
        0xde => Some("fi"),
        0xdf => Some("fl"),
        0xe0 => Some("daggerdbl"),
        0xe1 => Some("periodcentered"),
        0xe2 => Some("quotesinglbase"),
        0xe3 => Some("quotedblbase"),
        0xe4 => Some("perthousand"),
        0xe5 => Some("Acircumflex"),
        0xe6 => Some("Ecircumflex"),
        0xe7 => Some("Aacute"),
        0xe8 => Some("Edieresis"),
        0xe9 => Some("Egrave"),
        0xea => Some("Iacute"),
        0xeb => Some("Icircumflex"),
        0xec => Some("Idieresis"),
        0xed => Some("Igrave"),
        0xee => Some("Oacute"),
        0xef => Some("Ocircumflex"),
        0xf1 => Some("Ograve"),
        0xf2 => Some("Uacute"),
        0xf3 => Some("Ucircumflex"),
        0xf4 => Some("Ugrave"),
        0xf5 => Some("dotlessi"),
        0xf6 => Some("circumflex"),
        0xf7 => Some("tilde"),
        0xf8 => Some("macron"),
        0xf9 => Some("breve"),
        0xfa => Some("dotaccent"),
        0xfb => Some("ring"),
        0xfc => Some("cedilla"),
        0xfd => Some("hungarumlaut"),
        0xfe => Some("ogonek"),
        0xff => Some("caron"),
        _ => None,
    }
}

fn standard_encoding_glyph_name(code: u8) -> Option<&'static str> {
    if (0x20..=0x7e).contains(&code) {
        return FontInfo::gid_to_standard_glyph_name(u16::from(code));
    }
    match code {
        0xa1 => Some("exclamdown"),
        0xa2 => Some("cent"),
        0xa3 => Some("sterling"),
        0xa4 => Some("fraction"),
        0xa5 => Some("yen"),
        0xa6 => Some("florin"),
        0xa7 => Some("section"),
        0xa8 => Some("currency"),
        0xa9 => Some("quotesingle"),
        0xaa => Some("quotedblleft"),
        0xab => Some("guillemotleft"),
        0xac => Some("guilsinglleft"),
        0xad => Some("guilsinglright"),
        0xae => Some("fi"),
        0xaf => Some("fl"),
        0xb1 => Some("endash"),
        0xb2 => Some("dagger"),
        0xb3 => Some("daggerdbl"),
        0xb4 => Some("periodcentered"),
        0xb6 => Some("paragraph"),
        0xb7 => Some("bullet"),
        0xb8 => Some("quotesinglbase"),
        0xb9 => Some("quotedblbase"),
        0xba => Some("quotedblright"),
        0xbb => Some("guillemotright"),
        0xbc => Some("ellipsis"),
        0xbd => Some("perthousand"),
        0xbf => Some("questiondown"),
        0xc1 => Some("grave"),
        0xc2 => Some("acute"),
        0xc3 => Some("circumflex"),
        0xc4 => Some("tilde"),
        0xc5 => Some("macron"),
        0xc6 => Some("breve"),
        0xc7 => Some("dotaccent"),
        0xc8 => Some("dieresis"),
        0xca => Some("ring"),
        0xcb => Some("cedilla"),
        0xcd => Some("hungarumlaut"),
        0xce => Some("ogonek"),
        0xcf => Some("caron"),
        0xe1 => Some("AE"),
        0xe3 => Some("ordfeminine"),
        0xe8 => Some("Lslash"),
        0xe9 => Some("Oslash"),
        0xea => Some("OE"),
        0xeb => Some("ordmasculine"),
        0xf1 => Some("ae"),
        0xf5 => Some("dotlessi"),
        0xf8 => Some("lslash"),
        0xf9 => Some("oslash"),
        0xfa => Some("oe"),
        0xfb => Some("germandbls"),
        _ => None,
    }
}

fn simple_truetype_proof(
    document: &PdfDocument,
    font: &HashMap<String, Object>,
) -> Result<(FontProof, usize), ValidationError> {
    let (first_char, last_char) =
        match simple_font_code_range(document, font, SimpleFontProgram::TrueType)? {
            Ok(range) => range,
            Err(reason) => return Ok((FontProof::Unproved(reason), 0)),
        };

    let font_object = Object::Dictionary(font.clone());
    let Ok(mut font_info) = FontInfo::from_dict(&font_object, document) else {
        return Ok((
            FontProof::Unproved("simple TrueType dictionary cannot be parsed"),
            0,
        ));
    };
    if font_info.is_symbolic() {
        return Ok((
            FontProof::Unproved(
                "symbolic simple TrueType fonts have no independent code-to-glyph proof",
            ),
            0,
        ));
    }
    font_info.to_unicode = None;
    let Some(bytes) = font_info.embedded_font_data.as_deref() else {
        return Ok((
            FontProof::Unproved("simple TrueType embedded program cannot be decoded"),
            0,
        ));
    };
    let Ok(face) = ttf_parser::Face::parse(bytes, 0) else {
        return Ok((
            FontProof::Unproved("simple TrueType embedded program cannot be parsed"),
            0,
        ));
    };
    let use_mac_roman_cmap = matches!(
        &font_info.encoding,
        PdfFontEncoding::Standard(name) if name == "MacRomanEncoding"
    );
    let mut code_states = [CodeState::Unmapped; 256];
    for code in first_char..=last_char {
        let Some(unicode) = font_info.char_to_unicode(u32::from(code)) else {
            continue;
        };
        let mut characters = unicode.chars();
        let Some(character) = characters.next() else {
            continue;
        };
        if characters.next().is_some() {
            continue;
        }
        if face
            .glyph_index(character)
            .is_some_and(|glyph_id| glyph_id.0 != 0)
            || standard_ligature_glyph_name(character)
                .is_some_and(|name| face_has_named_glyph(&face, name))
            || (use_mac_roman_cmap && face_has_mac_roman_code(&face, code))
        {
            code_states[usize::from(code)] = CodeState::Present;
        }
    }
    Ok((
        FontProof::Simple {
            program: SimpleFontProgram::TrueType,
            first_char,
            last_char,
            code_states: Arc::new(code_states),
        },
        0,
    ))
}

fn standard_ligature_glyph_name(character: char) -> Option<&'static str> {
    match character {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        _ => None,
    }
}

fn face_has_named_glyph(face: &ttf_parser::Face<'_>, expected: &str) -> bool {
    (1..face.number_of_glyphs())
        .any(|glyph_id| face.glyph_name(ttf_parser::GlyphId(glyph_id)) == Some(expected))
}

fn face_has_mac_roman_code(face: &ttf_parser::Face<'_>, code: u8) -> bool {
    face.tables().cmap.is_some_and(|cmap| {
        cmap.subtables.into_iter().any(|subtable| {
            subtable.platform_id == ttf_parser::PlatformId::Macintosh
                && subtable.encoding_id == 0
                && subtable.glyph_index(u32::from(code)).is_some()
        })
    })
}

type SimpleEncoding = (PdfFontEncoding, HashMap<u8, String>);

fn simple_pdf_encoding(
    document: &PdfDocument,
    font: &HashMap<String, Object>,
) -> Result<Result<SimpleEncoding, &'static str>, ValidationError> {
    let Some(encoding) = font.get("Encoding") else {
        return Ok(Err("simple CFF font has no explicit Encoding"));
    };
    let encoding = resolve_object(document, encoding)?;
    if let Some(name) = encoding.as_name() {
        return Ok(supported_simple_encoding(name)
            .map(|encoding| (encoding, HashMap::new()))
            .ok_or("simple CFF font uses an unsupported predefined Encoding"));
    }
    let Some(dictionary) = encoding.as_dict() else {
        return Ok(Err(
            "simple CFF Encoding is neither a name nor a dictionary",
        ));
    };
    let Some(base_encoding) = dictionary.get("BaseEncoding") else {
        return Ok(Err("simple CFF Encoding dictionary has no BaseEncoding"));
    };
    let base_encoding = resolve_object(document, base_encoding)?;
    let Some(base_encoding) = base_encoding.as_name() else {
        return Ok(Err("simple CFF BaseEncoding is not a name"));
    };
    let Some(base_encoding) = supported_simple_encoding(base_encoding) else {
        return Ok(Err("simple CFF font uses an unsupported BaseEncoding"));
    };
    let mut differences = HashMap::new();
    if let Some(entries) = dictionary.get("Differences") {
        let entries = resolve_object(document, entries)?;
        let Some(entries) = entries.as_array() else {
            return Ok(Err("simple CFF Differences is not an array"));
        };
        if entries.len() > 512 {
            return Ok(Err(
                "simple CFF Differences exceeds the independent proof limit",
            ));
        }
        let mut next_code = None;
        for entry in entries {
            let entry = resolve_object(document, entry)?;
            match entry {
                Object::Integer(code) if (0..=255).contains(&code) => {
                    next_code = Some(u8::try_from(code).expect("bounded character code"));
                }
                Object::Name(name) if next_code.is_some() && name.len() <= MAX_FONT_NAME_BYTES => {
                    let code = next_code.expect("checked character code");
                    differences.insert(code, name);
                    next_code = code.checked_add(1);
                }
                _ => return Ok(Err("simple CFF Differences contains an invalid entry")),
            }
        }
    }
    Ok(Ok((base_encoding, differences)))
}

fn supported_simple_encoding(name: &str) -> Option<PdfFontEncoding> {
    matches!(
        name,
        "WinAnsiEncoding" | "MacRomanEncoding" | "StandardEncoding"
    )
    .then(|| PdfFontEncoding::Standard(name.to_owned()))
}

fn identity_type0_proof(
    document: &PdfDocument,
    font: &HashMap<String, Object>,
    cff_glyph_budget: usize,
    cid_to_gid_byte_budget: usize,
) -> Result<(FontProof, usize, usize), ValidationError> {
    let Some(encoding) = font.get("Encoding") else {
        return Ok((FontProof::Unproved("Type 0 font has no Encoding"), 0, 0));
    };
    let encoding = resolve_object(document, encoding)?;
    if !matches!(encoding.as_name(), Some("Identity-H" | "Identity-V")) {
        return Ok((
            FontProof::Unproved(
                "only Identity-H and Identity-V encodings have independent used-glyph proof",
            ),
            0,
            0,
        ));
    }
    let Some(descendants) = font.get("DescendantFonts") else {
        return Ok((
            FontProof::Unproved("Type 0 font has no DescendantFonts"),
            0,
            0,
        ));
    };
    let descendants = resolve_object(document, descendants)?;
    let Some(descendants) = descendants.as_array() else {
        return Ok((
            FontProof::Unproved("Type 0 DescendantFonts is not an array"),
            0,
            0,
        ));
    };
    if descendants.len() != 1 {
        return Ok((
            FontProof::Unproved(
                "Type 0 font must have exactly one descendant for used-glyph proof",
            ),
            0,
            0,
        ));
    }
    let descendant = resolve_object(document, &descendants[0])?;
    let Some(descendant) = descendant.as_dict() else {
        return Ok((
            FontProof::Unproved("descendant font is not a dictionary"),
            0,
            0,
        ));
    };
    match descendant.get("Subtype").and_then(Object::as_name) {
        Some("CIDFontType2") => {
            identity_type2_descendant_proof(document, descendant, cid_to_gid_byte_budget)
        }
        Some("CIDFontType0") => {
            identity_type0_cff_descendant_proof(document, descendant, cff_glyph_budget)
                .map(|(proof, glyphs)| (proof, glyphs, 0))
        }
        _ => Ok((
            FontProof::Unproved(
                "only CIDFontType0 and CIDFontType2 descendants have independent CID-to-glyph proof",
            ),
            0,
            0,
        )),
    }
}

struct CidToGidMapDecode {
    map: Result<Arc<[u8]>, &'static str>,
    charged_bytes: usize,
}

fn decode_cid_to_gid_map(
    object: &Object,
    remaining_byte_budget: usize,
) -> Result<CidToGidMapDecode, ValidationError> {
    let Object::Stream { dict, data } = object else {
        return Ok(CidToGidMapDecode {
            map: Err("CIDFontType2 CIDToGIDMap is neither Identity nor a stream"),
            charged_bytes: 0,
        });
    };
    if data.len() > MAX_CID_TO_GID_MAP_ENCODED_BYTES {
        return Ok(CidToGidMapDecode {
            map: Err("CIDFontType2 CIDToGIDMap encoded stream exceeds the proof limit"),
            charged_bytes: 0,
        });
    }
    if data.len() > remaining_byte_budget {
        return Err(ValidationError::OutputLimitExceeded);
    }
    let mut charged_bytes = data.len();
    let bytes = match dict.get("Filter") {
        None => {
            if data.len() > MAX_CID_TO_GID_MAP_BYTES {
                return Ok(CidToGidMapDecode {
                    map: Err("CIDFontType2 CIDToGIDMap decoded stream exceeds the proof limit"),
                    charged_bytes,
                });
            }
            data.to_vec()
        }
        Some(filter)
            if is_single_flate_filter(filter)
                && flate_decode_parameters_are_noop(filter, dict.get("DecodeParms")) =>
        {
            use std::io::Read as _;

            let output_limit = MAX_CID_TO_GID_MAP_BYTES
                .min(remaining_byte_budget)
                .saturating_add(1);
            let inflater = flate2::read::ZlibDecoder::new(data.as_ref());
            let mut bounded = inflater.take(output_limit as u64);
            let mut decoded = Vec::with_capacity(output_limit.min(8 * 1024));
            if bounded.read_to_end(&mut decoded).is_err() {
                charged_bytes = charged_bytes.max(decoded.len());
                return Ok(CidToGidMapDecode {
                    map: Err("CIDFontType2 CIDToGIDMap stream cannot be decoded"),
                    charged_bytes,
                });
            }
            charged_bytes = charged_bytes.max(decoded.len());
            if decoded.len() > MAX_CID_TO_GID_MAP_BYTES {
                return Ok(CidToGidMapDecode {
                    map: Err("CIDFontType2 CIDToGIDMap decoded stream exceeds the proof limit"),
                    charged_bytes,
                });
            }
            if charged_bytes > remaining_byte_budget {
                return Err(ValidationError::OutputLimitExceeded);
            }
            decoded
        }
        _ => {
            return Ok(CidToGidMapDecode {
                map: Err(
                    "CIDFontType2 CIDToGIDMap uses an unsupported stream filter or decode predictor",
                ),
                charged_bytes,
            });
        }
    };
    if bytes.len() % 2 != 0 {
        return Ok(CidToGidMapDecode {
            map: Err("CIDFontType2 CIDToGIDMap contains an incomplete two-byte GID"),
            charged_bytes,
        });
    }
    Ok(CidToGidMapDecode {
        map: Ok(bytes.into()),
        charged_bytes,
    })
}

fn is_single_flate_filter(filter: &Object) -> bool {
    match filter {
        Object::Name(name) => name == "FlateDecode",
        Object::Array(filters) => {
            matches!(filters.as_slice(), [Object::Name(name)] if name == "FlateDecode")
        }
        _ => false,
    }
}

fn flate_decode_parameters_are_noop(filter: &Object, parameters: Option<&Object>) -> bool {
    let Some(parameters) = parameters else {
        return true;
    };
    if parameters.is_null() {
        return true;
    }
    match filter {
        Object::Name(_) => flate_parameter_object_is_noop(parameters),
        Object::Array(_) => matches!(
            parameters,
            Object::Array(values)
                if matches!(values.as_slice(), [value] if flate_parameter_object_is_noop(value))
        ),
        _ => false,
    }
}

fn flate_parameter_object_is_noop(parameters: &Object) -> bool {
    if parameters.is_null() {
        return true;
    }
    let Some(dictionary) = parameters.as_dict() else {
        return false;
    };
    dictionary
        .get("Predictor")
        .is_none_or(|predictor| predictor.as_integer() == Some(1))
}

fn identity_type2_descendant_proof(
    document: &PdfDocument,
    descendant: &HashMap<String, Object>,
    cid_to_gid_byte_budget: usize,
) -> Result<(FontProof, usize, usize), ValidationError> {
    let (cid_to_gid, cid_to_gid_bytes) = if let Some(cid_to_gid) = descendant.get("CIDToGIDMap") {
        let cid_to_gid = resolve_object(document, cid_to_gid)?;
        if cid_to_gid.as_name() == Some("Identity") {
            (None, 0)
        } else {
            let decoded = decode_cid_to_gid_map(&cid_to_gid, cid_to_gid_byte_budget)?;
            match decoded.map {
                Ok(map) => (Some(map), decoded.charged_bytes),
                Err(reason) => {
                    return Ok((FontProof::Unproved(reason), 0, decoded.charged_bytes));
                }
            }
        }
    } else {
        (None, 0)
    };
    let Some(descriptor) = descendant.get("FontDescriptor") else {
        return Ok((
            FontProof::Unproved("CIDFontType2 has no FontDescriptor"),
            0,
            cid_to_gid_bytes,
        ));
    };
    let descriptor = resolve_object(document, descriptor)?;
    let Some(descriptor) = descriptor.as_dict() else {
        return Ok((
            FontProof::Unproved("CIDFontType2 FontDescriptor is not a dictionary"),
            0,
            cid_to_gid_bytes,
        ));
    };
    let Some((program_key, program)) = descriptor
        .get("FontFile2")
        .map(|program| ("FontFile2", program))
        .or_else(|| {
            descriptor
                .get("FontFile3")
                .map(|program| ("FontFile3", program))
        })
    else {
        return Ok((
            FontProof::Unproved("CIDFontType2 has no TrueType or OpenType font program"),
            0,
            cid_to_gid_bytes,
        ));
    };
    let program = resolve_object(document, program)?;
    if program_key == "FontFile3" {
        let Object::Stream { dict, .. } = &program else {
            return Ok((
                FontProof::Unproved("CIDFontType2 FontFile3 is not a stream"),
                0,
                cid_to_gid_bytes,
            ));
        };
        let is_open_type = if let Some(subtype) = dict.get("Subtype") {
            resolve_object(document, subtype)?
                .as_name()
                .is_some_and(|name| name == "OpenType")
        } else {
            false
        };
        if !is_open_type {
            return Ok((
                FontProof::Unproved("CIDFontType2 FontFile3 is not an OpenType program"),
                0,
                cid_to_gid_bytes,
            ));
        }
    }
    let Ok(bytes) = program.decode_stream_data() else {
        return Ok((
            FontProof::Unproved("embedded TrueType font cannot be decoded"),
            0,
            cid_to_gid_bytes,
        ));
    };
    let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else {
        return Ok((
            FontProof::Unproved("embedded TrueType font cannot be parsed"),
            0,
            cid_to_gid_bytes,
        ));
    };
    if face.tables().glyf.is_none() {
        return Ok((
            FontProof::Unproved("CIDFontType2 OpenType program does not contain TrueType outlines"),
            0,
            cid_to_gid_bytes,
        ));
    }
    Ok((
        FontProof::IdentityType2 {
            glyph_count: face.number_of_glyphs(),
            cid_to_gid,
        },
        0,
        cid_to_gid_bytes,
    ))
}

fn identity_type0_cff_descendant_proof(
    document: &PdfDocument,
    descendant: &HashMap<String, Object>,
    cff_glyph_budget: usize,
) -> Result<(FontProof, usize), ValidationError> {
    let Some(descriptor) = descendant.get("FontDescriptor") else {
        return Ok((FontProof::Unproved("CIDFontType0 has no FontDescriptor"), 0));
    };
    let descriptor = resolve_object(document, descriptor)?;
    let Some(descriptor) = descriptor.as_dict() else {
        return Ok((
            FontProof::Unproved("CIDFontType0 FontDescriptor is not a dictionary"),
            0,
        ));
    };
    let Some(program) = descriptor.get("FontFile3") else {
        return Ok((FontProof::Unproved("CIDFontType0 has no FontFile3"), 0));
    };
    let program = resolve_object(document, program)?;
    let Ok(bytes) = program.decode_stream_data() else {
        return Ok((
            FontProof::Unproved("embedded CFF font cannot be decoded"),
            0,
        ));
    };
    let proof = match inspect_cff_program(&bytes, cff_glyph_budget) {
        Ok(proof) => proof,
        Err(error) if error.exceeded_budget() => {
            return Err(ValidationError::OutputLimitExceeded);
        }
        Err(error) => {
            return Ok((
                FontProof::Unproved(error.reason()),
                error.glyph_count() as usize,
            ));
        }
    };
    let glyph_count = proof.glyph_count as usize;
    let CffCharsetProof::Cid(cids) = proof.charset else {
        return Ok((
            FontProof::Unproved("CIDFontType0 FontFile3 is not a CID-keyed CFF program"),
            glyph_count,
        ));
    };
    Ok((
        FontProof::IdentityType0Cff { cids: cids.into() },
        glyph_count,
    ))
}

fn type3_proof(
    document: &PdfDocument,
    font: &HashMap<String, Object>,
) -> Result<FontProof, ValidationError> {
    let Some(first_char) = font.get("FirstChar") else {
        return Ok(FontProof::Unproved("Type 3 font has no FirstChar"));
    };
    let first_char = resolve_object(document, first_char)?;
    let Some(first_char) = first_char
        .as_integer()
        .and_then(|value| u8::try_from(value).ok())
    else {
        return Ok(FontProof::Unproved(
            "Type 3 FirstChar is not a one-byte character code",
        ));
    };
    let Some(last_char) = font.get("LastChar") else {
        return Ok(FontProof::Unproved("Type 3 font has no LastChar"));
    };
    let last_char = resolve_object(document, last_char)?;
    let Some(last_char) = last_char
        .as_integer()
        .and_then(|value| u8::try_from(value).ok())
    else {
        return Ok(FontProof::Unproved(
            "Type 3 LastChar is not a one-byte character code",
        ));
    };
    if first_char > last_char {
        return Ok(FontProof::Unproved(
            "Type 3 FirstChar is greater than LastChar",
        ));
    }
    let Some(encoding) = font.get("Encoding") else {
        return Ok(FontProof::Unproved("Type 3 font has no Encoding"));
    };
    let encoding = resolve_object(document, encoding)?;
    let Some(encoding) = encoding.as_dict() else {
        return Ok(FontProof::Unproved(
            "only a Type 3 Encoding dictionary with explicit Differences is independently proved",
        ));
    };
    let Some(differences) = encoding.get("Differences") else {
        return Ok(FontProof::Unproved(
            "Type 3 Encoding has no explicit Differences mapping",
        ));
    };
    let differences = resolve_object(document, differences)?;
    let Some(differences) = differences.as_array() else {
        return Ok(FontProof::Unproved(
            "Type 3 Encoding Differences is not an array",
        ));
    };
    let Some(char_procs) = font.get("CharProcs") else {
        return Ok(FontProof::Unproved("Type 3 font has no CharProcs"));
    };
    let char_procs = resolve_object(document, char_procs)?;
    let Some(char_procs) = char_procs.as_dict() else {
        return Ok(FontProof::Unproved("Type 3 CharProcs is not a dictionary"));
    };

    let mut code_states = [CodeState::Unmapped; 256];
    let mut next_code = None;
    for entry in differences {
        let entry = resolve_object(document, entry)?;
        match entry {
            Object::Integer(code) if (0..=255).contains(&code) => {
                next_code = Some(u8::try_from(code).expect("bounded character code"));
            }
            Object::Name(name) => {
                let Some(code) = next_code else {
                    return Ok(FontProof::Unproved(
                        "Type 3 Encoding Differences begins with a glyph name",
                    ));
                };
                code_states[usize::from(code)] = if char_procs
                    .get(&name)
                    .is_some_and(|char_proc| type3_charproc_is_readable(document, char_proc))
                {
                    CodeState::Present
                } else {
                    CodeState::Missing
                };
                next_code = code.checked_add(1);
            }
            _ => {
                return Ok(FontProof::Unproved(
                    "Type 3 Encoding Differences contains an invalid entry",
                ));
            }
        }
    }
    Ok(FontProof::Type3 {
        first_char,
        last_char,
        code_states: Arc::new(code_states),
    })
}

fn type3_charproc_is_readable(document: &PdfDocument, char_proc: &Object) -> bool {
    let Ok(char_proc) = resolve_object(document, char_proc) else {
        return false;
    };
    matches!(char_proc, Object::Stream { .. }) && char_proc.decode_stream_data().is_ok()
}

fn page_resources(document: &PdfDocument, page_index: usize) -> Result<Object, ValidationError> {
    let mut current = document
        .get_page(page_index)
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let mut seen = HashSet::new();
    for _ in 0..MAX_REFERENCE_DEPTH {
        let dictionary = current.as_dict().ok_or_else(|| {
            ValidationError::MalformedReport(format!("page {} is not a dictionary", page_index + 1))
        })?;
        if let Some(resources) = dictionary.get("Resources") {
            return resolve_object(document, resources);
        }
        let Some(parent) = dictionary.get("Parent") else {
            return Ok(Object::Dictionary(HashMap::new()));
        };
        if let Object::Reference(reference) = parent
            && !seen.insert(*reference)
        {
            return Err(ValidationError::MalformedReport(
                "page resource inheritance contains a cycle".into(),
            ));
        }
        current = resolve_object(document, parent)?;
    }
    Err(ValidationError::OutputLimitExceeded)
}

fn lookup_resource(
    document: &PdfDocument,
    resources: &Object,
    category: &str,
    name: &str,
) -> Result<Option<Object>, ValidationError> {
    let resources = resolve_object(document, resources)?;
    let Some(resources) = resources.as_dict() else {
        return Ok(None);
    };
    let Some(category) = resources.get(category) else {
        return Ok(None);
    };
    let category = resolve_object(document, category)?;
    let Some(category) = category.as_dict() else {
        return Ok(None);
    };
    Ok(category.get(name).cloned())
}

fn resolve_object(document: &PdfDocument, object: &Object) -> Result<Object, ValidationError> {
    let mut current = object.clone();
    let mut seen = HashSet::new();
    for _ in 0..MAX_REFERENCE_DEPTH {
        let Object::Reference(reference) = current else {
            return Ok(current);
        };
        if !seen.insert(reference) {
            return Err(ValidationError::MalformedReport(
                "PDF object references contain a cycle".into(),
            ));
        }
        current = document
            .load_object(reference)
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    }
    Err(ValidationError::OutputLimitExceeded)
}

fn bounded_name(name: &str) -> String {
    let mut characters = name.chars();
    let mut bounded = String::with_capacity(MAX_REPORTED_NAME_CHARS + 3);
    for character in characters.by_ref().take(MAX_REPORTED_NAME_CHARS) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_ansi_uses_literal_ascii_quote_and_grave_names() {
        let encoding = PdfFontEncoding::Standard("WinAnsiEncoding".to_owned());
        assert_eq!(
            predefined_simple_glyph_name(&encoding, 0x27),
            Some("quotesingle")
        );
        assert_eq!(predefined_simple_glyph_name(&encoding, 0x60), Some("grave"));
        assert_eq!(
            predefined_simple_glyph_name(&encoding, 0x91),
            Some("quoteleft")
        );
    }

    #[test]
    fn mac_roman_uses_its_annex_d_names() {
        assert_eq!(mac_roman_glyph_name(0x27), Some("quotesingle"));
        assert_eq!(mac_roman_glyph_name(0x8a), Some("adieresis"));
        assert_eq!(mac_roman_glyph_name(0xae), Some("AE"));
        assert_eq!(mac_roman_glyph_name(0xdb), Some("currency"));
        assert_eq!(mac_roman_glyph_name(0xf0), None);
    }

    #[test]
    fn standard_encoding_retains_postscript_quote_and_high_byte_names() {
        assert_eq!(standard_encoding_glyph_name(0x27), Some("quoteright"));
        assert_eq!(standard_encoding_glyph_name(0x60), Some("quoteleft"));
        assert_eq!(standard_encoding_glyph_name(0xa9), Some("quotesingle"));
        assert_eq!(standard_encoding_glyph_name(0xae), Some("fi"));
        assert_eq!(standard_encoding_glyph_name(0xf1), Some("ae"));
    }

    #[test]
    fn cid_to_gid_map_respects_the_remaining_aggregate_budget() {
        let stream = Object::Stream {
            dict: HashMap::new(),
            data: vec![0, 1].into(),
        };

        assert!(matches!(
            decode_cid_to_gid_map(&stream, 1),
            Err(ValidationError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn malformed_cid_to_gid_maps_retain_their_bounded_work_charge() {
        let odd_stream = Object::Stream {
            dict: HashMap::new(),
            data: vec![0, 1, 0].into(),
        };
        let odd = decode_cid_to_gid_map(&odd_stream, MAX_TOTAL_CID_TO_GID_MAP_BYTES)
            .expect("odd map remains a bounded proof result");
        assert!(odd.map.is_err());
        assert_eq!(odd.charged_bytes, 3);

        let malformed_flate = Object::Stream {
            dict: HashMap::from([("Filter".to_owned(), Object::Name("FlateDecode".to_owned()))]),
            data: b"not a zlib stream".to_vec().into(),
        };
        let malformed = decode_cid_to_gid_map(&malformed_flate, MAX_TOTAL_CID_TO_GID_MAP_BYTES)
            .expect("malformed compressed map remains a bounded proof result");
        assert!(malformed.map.is_err());
        assert_eq!(malformed.charged_bytes, b"not a zlib stream".len());
    }
}
