//! Active color-space audit for the composite PDF/X authority.

use lcms2::{ColorSpaceSignature, Intent, PixelFormat, Profile, ProfileClassSignature, Transform};
use pdf_oxide::{
    content::{Operator, parse_content_stream},
    document::PdfDocument,
    object::{Object, ObjectRef},
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;

use crate::ValidationError;

pub const ACTIVE_ICC_NAME: &str = "LittleCMS and Publisher active color-space validator";
pub const ACTIVE_ICC_VERSION: &str = "6";
pub const ACTIVE_ICC_SOURCE_IDENTITY: &str =
    "publisher-v2:active-color-space-audit:v6+LittleCMS/lcms2-rs-6.2.0";
pub const ACTIVE_ICC_RULES_IDENTITY: &str =
    "publisher-v2/pdf-x-4/active-color-space-validity-and-spot-consistency/v6";

const MAX_REFERENCE_DEPTH: usize = 32;
const MAX_CONTENT_DEPTH: usize = 32;
const MAX_CONTENT_OPERATORS: usize = 1_000_000;
const MAX_AUDITED_PROFILES: usize = 4_096;
const MAX_FINDINGS: usize = 512;
const MAX_TOTAL_PROFILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_REPORTED_NAME_CHARS: usize = 64;
const MAX_DISTINCT_SPOT_COLORANTS: usize = 4_096;
const MAX_INDEXED_LOOKUP_BYTES: usize = 32 * 256;
const MAX_INDEXED_LOOKUP_ENCODED_BYTES: usize = 64 * 1024;
const MAX_INDEXED_LOOKUP_STREAMS: usize = 4_096;
const MAX_INDEXED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_INDEXED_IMAGE_STREAMS: usize = 4_096;

#[derive(Debug, Default)]
pub struct ActiveIccProfileAudit {
    profiles_checked: usize,
    findings: Vec<String>,
}

impl ActiveIccProfileAudit {
    #[must_use]
    pub const fn profiles_checked(&self) -> usize {
        self.profiles_checked
    }

    #[must_use]
    pub fn findings(&self) -> &[String] {
        &self.findings
    }

    #[must_use]
    pub fn into_findings(self) -> Vec<String> {
        self.findings
    }
}

/// Parses the exact candidate and validates ICC profiles reached by active page,
/// image, form, shading, pattern, and inline-image color-space operations.
///
/// # Errors
///
/// Returns a validator error when the PDF cannot be traversed safely or an
/// operation, profile, or finding budget is exceeded. Invalid active profiles
/// are ordinary blocking findings rather than execution errors.
pub fn audit_active_icc_profiles(
    candidate: &[u8],
) -> Result<ActiveIccProfileAudit, ValidationError> {
    let document = PdfDocument::from_bytes(candidate.to_vec())
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let page_count = document
        .page_count()
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let mut context = AuditContext {
        output_intent_space: output_intent_color_space(&document)?,
        ..AuditContext::default()
    };

    for page_index in 0..page_count {
        let resources = page_resources(&document, page_index)?;
        let page_content = document
            .get_page_content_data(page_index)
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        context.walk_content(
            &page_content,
            ContentWalkContext {
                document: &document,
                resources: &resources,
                page_number: page_index + 1,
                owner: "page",
                depth: 0,
                uncolored_pattern: false,
            },
            PaintColorState::default(),
        )?;
    }

    Ok(ActiveIccProfileAudit {
        profiles_checked: context.profiles_checked,
        findings: context.findings,
    })
}

#[derive(Default)]
struct AuditContext {
    profiles_checked: usize,
    total_profile_bytes: usize,
    operators_checked: usize,
    findings: Vec<String>,
    output_intent_space: Option<&'static str>,
    seen_profiles: HashSet<([u8; 32], i64)>,
    spot_alternates: HashMap<String, String>,
    indexed_lookup_lengths: HashMap<ObjectRef, Option<usize>>,
    indexed_image_data: HashMap<ObjectRef, IndexedImageDataProof>,
    indexed_image_encoded_bytes: usize,
    indexed_image_decoded_bytes: usize,
    active_streams: HashSet<ObjectRef>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexedImageDataProof {
    Decoded(usize),
    DeferredJpx,
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IndexedImageDataAttempt {
    proof: IndexedImageDataProof,
    decoded_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SelectedPaintColorSpace {
    #[default]
    Other,
    Indexed {
        hival: i64,
    },
    ColoredPattern,
    UncoloredPattern {
        base_components: usize,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PaintColorState {
    fill: SelectedPaintColorSpace,
    stroke: SelectedPaintColorSpace,
}

#[derive(Clone, Copy)]
struct ContentWalkContext<'a> {
    document: &'a PdfDocument,
    resources: &'a Object,
    page_number: usize,
    owner: &'a str,
    depth: usize,
    uncolored_pattern: bool,
}

impl AuditContext {
    fn selected_paint_color_space(
        document: &PdfDocument,
        resources: &Object,
        name: &str,
    ) -> Result<SelectedPaintColorSpace, ValidationError> {
        let normalized = normalize_device_color_space(name);
        if normalized == "Pattern" {
            return Ok(SelectedPaintColorSpace::ColoredPattern);
        }
        if is_device_color_space(normalized) {
            return Ok(SelectedPaintColorSpace::Other);
        }
        let Some(color_space) = lookup_resource(document, resources, "ColorSpace", normalized)?
        else {
            return Ok(SelectedPaintColorSpace::Other);
        };
        let color_space = resolve_object(document, &color_space)?;
        let Some(array) = color_space.as_array() else {
            return Ok(SelectedPaintColorSpace::Other);
        };
        match array.first().and_then(Object::as_name) {
            Some("Indexed") if array.len() == 4 => {
                let Some(hival) = resolve_object(document, &array[2])?
                    .as_integer()
                    .filter(|value| (0..=255).contains(value))
                else {
                    return Ok(SelectedPaintColorSpace::Other);
                };
                Ok(SelectedPaintColorSpace::Indexed { hival })
            }
            Some("Pattern") if array.len() == 2 => {
                let Some(base_components) =
                    color_space_component_count(document, resources, &array[1], 0)?
                else {
                    return Ok(SelectedPaintColorSpace::Other);
                };
                Ok(SelectedPaintColorSpace::UncoloredPattern { base_components })
            }
            _ => Ok(SelectedPaintColorSpace::Other),
        }
    }

    fn inspect_indexed_paint_components(
        &mut self,
        selected: SelectedPaintColorSpace,
        components: &[f32],
        page_number: usize,
        owner: &str,
        paint_kind: &str,
    ) -> Result<(), ValidationError> {
        let SelectedPaintColorSpace::Indexed { hival } = selected else {
            return Ok(());
        };
        if components.len() != 1 || !components[0].is_finite() {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} {paint_kind} in an Indexed color space with /hival {hival} must provide exactly one finite numeric component"
            ))?;
        }
        Ok(())
    }

    fn inspect_pattern_paint_components(
        &mut self,
        selected: SelectedPaintColorSpace,
        components: &[f32],
        page_number: usize,
        owner: &str,
        paint_kind: &str,
    ) -> Result<(), ValidationError> {
        let expected_components = match selected {
            SelectedPaintColorSpace::ColoredPattern => 0,
            SelectedPaintColorSpace::UncoloredPattern { base_components } => base_components,
            _ => return Ok(()),
        };
        if components.len() != expected_components
            || components.iter().any(|component| !component.is_finite())
        {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} {paint_kind} Pattern selection must provide exactly {expected_components} finite numeric component(s) before the pattern name"
            ))?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Keep the ordered PDF operator state machine centralized and auditable.
    fn walk_content(
        &mut self,
        content: &[u8],
        walk: ContentWalkContext<'_>,
        initial_paint_state: PaintColorState,
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources,
            page_number,
            owner,
            depth,
            uncolored_pattern,
        } = walk;
        if depth > MAX_CONTENT_DEPTH {
            return Err(ValidationError::OutputLimitExceeded);
        }
        let operators = parse_content_stream(content)
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        self.operators_checked = self
            .operators_checked
            .checked_add(operators.len())
            .filter(|count| *count <= MAX_CONTENT_OPERATORS)
            .ok_or(ValidationError::OutputLimitExceeded)?;

        let stream_entry_paint_state = initial_paint_state;
        let mut paint_state = initial_paint_state;
        let mut paint_state_stack = Vec::new();
        for operator in operators {
            if uncolored_pattern
                && (operator.is_color_setting()
                    || matches!(
                        operator,
                        Operator::SetRenderingIntent { .. } | Operator::PaintShading { .. }
                    ))
            {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} uncolored tiling pattern content specifies color information"
                ))?;
            }
            match operator {
                Operator::SaveState => paint_state_stack.push(paint_state),
                Operator::RestoreState => {
                    if let Some(saved_state) = paint_state_stack.pop() {
                        paint_state = saved_state;
                    } else {
                        self.add_finding(format!(
                            "blocking authority rule: page {page_number} {owner} restores graphics state with Q without a matching q"
                        ))?;
                    }
                }
                Operator::SetFillColorSpace { name } => {
                    self.inspect_named_color_space(
                        document,
                        resources,
                        &name,
                        page_number,
                        owner,
                        0,
                    )?;
                    paint_state.fill =
                        Self::selected_paint_color_space(document, resources, &name)?;
                }
                Operator::SetStrokeColorSpace { name } => {
                    self.inspect_named_color_space(
                        document,
                        resources,
                        &name,
                        page_number,
                        owner,
                        0,
                    )?;
                    paint_state.stroke =
                        Self::selected_paint_color_space(document, resources, &name)?;
                }
                Operator::SetFillRgb { .. } => {
                    self.inspect_device_color_space(
                        document,
                        resources,
                        "DeviceRGB",
                        page_number,
                        owner,
                    )?;
                    paint_state.fill = SelectedPaintColorSpace::Other;
                }
                Operator::SetStrokeRgb { .. } => {
                    self.inspect_device_color_space(
                        document,
                        resources,
                        "DeviceRGB",
                        page_number,
                        owner,
                    )?;
                    paint_state.stroke = SelectedPaintColorSpace::Other;
                }
                Operator::SetFillGray { .. } => {
                    self.inspect_device_color_space(
                        document,
                        resources,
                        "DeviceGray",
                        page_number,
                        owner,
                    )?;
                    paint_state.fill = SelectedPaintColorSpace::Other;
                }
                Operator::SetStrokeGray { .. } => {
                    self.inspect_device_color_space(
                        document,
                        resources,
                        "DeviceGray",
                        page_number,
                        owner,
                    )?;
                    paint_state.stroke = SelectedPaintColorSpace::Other;
                }
                Operator::SetFillCmyk { .. } => {
                    self.inspect_device_color_space(
                        document,
                        resources,
                        "DeviceCMYK",
                        page_number,
                        owner,
                    )?;
                    paint_state.fill = SelectedPaintColorSpace::Other;
                }
                Operator::SetStrokeCmyk { .. } => {
                    self.inspect_device_color_space(
                        document,
                        resources,
                        "DeviceCMYK",
                        page_number,
                        owner,
                    )?;
                    paint_state.stroke = SelectedPaintColorSpace::Other;
                }
                Operator::SetFillColor { components }
                | Operator::SetFillColorN {
                    components,
                    name: None,
                } => {
                    if matches!(
                        paint_state.fill,
                        SelectedPaintColorSpace::ColoredPattern
                            | SelectedPaintColorSpace::UncoloredPattern { .. }
                    ) {
                        self.add_finding(format!(
                            "blocking authority rule: page {page_number} {owner} fill Pattern selection requires scn with a pattern name"
                        ))?;
                    }
                    self.inspect_indexed_paint_components(
                        paint_state.fill,
                        &components,
                        page_number,
                        owner,
                        "fill",
                    )?;
                }
                Operator::SetStrokeColor { components }
                | Operator::SetStrokeColorN {
                    components,
                    name: None,
                } => {
                    if matches!(
                        paint_state.stroke,
                        SelectedPaintColorSpace::ColoredPattern
                            | SelectedPaintColorSpace::UncoloredPattern { .. }
                    ) {
                        self.add_finding(format!(
                            "blocking authority rule: page {page_number} {owner} stroke Pattern selection requires SCN with a pattern name"
                        ))?;
                    }
                    self.inspect_indexed_paint_components(
                        paint_state.stroke,
                        &components,
                        page_number,
                        owner,
                        "stroke",
                    )?;
                }
                Operator::SetExtGState { dict_name } => {
                    self.inspect_ext_g_state(walk, &dict_name, paint_state)?;
                }
                Operator::Do { name } => {
                    self.inspect_xobject(walk, &name, paint_state)?;
                }
                Operator::PaintShading { name } => {
                    self.inspect_shading(document, resources, &name, page_number, owner)?;
                }
                Operator::SetFillColorN {
                    components,
                    name: Some(name),
                } => {
                    self.inspect_pattern_paint_components(
                        paint_state.fill,
                        &components,
                        page_number,
                        owner,
                        "fill",
                    )?;
                    if !matches!(
                        paint_state.fill,
                        SelectedPaintColorSpace::ColoredPattern
                            | SelectedPaintColorSpace::UncoloredPattern { .. }
                    ) {
                        self.add_finding(format!(
                            "blocking authority rule: page {page_number} {owner} fill cannot select pattern /{} without a Pattern color space",
                            bounded_name(&name)
                        ))?;
                    }
                    self.inspect_pattern(walk, &name, paint_state.fill, stream_entry_paint_state)?;
                }
                Operator::SetStrokeColorN {
                    components,
                    name: Some(name),
                } => {
                    self.inspect_pattern_paint_components(
                        paint_state.stroke,
                        &components,
                        page_number,
                        owner,
                        "stroke",
                    )?;
                    if !matches!(
                        paint_state.stroke,
                        SelectedPaintColorSpace::ColoredPattern
                            | SelectedPaintColorSpace::UncoloredPattern { .. }
                    ) {
                        self.add_finding(format!(
                            "blocking authority rule: page {page_number} {owner} stroke cannot select pattern /{} without a Pattern color space",
                            bounded_name(&name)
                        ))?;
                    }
                    self.inspect_pattern(
                        walk,
                        &name,
                        paint_state.stroke,
                        stream_entry_paint_state,
                    )?;
                }
                Operator::InlineImage { dict, .. } => {
                    if uncolored_pattern
                        && !image_mask_has_color_neutral_structure(
                            document,
                            &dict,
                            ImageMaskDictionaryKind::Inline,
                        )?
                    {
                        self.add_finding(format!(
                            "blocking authority rule: page {page_number} {owner} uncolored tiling pattern contains an inline image without valid image mask structure"
                        ))?;
                    }
                    if let Some(color_space) = dict.get("ColorSpace").or_else(|| dict.get("CS")) {
                        self.inspect_color_space(
                            document,
                            resources,
                            color_space,
                            page_number,
                            &format!("{owner} inline image"),
                            0,
                        )?;
                    }
                }
                _ => {}
            }
        }
        if !paint_state_stack.is_empty() {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ends with {} unmatched q graphics-state save operation(s)",
                paint_state_stack.len()
            ))?;
        }
        Ok(())
    }

    fn inspect_device_color_space(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        device_space: &str,
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let default = match device_space {
            "DeviceRGB" => "DefaultRGB",
            "DeviceCMYK" => "DefaultCMYK",
            "DeviceGray" => "DefaultGray",
            _ => return Ok(()),
        };
        if let Some(color_space) = lookup_resource(document, resources, "ColorSpace", default)? {
            return self.inspect_color_space(
                document,
                resources,
                &color_space,
                page_number,
                &format!("{owner} color space /{default}"),
                1,
            );
        }
        let conflicts = matches!(
            (device_space, self.output_intent_space),
            ("DeviceRGB", Some("CMYK")) | ("DeviceCMYK", Some("RGB"))
        );
        if conflicts {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} uses {device_space} without a matching default color space under a {} output intent",
                self.output_intent_space
                    .expect("a conflicting output intent is present")
            ))?;
        }
        Ok(())
    }

    fn inspect_named_color_space(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        name: &str,
        page_number: usize,
        owner: &str,
        alias_depth: usize,
    ) -> Result<(), ValidationError> {
        let normalized = normalize_device_color_space(name);
        if normalized == "Pattern" {
            return Ok(());
        }
        if is_device_color_space(normalized) {
            return self.inspect_device_color_space(
                document,
                resources,
                normalized,
                page_number,
                owner,
            );
        }
        if alias_depth > MAX_REFERENCE_DEPTH {
            return Err(ValidationError::OutputLimitExceeded);
        }
        let Some(color_space) = lookup_resource(document, resources, "ColorSpace", normalized)?
        else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} selects unavailable color space /{}",
                bounded_name(normalized)
            ))?;
            return Ok(());
        };
        let resolved_color_space = resolve_object(document, &color_space)?;
        if let Some(alias) = resolved_color_space.as_name()
            && !is_device_color_space(normalize_device_color_space(alias))
        {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} color-space resource /{} aliases non-device color space /{} instead of containing its definition",
                bounded_name(normalized),
                bounded_name(alias)
            ))?;
            return Ok(());
        }
        self.inspect_color_space(
            document,
            resources,
            &resolved_color_space,
            page_number,
            &format!("{owner} color space /{}", bounded_name(normalized)),
            alias_depth + 1,
        )
    }

    fn inspect_color_space(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        color_space: &Object,
        page_number: usize,
        owner: &str,
        alias_depth: usize,
    ) -> Result<(), ValidationError> {
        if alias_depth > MAX_REFERENCE_DEPTH {
            return Err(ValidationError::OutputLimitExceeded);
        }
        let color_space = resolve_object(document, color_space)?;
        if let Some(name) = color_space.as_name() {
            return self.inspect_named_color_space(
                document,
                resources,
                name,
                page_number,
                owner,
                alias_depth,
            );
        }
        let Some(array) = color_space.as_array() else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} is not a readable color-space definition"
            ))?;
            return Ok(());
        };
        let Some(kind) = array.first().and_then(Object::as_name) else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} has no color-space type"
            ))?;
            return Ok(());
        };
        match kind {
            "ICCBased" => {
                let Some(profile) = array.get(1).filter(|_| array.len() == 2) else {
                    self.add_finding(format!(
                        "blocking authority rule: page {page_number} {owner} has a malformed ICCBased color-space array"
                    ))?;
                    return Ok(());
                };
                self.inspect_profile(document, profile, page_number, owner)
            }
            "Indexed" => {
                self.inspect_indexed_color_space(
                    document,
                    resources,
                    array,
                    page_number,
                    owner,
                    alias_depth,
                )
            }
            "Pattern" => {
                if array.len() != 2 {
                    self.add_finding(format!(
                        "blocking authority rule: page {page_number} {owner} has a malformed Pattern color-space array"
                    ))?;
                    return Ok(());
                }
                self.inspect_color_space(
                    document,
                    resources,
                    &array[1],
                    page_number,
                    owner,
                    alias_depth + 1,
                )?;
                if color_space_component_count(
                    document,
                    resources,
                    &array[1],
                    alias_depth + 1,
                )?
                .is_none()
                {
                    self.add_finding(format!(
                        "blocking authority rule: page {page_number} {owner} Pattern underlying color space has no provable component count or is another Pattern color space"
                    ))?;
                }
                Ok(())
            }
            "Lab" => self.inspect_lab_color_space(document, array, page_number, owner),
            "CalGray" | "CalRGB" => {
                self.inspect_cal_color_space(document, kind, array, page_number, owner)
            }
            "Separation" | "DeviceN" => self.inspect_spot_color_space(
                document,
                resources,
                kind,
                array,
                page_number,
                owner,
                alias_depth,
            ),
            _ => self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} uses unsupported or malformed active color-space type /{}",
                bounded_name(kind)
            )),
        }
    }

    #[expect(clippy::too_many_arguments)]
    fn inspect_spot_color_space(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        kind: &str,
        array: &[Object],
        page_number: usize,
        owner: &str,
        alias_depth: usize,
    ) -> Result<(), ValidationError> {
        let valid_length = match kind {
            "Separation" => array.len() == 4,
            "DeviceN" => matches!(array.len(), 4 | 5),
            _ => false,
        };
        if !valid_length {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} {kind} color space is missing its names, alternate color space, or tint transform"
            ))?;
            return Ok(());
        }
        let Some(spot_key) = spot_colorant_key(kind, &array[1]) else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} {kind} colorant names are malformed"
            ))?;
            return Ok(());
        };
        let alternate = &array[2];
        self.inspect_color_space(
            document,
            resources,
            alternate,
            page_number,
            owner,
            alias_depth,
        )?;
        let expected_inputs = if kind == "Separation" {
            1
        } else {
            array[1].as_array().map_or(0, Vec::len)
        };
        let Some(expected_outputs) =
            alternate_color_component_count(document, resources, alternate, alias_depth)?
        else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} {kind} alternate color space cannot be proved for tint-transform consistency"
            ))?;
            return Ok(());
        };
        if !tint_function_is_readable(document, &array[3], expected_inputs, expected_outputs)? {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} {kind} color space has no readable tint transform"
            ))?;
            return Ok(());
        }
        let Some(alternate_identity) =
            Self::alternate_color_identity(document, resources, alternate, alias_depth)?
        else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} {spot_key} alternate color-space identity cannot be proved consistent"
            ))?;
            return Ok(());
        };
        if spot_key == "Separation:/All" || spot_key == "Separation:/None" {
            return Ok(());
        }
        self.record_spot_alternate(spot_key, alternate_identity, page_number, owner)
    }

    fn record_spot_alternate(
        &mut self,
        spot_key: String,
        alternate_identity: String,
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        if let Some(previous) = self.spot_alternates.get(&spot_key) {
            if previous != &alternate_identity {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} {spot_key} uses conflicting alternate color spaces"
                ))?;
            }
            return Ok(());
        }
        if self.spot_alternates.len() >= MAX_DISTINCT_SPOT_COLORANTS {
            return Err(ValidationError::OutputLimitExceeded);
        }
        self.spot_alternates.insert(spot_key, alternate_identity);
        Ok(())
    }

    fn alternate_color_identity(
        document: &PdfDocument,
        resources: &Object,
        color_space: &Object,
        alias_depth: usize,
    ) -> Result<Option<String>, ValidationError> {
        if alias_depth > MAX_REFERENCE_DEPTH {
            return Err(ValidationError::OutputLimitExceeded);
        }
        let color_space = resolve_object(document, color_space)?;
        if let Some(name) = color_space.as_name() {
            let normalized = normalize_device_color_space(name);
            if is_device_color_space(normalized) {
                return Ok(Some(normalized.to_owned()));
            }
            let Some(resolved) = lookup_resource(document, resources, "ColorSpace", normalized)?
            else {
                return Ok(None);
            };
            return Self::alternate_color_identity(document, resources, &resolved, alias_depth + 1);
        }
        let Some(array) = color_space.as_array() else {
            return Ok(None);
        };
        match array.first().and_then(Object::as_name) {
            Some("Lab") => lab_alternate_identity(document, array),
            Some(kind @ ("CalGray" | "CalRGB")) => cal_alternate_identity(document, kind, array),
            Some("ICCBased") => {
                let Some(profile) = array.get(1) else {
                    return Ok(None);
                };
                let profile = resolve_object(document, profile)?;
                let Object::Stream { dict, .. } = &profile else {
                    return Ok(None);
                };
                let Some(channels) = dict.get("N").and_then(Object::as_integer) else {
                    return Ok(None);
                };
                let Ok(bytes) = profile.decode_stream_data() else {
                    return Ok(None);
                };
                let hash: [u8; 32] = Sha256::digest(&bytes).into();
                Ok(Some(format!("ICCBased:{channels}:{}", hex_hash(hash))))
            }
            _ => Ok(None),
        }
    }

    fn inspect_lab_color_space(
        &mut self,
        document: &PdfDocument,
        array: &[Object],
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let valid = if array.len() == 2 {
            let parameters = resolve_object(document, &array[1])?;
            if let Some(parameters) = parameters.as_dict() {
                lab_parameters_are_valid(document, parameters)?
            } else {
                false
            }
        } else {
            false
        };
        if !valid {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} contains a malformed Lab color space"
            ))?;
        }
        Ok(())
    }

    fn inspect_indexed_color_space(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        array: &[Object],
        page_number: usize,
        owner: &str,
        alias_depth: usize,
    ) -> Result<(), ValidationError> {
        if array.len() != 4 {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} has a malformed Indexed color-space array"
            ))?;
            return Ok(());
        }
        let base = &array[1];
        self.inspect_color_space(document, resources, base, page_number, owner, alias_depth)?;
        let hival = resolve_object(document, &array[2])?
            .as_integer()
            .filter(|value| (0..=255).contains(value));
        let Some(hival) = hival else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} Indexed color space has no integer /hival from 0 through 255"
            ))?;
            return Ok(());
        };
        let Some(components) = color_space_component_count(document, resources, base, alias_depth)?
        else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} Indexed color-space base has no provable component count"
            ))?;
            return Ok(());
        };
        let expected_bytes = usize::try_from(hival)
            .ok()
            .and_then(|hival| hival.checked_add(1))
            .and_then(|entries| entries.checked_mul(components))
            .ok_or(ValidationError::OutputLimitExceeded)?;
        let actual_bytes = self.indexed_lookup_length(document, &array[3])?;
        if actual_bytes != Some(expected_bytes) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} Indexed lookup must decode to exactly {expected_bytes} bytes; decoded length is {actual_bytes:?}"
            ))?;
        }
        Ok(())
    }

    fn indexed_lookup_length(
        &mut self,
        document: &PdfDocument,
        lookup: &Object,
    ) -> Result<Option<usize>, ValidationError> {
        let reference = match lookup {
            Object::Reference(reference) => Some(*reference),
            _ => None,
        };
        if let Some(reference) = reference
            && let Some(length) = self.indexed_lookup_lengths.get(&reference)
        {
            return Ok(*length);
        }
        let lookup = resolve_object(document, lookup)?;
        let length = match &lookup {
            Object::String(bytes) => Some(bytes.len()),
            Object::Stream { dict, data } => bounded_indexed_stream_length(dict, data),
            _ => None,
        };
        if let Some(reference) = reference {
            if self.indexed_lookup_lengths.len() >= MAX_INDEXED_LOOKUP_STREAMS {
                return Err(ValidationError::OutputLimitExceeded);
            }
            self.indexed_lookup_lengths.insert(reference, length);
        }
        Ok(length)
    }

    fn inspect_cal_color_space(
        &mut self,
        document: &PdfDocument,
        kind: &str,
        array: &[Object],
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let valid = if array.len() == 2 {
            let parameters = resolve_object(document, &array[1])?;
            if let Some(parameters) = parameters.as_dict() {
                cal_parameters_are_valid(document, kind, parameters)?
            } else {
                false
            }
        } else {
            false
        };
        if !valid {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} contains a malformed {kind} color space"
            ))?;
        }
        Ok(())
    }

    fn inspect_profile(
        &mut self,
        document: &PdfDocument,
        profile: &Object,
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let profile = resolve_object(document, profile)?;
        let Object::Stream { dict, .. } = &profile else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} does not contain an ICC profile stream"
            ))?;
            return Ok(());
        };
        let Some(channels) = dict.get("N").and_then(Object::as_integer) else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ICC profile has no integer /N channel count"
            ))?;
            return Ok(());
        };
        if !matches!(channels, 1 | 3 | 4) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ICC profile declares unsupported /N {channels}"
            ))?;
            return Ok(());
        }
        let bytes = match profile.decode_stream_data() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} ICC profile stream cannot be decoded: {error}"
                ))?;
                return Ok(());
            }
        };
        if bytes.is_empty() || bytes.len() > document_core::MAX_ICC_PROFILE_BYTES {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ICC profile byte length is invalid"
            ))?;
            return Ok(());
        }
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        if !self.seen_profiles.insert((hash, channels)) {
            return Ok(());
        }
        self.profiles_checked = self
            .profiles_checked
            .checked_add(1)
            .filter(|count| *count <= MAX_AUDITED_PROFILES)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        self.total_profile_bytes = self
            .total_profile_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_TOTAL_PROFILE_BYTES)
            .ok_or(ValidationError::OutputLimitExceeded)?;

        let Ok(parsed) = Profile::new_icc(&bytes) else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} contains an ICC profile that LittleCMS rejected"
            ))?;
            return Ok(());
        };
        let expected_space = match channels {
            1 => ColorSpaceSignature::GrayData,
            3 => ColorSpaceSignature::RgbData,
            4 => ColorSpaceSignature::CmykData,
            _ => unreachable!("channel count was bounded above"),
        };
        if parsed.color_space() != expected_space {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ICC profile color space disagrees with /N {channels}"
            ))?;
            return Ok(());
        }
        if !matches!(
            parsed.device_class(),
            ProfileClassSignature::InputClass
                | ProfileClassSignature::DisplayClass
                | ProfileClassSignature::OutputClass
                | ProfileClassSignature::ColorSpaceClass
        ) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ICC profile class cannot be used as a PDF device color space"
            ))?;
            return Ok(());
        }
        if !device_to_pcs_transform_is_available(&parsed, channels) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ICC profile cannot construct a device-to-PCS transform"
            ))?;
        }
        Ok(())
    }

    fn inspect_xobject(
        &mut self,
        walk: ContentWalkContext<'_>,
        name: &str,
        paint_state: PaintColorState,
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources,
            page_number,
            owner,
            depth,
            uncolored_pattern,
        } = walk;
        let Some(xobject) = lookup_resource(document, resources, "XObject", name)? else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} invokes unavailable XObject /{}",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let reference = match xobject {
            Object::Reference(reference) => Some(reference),
            _ => None,
        };
        if reference.is_some_and(|reference| !self.active_streams.insert(reference)) {
            return Ok(());
        }
        let result = (|| {
            let xobject = resolve_object(document, &xobject)?;
            let Object::Stream { dict, data } = &xobject else {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} XObject /{} is not a readable stream",
                    bounded_name(name)
                ))?;
                return Ok(());
            };
            match dict.get("Subtype").and_then(Object::as_name) {
                Some("Image") => self.inspect_image_xobject(walk, name, reference, dict, data)?,
                Some("Form") => {
                    let form_owner = format!("form /{}", bounded_name(name));
                    self.inspect_form_stream(
                        ContentWalkContext {
                            document,
                            resources,
                            page_number,
                            owner: &form_owner,
                            depth,
                            uncolored_pattern,
                        },
                        &xobject,
                        paint_state,
                    )?;
                }
                _ => {}
            }
            Ok(())
        })();
        if let Some(reference) = reference {
            self.active_streams.remove(&reference);
        }
        result
    }

    fn inspect_image_xobject(
        &mut self,
        walk: ContentWalkContext<'_>,
        name: &str,
        reference: Option<ObjectRef>,
        dictionary: &HashMap<String, Object>,
        data: &[u8],
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources,
            page_number,
            owner,
            uncolored_pattern,
            ..
        } = walk;
        for key in ["IM", "BPC"] {
            if dictionary.contains_key(key) {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} image /{} uses inline-only /{key} instead of its full image-XObject key",
                    bounded_name(name)
                ))?;
            }
        }
        let image_mask = image_xobject_image_mask_declaration(document, dictionary)?;
        let is_image_mask = image_mask == ImageMaskDeclaration::Enabled;
        if image_mask == ImageMaskDeclaration::Invalid {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} image /{} has a nonboolean /ImageMask entry",
                bounded_name(name)
            ))?;
        }
        let color_neutral_image_mask = is_image_mask
            && image_mask_has_color_neutral_structure(
                document,
                dictionary,
                ImageMaskDictionaryKind::XObject,
            )?;
        if is_image_mask && !color_neutral_image_mask {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} image /{} lacks required color-neutral image-mask structure",
                bounded_name(name)
            ))?;
        }
        if uncolored_pattern && !color_neutral_image_mask {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} uncolored tiling pattern contains image /{} without valid image mask structure",
                bounded_name(name)
            ))?;
        }
        if dictionary.contains_key("CS") {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} image /{} uses inline-only /CS instead of /ColorSpace",
                bounded_name(name)
            ))?;
        }
        let exact_jpx = matches!(
            indexed_image_filter(document, dictionary.get("Filter"))?,
            IndexedImageFilter::Jpx
        );
        if let Some(color_space) = dictionary.get("ColorSpace") {
            self.inspect_color_space(
                document,
                resources,
                color_space,
                page_number,
                &format!("image /{}", bounded_name(name)),
                0,
            )?;
            if !is_image_mask
                && !exact_jpx
                && color_space_resolves_to_indexed(document, resources, color_space, 0)?
            {
                self.inspect_indexed_image_samples(
                    document,
                    name,
                    reference,
                    dictionary,
                    data,
                    page_number,
                    owner,
                )?;
            }
        } else if !is_image_mask && !exact_jpx {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} image /{} has no full /ColorSpace entry",
                bounded_name(name)
            ))?;
        }
        if let Some(soft_mask) = dictionary.get("SMask")
            && soft_mask.as_name() != Some("None")
        {
            self.inspect_image_soft_mask(
                document,
                resources,
                soft_mask,
                page_number,
                &format!("image /{} soft mask", bounded_name(name)),
            )?;
        }
        Ok(())
    }

    #[expect(clippy::too_many_arguments)]
    fn inspect_indexed_image_samples(
        &mut self,
        document: &PdfDocument,
        name: &str,
        reference: Option<ObjectRef>,
        dictionary: &HashMap<String, Object>,
        data: &[u8],
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let image_owner = format!("{owner} image /{}", bounded_name(name));
        for key in ["F", "FFilter", "FDecodeParms"] {
            if dictionary.contains_key(key) {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {image_owner} uses unsupported external image data entry /{key}"
                ))?;
                return Ok(());
            }
        }
        let Some(width) = positive_usize_entry(document, dictionary.get("Width"))? else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {image_owner} Indexed image has no positive integer /Width"
            ))?;
            return Ok(());
        };
        let Some(height) = positive_usize_entry(document, dictionary.get("Height"))? else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {image_owner} Indexed image has no positive integer /Height"
            ))?;
            return Ok(());
        };
        let Some(bits_per_component) =
            positive_usize_entry(document, dictionary.get("BitsPerComponent"))?
                .filter(|value| matches!(value, 1 | 2 | 4 | 8 | 16))
        else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {image_owner} Indexed image /BitsPerComponent must be 1, 2, 4, 8, or 16"
            ))?;
            return Ok(());
        };
        if let Some(decode) = dictionary.get("Decode")
            && numeric_array(document, decode, 2)?.is_none()
        {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {image_owner} Indexed image /Decode must contain exactly two finite numbers"
            ))?;
        }
        let row_bytes = width
            .checked_mul(bits_per_component)
            .and_then(|bits| bits.checked_add(7))
            .map(|bits| bits / 8)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        let expected_bytes = row_bytes
            .checked_mul(height)
            .filter(|length| *length <= MAX_INDEXED_IMAGE_BYTES)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        match self.indexed_image_data_proof(
            document,
            reference,
            dictionary,
            data,
            expected_bytes,
        )? {
            IndexedImageDataProof::Decoded(actual_bytes) if actual_bytes != expected_bytes => {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {image_owner} Indexed image must decode to exactly {expected_bytes} sample bytes; decoded length is {actual_bytes}"
                ))?;
            }
            IndexedImageDataProof::Invalid => {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {image_owner} Indexed image sample stream is not a supported complete raw or single-Flate stream"
                ))?;
            }
            IndexedImageDataProof::Decoded(_) | IndexedImageDataProof::DeferredJpx => {}
        }
        Ok(())
    }

    fn indexed_image_data_proof(
        &mut self,
        document: &PdfDocument,
        reference: Option<ObjectRef>,
        dictionary: &HashMap<String, Object>,
        data: &[u8],
        expected_bytes: usize,
    ) -> Result<IndexedImageDataProof, ValidationError> {
        if let Some(reference) = reference
            && let Some(proof) = self.indexed_image_data.get(&reference)
        {
            return Ok(*proof);
        }
        if data.len() > MAX_INDEXED_IMAGE_BYTES {
            return Err(ValidationError::OutputLimitExceeded);
        }
        if reference.is_some() && self.indexed_image_data.len() >= MAX_INDEXED_IMAGE_STREAMS {
            return Err(ValidationError::OutputLimitExceeded);
        }
        self.indexed_image_encoded_bytes = self
            .indexed_image_encoded_bytes
            .checked_add(data.len())
            .filter(|total| *total <= MAX_INDEXED_IMAGE_BYTES)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        let remaining_decoded_bytes = MAX_INDEXED_IMAGE_BYTES
            .checked_sub(self.indexed_image_decoded_bytes)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        let attempt = bounded_indexed_image_data_proof(
            document,
            dictionary,
            data,
            expected_bytes,
            remaining_decoded_bytes,
        )?;
        self.indexed_image_decoded_bytes = self
            .indexed_image_decoded_bytes
            .checked_add(attempt.decoded_bytes)
            .filter(|total| *total <= MAX_INDEXED_IMAGE_BYTES)
            .ok_or(ValidationError::OutputLimitExceeded)?;
        if let Some(reference) = reference {
            self.indexed_image_data.insert(reference, attempt.proof);
        }
        Ok(attempt.proof)
    }

    fn inspect_ext_g_state(
        &mut self,
        walk: ContentWalkContext<'_>,
        name: &str,
        paint_state: PaintColorState,
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources,
            page_number,
            owner,
            depth,
            uncolored_pattern,
        } = walk;
        let Some(state) = lookup_resource(document, resources, "ExtGState", name)? else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} selects unavailable ExtGState /{}",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let state = resolve_object(document, &state)?;
        let Some(state) = state.as_dict() else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ExtGState /{} is not a dictionary",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        if uncolored_pattern {
            for key in ["TR", "TR2", "BG", "BG2", "UCR", "UCR2", "HT"] {
                if state.contains_key(key) {
                    self.add_finding(format!(
                        "blocking authority rule: page {page_number} {owner} uncolored tiling pattern ExtGState /{} contains forbidden color-related /{key}",
                        bounded_name(name)
                    ))?;
                }
            }
        }
        let Some(soft_mask) = state.get("SMask") else {
            return Ok(());
        };
        if soft_mask.as_name() == Some("None") {
            return Ok(());
        }
        let soft_mask = resolve_object(document, soft_mask)?;
        let Some(soft_mask) = soft_mask.as_dict() else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ExtGState /{} has a malformed soft-mask dictionary",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let Some(group) = soft_mask.get("G") else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} ExtGState /{} soft mask has no group Form",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let group_owner = format!("{owner} ExtGState /{} soft-mask group", bounded_name(name));
        self.inspect_form_object(
            ContentWalkContext {
                document,
                resources,
                page_number,
                owner: &group_owner,
                depth,
                uncolored_pattern,
            },
            group,
            paint_state,
        )
    }

    fn inspect_form_object(
        &mut self,
        walk: ContentWalkContext<'_>,
        form: &Object,
        paint_state: PaintColorState,
    ) -> Result<(), ValidationError> {
        let document = walk.document;
        let reference = form.as_reference();
        if reference.is_some_and(|reference| !self.active_streams.insert(reference)) {
            return Ok(());
        }
        let result = (|| {
            let form = resolve_object(document, form)?;
            self.inspect_form_stream(walk, &form, paint_state)
        })();
        if let Some(reference) = reference {
            self.active_streams.remove(&reference);
        }
        result
    }

    fn inspect_form_stream(
        &mut self,
        walk: ContentWalkContext<'_>,
        form: &Object,
        paint_state: PaintColorState,
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources: inherited_resources,
            page_number,
            owner,
            depth,
            uncolored_pattern,
        } = walk;
        let Object::Stream { dict, .. } = form else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} is not a readable Form stream"
            ))?;
            return Ok(());
        };
        let form_resources = dict
            .get("Resources")
            .cloned()
            .unwrap_or_else(|| inherited_resources.clone());
        if let Some(group) = dict.get("Group") {
            let group = resolve_object(document, group)?;
            if let Some(group) = group.as_dict()
                && group.get("S").and_then(Object::as_name) == Some("Transparency")
                && let Some(color_space) = group.get("CS")
            {
                self.inspect_color_space(
                    document,
                    &form_resources,
                    color_space,
                    page_number,
                    owner,
                    0,
                )?;
            }
        }
        let content = form.decode_stream_data().map_err(|error| {
            ValidationError::MalformedReport(format!(
                "page {page_number} {owner} cannot be decoded: {error}"
            ))
        })?;
        self.walk_content(
            &content,
            ContentWalkContext {
                document,
                resources: &form_resources,
                page_number,
                owner,
                depth: depth + 1,
                uncolored_pattern,
            },
            paint_state,
        )
    }

    fn inspect_image_soft_mask(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        soft_mask: &Object,
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let reference = soft_mask.as_reference();
        if reference.is_some_and(|reference| !self.active_streams.insert(reference)) {
            return Ok(());
        }
        let result = (|| {
            let soft_mask = resolve_object(document, soft_mask)?;
            let Object::Stream { dict, .. } = &soft_mask else {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} is not a readable image stream"
                ))?;
                return Ok(());
            };
            if dict.get("Subtype").and_then(Object::as_name) != Some("Image") {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} is not an image XObject"
                ))?;
                return Ok(());
            }
            if let Some(color_space) = dict.get("ColorSpace").or_else(|| dict.get("CS")) {
                self.inspect_color_space(document, resources, color_space, page_number, owner, 0)?;
            }
            Ok(())
        })();
        if let Some(reference) = reference {
            self.active_streams.remove(&reference);
        }
        result
    }

    fn inspect_shading(
        &mut self,
        document: &PdfDocument,
        resources: &Object,
        name: &str,
        page_number: usize,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let Some(shading) = lookup_resource(document, resources, "Shading", name)? else {
            return Ok(());
        };
        let shading = resolve_object(document, &shading)?;
        let dict = object_dictionary(&shading).ok_or_else(|| {
            ValidationError::MalformedReport(format!(
                "page {page_number} shading /{} is not a dictionary",
                bounded_name(name)
            ))
        })?;
        if let Some(color_space) = dict.get("ColorSpace").or_else(|| dict.get("CS")) {
            self.inspect_color_space(
                document,
                resources,
                color_space,
                page_number,
                &format!("{owner} shading /{}", bounded_name(name)),
                0,
            )?;
        }
        Ok(())
    }

    fn inspect_pattern(
        &mut self,
        walk: ContentWalkContext<'_>,
        name: &str,
        selected: SelectedPaintColorSpace,
        stream_entry_paint_state: PaintColorState,
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources,
            page_number,
            owner,
            ..
        } = walk;
        let Some(pattern) = lookup_resource(document, resources, "Pattern", name)? else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} selects unavailable Pattern /{}",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let reference = match pattern {
            Object::Reference(reference) => Some(reference),
            _ => None,
        };
        if reference.is_some_and(|reference| !self.active_streams.insert(reference)) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} recursively selects active Pattern /{}",
                bounded_name(name)
            ))?;
            return Ok(());
        }
        let result = (|| {
            let pattern = resolve_object(document, &pattern)?;
            let Some(dict) = object_dictionary(&pattern) else {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} Pattern /{} is not a readable dictionary or stream",
                    bounded_name(name)
                ))?;
                return Ok(());
            };
            if let Some(kind) = dict.get("Type")
                && resolve_object(document, kind)?.as_name() != Some("Pattern")
            {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} Pattern /{} has a non-Pattern /Type",
                    bounded_name(name)
                ))?;
            }
            let Some(pattern_type) = dict
                .get("PatternType")
                .map(|value| resolve_object(document, value))
                .transpose()?
                .as_ref()
                .and_then(Object::as_integer)
            else {
                self.add_finding(format!(
                    "blocking authority rule: page {page_number} {owner} Pattern /{} has no integer /PatternType",
                    bounded_name(name)
                ))?;
                return Ok(());
            };
            match pattern_type {
                1 => self.inspect_tiling_pattern(
                    walk,
                    name,
                    &pattern,
                    dict,
                    selected,
                    stream_entry_paint_state,
                )?,
                2 => self.inspect_shading_pattern(walk, name, &pattern, dict, selected)?,
                _ => {
                    self.add_finding(format!(
                        "blocking authority rule: page {page_number} {owner} Pattern /{} has unsupported /PatternType {pattern_type}",
                        bounded_name(name)
                    ))?;
                    return Ok(());
                }
            }
            Ok(())
        })();
        if let Some(reference) = reference {
            self.active_streams.remove(&reference);
        }
        result
    }

    fn inspect_tiling_pattern(
        &mut self,
        walk: ContentWalkContext<'_>,
        name: &str,
        pattern: &Object,
        dict: &HashMap<String, Object>,
        selected: SelectedPaintColorSpace,
        stream_entry_paint_state: PaintColorState,
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources: _,
            page_number,
            owner,
            depth,
            uncolored_pattern,
        } = walk;
        if !matches!(pattern, Object::Stream { .. }) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} tiling Pattern /{} is not a content stream",
                bounded_name(name)
            ))?;
            return Ok(());
        }
        let Some(paint_type) = dict
            .get("PaintType")
            .map(|value| resolve_object(document, value))
            .transpose()?
            .as_ref()
            .and_then(Object::as_integer)
            .filter(|value| matches!(value, 1 | 2))
        else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} tiling Pattern /{} has no supported integer /PaintType",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let selection_matches = matches!(
            (selected, paint_type),
            (SelectedPaintColorSpace::ColoredPattern, 1)
                | (SelectedPaintColorSpace::UncoloredPattern { .. }, 2)
        );
        if !selection_matches {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} Pattern /{} paint type does not match the selected Pattern color space",
                bounded_name(name)
            ))?;
        }
        let Some(pattern_resources) = self.inspect_tiling_pattern_parameters(walk, name, dict)?
        else {
            return Ok(());
        };
        let content = pattern
            .decode_stream_data()
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        let pattern_owner = format!("pattern /{}", bounded_name(name));
        self.walk_content(
            &content,
            ContentWalkContext {
                document,
                resources: &pattern_resources,
                page_number,
                owner: &pattern_owner,
                depth: depth + 1,
                uncolored_pattern: uncolored_pattern || paint_type == 2,
            },
            if paint_type == 1 {
                stream_entry_paint_state
            } else {
                PaintColorState::default()
            },
        )
    }

    fn inspect_tiling_pattern_parameters(
        &mut self,
        walk: ContentWalkContext<'_>,
        name: &str,
        dict: &HashMap<String, Object>,
    ) -> Result<Option<Object>, ValidationError> {
        let document = walk.document;
        let prefix = format!(
            "blocking authority rule: page {} {} tiling Pattern /{}",
            walk.page_number,
            walk.owner,
            bounded_name(name)
        );
        let tiling_type_is_valid = dict
            .get("TilingType")
            .map(|value| resolve_object(document, value))
            .transpose()?
            .as_ref()
            .and_then(Object::as_integer)
            .is_some_and(|value| matches!(value, 1..=3));
        if !tiling_type_is_valid {
            self.add_finding(format!("{prefix} has no supported integer /TilingType"))?;
        }
        let bbox_is_valid = match dict.get("BBox") {
            Some(value) => numeric_array(document, value, 4)?
                .is_some_and(|bbox| bbox[0] < bbox[2] && bbox[1] < bbox[3]),
            None => false,
        };
        if !bbox_is_valid {
            self.add_finding(format!("{prefix} has no valid /BBox"))?;
        }
        for key in ["XStep", "YStep"] {
            let step_is_valid = match dict.get(key) {
                Some(value) => numeric_value(document, value)?.is_some_and(|value| value != 0.0),
                None => false,
            };
            if !step_is_valid {
                self.add_finding(format!("{prefix} has no nonzero finite /{key}"))?;
            }
        }
        let Some(resources) = dict
            .get("Resources")
            .map(|value| resolve_object(document, value))
            .transpose()?
            .filter(|value| value.as_dict().is_some())
        else {
            self.add_finding(format!("{prefix} has no readable /Resources dictionary"))?;
            return Ok(None);
        };
        Ok(Some(resources))
    }

    fn inspect_shading_pattern(
        &mut self,
        walk: ContentWalkContext<'_>,
        name: &str,
        pattern: &Object,
        dict: &HashMap<String, Object>,
        selected: SelectedPaintColorSpace,
    ) -> Result<(), ValidationError> {
        let ContentWalkContext {
            document,
            resources,
            page_number,
            owner,
            ..
        } = walk;
        if matches!(pattern, Object::Stream { .. }) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} shading Pattern /{} must be a dictionary, not a stream",
                bounded_name(name)
            ))?;
        }
        if !matches!(selected, SelectedPaintColorSpace::ColoredPattern) {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} shading Pattern /{} cannot use an underlying Pattern color space",
                bounded_name(name)
            ))?;
        }
        let Some(shading) = dict.get("Shading") else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} shading Pattern /{} has no shading dictionary",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let shading = resolve_object(document, shading)?;
        let Some(shading_dict) = object_dictionary(&shading) else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} shading Pattern /{} has an unreadable shading dictionary",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        let Some(color_space) = shading_dict.get("ColorSpace") else {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} shading Pattern /{} has no color space",
                bounded_name(name)
            ))?;
            return Ok(());
        };
        if color_space_resolves_to_pattern(document, resources, color_space, 0)? {
            self.add_finding(format!(
                "blocking authority rule: page {page_number} {owner} shading Pattern /{} cannot use a Pattern color space",
                bounded_name(name)
            ))?;
            return Ok(());
        }
        self.inspect_color_space(
            document,
            resources,
            color_space,
            page_number,
            &format!("{owner} pattern /{}", bounded_name(name)),
            0,
        )
    }

    fn add_finding(&mut self, finding: String) -> Result<(), ValidationError> {
        if self.findings.len() >= MAX_FINDINGS {
            return Err(ValidationError::OutputLimitExceeded);
        }
        self.findings.push(finding);
        Ok(())
    }
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
    let Some(dictionary) = resources.as_dict() else {
        return Ok(None);
    };
    let Some(category) = dictionary.get(category) else {
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

fn object_dictionary(object: &Object) -> Option<&HashMap<String, Object>> {
    match object {
        Object::Dictionary(dictionary)
        | Object::Stream {
            dict: dictionary, ..
        } => Some(dictionary),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum ImageMaskDictionaryKind {
    XObject,
    Inline,
}

fn image_mask_has_color_neutral_structure(
    document: &PdfDocument,
    dictionary: &HashMap<String, Object>,
    kind: ImageMaskDictionaryKind,
) -> Result<bool, ValidationError> {
    let (image_mask, bits) = match kind {
        ImageMaskDictionaryKind::XObject => {
            if dictionary.contains_key("IM") || dictionary.contains_key("BPC") {
                return Ok(false);
            }
            (
                dictionary.get("ImageMask"),
                dictionary.get("BitsPerComponent"),
            )
        }
        ImageMaskDictionaryKind::Inline => (
            dictionary.get("ImageMask").or_else(|| dictionary.get("IM")),
            dictionary
                .get("BitsPerComponent")
                .or_else(|| dictionary.get("BPC")),
        ),
    };
    let Some(image_mask) = image_mask else {
        return Ok(false);
    };
    let image_mask = if matches!(kind, ImageMaskDictionaryKind::XObject) {
        resolve_object(document, image_mask)?
    } else {
        image_mask.clone()
    };
    if image_mask.as_bool() != Some(true) {
        return Ok(false);
    }
    if let Some(bits) = bits {
        let bits = if matches!(kind, ImageMaskDictionaryKind::XObject) {
            resolve_object(document, bits)?
        } else {
            bits.clone()
        };
        if bits.as_integer() != Some(1) {
            return Ok(false);
        }
    }
    let (width, height, decode) = match kind {
        ImageMaskDictionaryKind::XObject => (
            dictionary.get("Width"),
            dictionary.get("Height"),
            dictionary.get("Decode"),
        ),
        ImageMaskDictionaryKind::Inline => (
            dictionary.get("Width").or_else(|| dictionary.get("W")),
            dictionary.get("Height").or_else(|| dictionary.get("H")),
            dictionary.get("Decode").or_else(|| dictionary.get("D")),
        ),
    };
    let dimensions_are_valid = match kind {
        ImageMaskDictionaryKind::XObject => {
            positive_usize_entry(document, width)?.is_some()
                && positive_usize_entry(document, height)?.is_some()
        }
        ImageMaskDictionaryKind::Inline => [width, height].iter().all(|entry| {
            entry
                .and_then(Object::as_integer)
                .is_some_and(|value| value > 0)
        }),
    };
    if !dimensions_are_valid {
        return Ok(false);
    }
    if let Some(decode) = decode {
        let Some(values) = numeric_array(document, decode, 2)? else {
            return Ok(false);
        };
        if !matches!(values.as_slice(), [0.0, 1.0] | [1.0, 0.0]) {
            return Ok(false);
        }
    }
    Ok(!["ColorSpace", "CS", "Mask", "SMask"]
        .iter()
        .any(|key| dictionary.contains_key(*key)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageMaskDeclaration {
    Absent,
    Disabled,
    Enabled,
    Invalid,
}

fn image_xobject_image_mask_declaration(
    document: &PdfDocument,
    dictionary: &HashMap<String, Object>,
) -> Result<ImageMaskDeclaration, ValidationError> {
    let Some(image_mask) = dictionary.get("ImageMask") else {
        return Ok(ImageMaskDeclaration::Absent);
    };
    Ok(match resolve_object(document, image_mask)?.as_bool() {
        Some(true) => ImageMaskDeclaration::Enabled,
        Some(false) => ImageMaskDeclaration::Disabled,
        None => ImageMaskDeclaration::Invalid,
    })
}

fn output_intent_color_space(
    document: &PdfDocument,
) -> Result<Option<&'static str>, ValidationError> {
    let Some(trailer) = document.trailer().as_dict() else {
        return Ok(None);
    };
    let Some(root) = trailer.get("Root") else {
        return Ok(None);
    };
    let root = resolve_object(document, root)?;
    let Some(catalog) = root.as_dict() else {
        return Ok(None);
    };
    let Some(output_intents) = catalog.get("OutputIntents") else {
        return Ok(None);
    };
    let output_intents = resolve_object(document, output_intents)?;
    let Some(output_intents) = output_intents.as_array() else {
        return Ok(None);
    };
    for output_intent in output_intents {
        let output_intent = resolve_object(document, output_intent)?;
        let Some(output_intent) = output_intent.as_dict() else {
            continue;
        };
        if output_intent.get("S").and_then(Object::as_name) != Some("GTS_PDFX") {
            continue;
        }
        let Some(profile) = output_intent.get("DestOutputProfile") else {
            continue;
        };
        let profile = resolve_object(document, profile)?;
        let Ok(bytes) = profile.decode_stream_data() else {
            continue;
        };
        let Ok(profile) = Profile::new_icc(&bytes) else {
            continue;
        };
        return Ok(match profile.color_space() {
            ColorSpaceSignature::RgbData => Some("RGB"),
            ColorSpaceSignature::CmykData => Some("CMYK"),
            ColorSpaceSignature::GrayData => Some("Gray"),
            _ => None,
        });
    }
    Ok(None)
}

fn spot_colorant_key(kind: &str, names: &Object) -> Option<String> {
    const MAX_COLORANTS: usize = 32;
    match kind {
        "Separation" => names
            .as_name()
            .filter(|name| name.len() <= MAX_REPORTED_NAME_CHARS)
            .map(|name| format!("Separation:/{name}")),
        "DeviceN" => {
            let names = names.as_array()?;
            if names.is_empty() || names.len() > MAX_COLORANTS {
                return None;
            }
            let mut key = String::from("DeviceN:");
            for (index, name) in names.iter().enumerate() {
                let name = name.as_name()?;
                if name.len() > MAX_REPORTED_NAME_CHARS {
                    return None;
                }
                if index > 0 {
                    key.push(',');
                }
                key.push('/');
                key.push_str(name);
            }
            Some(key)
        }
        _ => None,
    }
}

fn alternate_color_component_count(
    document: &PdfDocument,
    resources: &Object,
    color_space: &Object,
    alias_depth: usize,
) -> Result<Option<usize>, ValidationError> {
    if alias_depth > MAX_REFERENCE_DEPTH {
        return Err(ValidationError::OutputLimitExceeded);
    }
    let color_space = resolve_object(document, color_space)?;
    if let Some(name) = color_space.as_name() {
        let normalized = normalize_device_color_space(name);
        if normalized == "Pattern" {
            return Ok(None);
        }
        let components = match normalized {
            "DeviceGray" => Some(1),
            "DeviceRGB" => Some(3),
            "DeviceCMYK" => Some(4),
            _ => None,
        };
        if components.is_some() {
            return Ok(components);
        }
        let Some(resolved) = lookup_resource(document, resources, "ColorSpace", normalized)? else {
            return Ok(None);
        };
        return alternate_color_component_count(document, resources, &resolved, alias_depth + 1);
    }
    let Some(array) = color_space.as_array() else {
        return Ok(None);
    };
    match array.first().and_then(Object::as_name) {
        Some("Lab" | "CalRGB") if array.len() == 2 => Ok(Some(3)),
        Some("CalGray") if array.len() == 2 => Ok(Some(1)),
        Some("ICCBased") if array.len() == 2 => {
            let profile = resolve_object(document, &array[1])?;
            let Object::Stream { dict, .. } = profile else {
                return Ok(None);
            };
            Ok(dict
                .get("N")
                .and_then(Object::as_integer)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| matches!(value, 1 | 3 | 4)))
        }
        _ => Ok(None),
    }
}

fn color_space_component_count(
    document: &PdfDocument,
    resources: &Object,
    color_space: &Object,
    alias_depth: usize,
) -> Result<Option<usize>, ValidationError> {
    if alias_depth > MAX_REFERENCE_DEPTH {
        return Err(ValidationError::OutputLimitExceeded);
    }
    let color_space = resolve_object(document, color_space)?;
    if let Some(name) = color_space.as_name() {
        let normalized = normalize_device_color_space(name);
        if normalized == "Pattern" {
            return Ok(None);
        }
        let components = match normalized {
            "DeviceGray" => Some(1),
            "DeviceRGB" => Some(3),
            "DeviceCMYK" => Some(4),
            _ => None,
        };
        if components.is_some() {
            return Ok(components);
        }
        let Some(resolved) = lookup_resource(document, resources, "ColorSpace", normalized)? else {
            return Ok(None);
        };
        return color_space_component_count(document, resources, &resolved, alias_depth + 1);
    }
    let Some(array) = color_space.as_array() else {
        return Ok(None);
    };
    match array.first().and_then(Object::as_name) {
        Some("Lab" | "CalRGB") if array.len() == 2 => Ok(Some(3)),
        Some("CalGray") if array.len() == 2 => Ok(Some(1)),
        Some("ICCBased") if array.len() == 2 => {
            let profile = resolve_object(document, &array[1])?;
            let Object::Stream { dict, .. } = profile else {
                return Ok(None);
            };
            Ok(dict
                .get("N")
                .and_then(Object::as_integer)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| matches!(value, 1 | 3 | 4)))
        }
        Some("Indexed") if array.len() == 4 => Ok(Some(1)),
        Some("Separation") if array.len() == 4 && array[1].as_name().is_some() => Ok(Some(1)),
        Some("DeviceN") if matches!(array.len(), 4 | 5) => Ok(array[1]
            .as_array()
            .filter(|names| !names.is_empty() && names.len() <= 32)
            .filter(|names| names.iter().all(|name| name.as_name().is_some()))
            .map(Vec::len)),
        _ => Ok(None),
    }
}

fn color_space_resolves_to_pattern(
    document: &PdfDocument,
    resources: &Object,
    color_space: &Object,
    alias_depth: usize,
) -> Result<bool, ValidationError> {
    if alias_depth > MAX_REFERENCE_DEPTH {
        return Err(ValidationError::OutputLimitExceeded);
    }
    let color_space = resolve_object(document, color_space)?;
    if let Some(name) = color_space.as_name() {
        let normalized = normalize_device_color_space(name);
        if normalized == "Pattern" {
            return Ok(true);
        }
        if is_device_color_space(normalized) {
            return Ok(false);
        }
        let Some(aliased) = lookup_resource(document, resources, "ColorSpace", normalized)? else {
            return Ok(false);
        };
        return color_space_resolves_to_pattern(document, resources, &aliased, alias_depth + 1);
    }
    Ok(color_space
        .as_array()
        .and_then(|array| array.first())
        .and_then(Object::as_name)
        == Some("Pattern"))
}

fn color_space_resolves_to_indexed(
    document: &PdfDocument,
    resources: &Object,
    color_space: &Object,
    alias_depth: usize,
) -> Result<bool, ValidationError> {
    if alias_depth > MAX_REFERENCE_DEPTH {
        return Err(ValidationError::OutputLimitExceeded);
    }
    let color_space = resolve_object(document, color_space)?;
    if let Some(name) = color_space.as_name() {
        if is_device_color_space(normalize_device_color_space(name)) || name == "Pattern" {
            return Ok(false);
        }
        let Some(aliased) = lookup_resource(document, resources, "ColorSpace", name)? else {
            return Ok(false);
        };
        return color_space_resolves_to_indexed(document, resources, &aliased, alias_depth + 1);
    }
    Ok(color_space
        .as_array()
        .and_then(|array| array.first())
        .and_then(Object::as_name)
        == Some("Indexed"))
}

fn positive_usize_entry(
    document: &PdfDocument,
    entry: Option<&Object>,
) -> Result<Option<usize>, ValidationError> {
    let Some(entry) = entry else {
        return Ok(None);
    };
    Ok(resolve_object(document, entry)?
        .as_integer()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0))
}

#[derive(Clone, Copy)]
enum IndexedImageFilter {
    Raw,
    Flate { array_form: bool },
    Jpx,
    Unsupported,
}

fn indexed_image_filter(
    document: &PdfDocument,
    filter: Option<&Object>,
) -> Result<IndexedImageFilter, ValidationError> {
    let Some(filter) = filter else {
        return Ok(IndexedImageFilter::Raw);
    };
    let filter = resolve_object(document, filter)?;
    if let Some(name) = filter.as_name() {
        return Ok(match name {
            "FlateDecode" => IndexedImageFilter::Flate { array_form: false },
            "JPXDecode" => IndexedImageFilter::Jpx,
            _ => IndexedImageFilter::Unsupported,
        });
    }
    let Some(filters) = filter.as_array() else {
        return Ok(IndexedImageFilter::Unsupported);
    };
    if filters.is_empty() {
        return Ok(IndexedImageFilter::Raw);
    }
    let mut names = Vec::with_capacity(filters.len());
    for filter in filters {
        let filter = resolve_object(document, filter)?;
        let Some(name) = filter.as_name() else {
            return Ok(IndexedImageFilter::Unsupported);
        };
        names.push(name.to_owned());
    }
    if names.as_slice() == ["JPXDecode"] {
        return Ok(IndexedImageFilter::Jpx);
    }
    Ok(if names.as_slice() == ["FlateDecode"] {
        IndexedImageFilter::Flate { array_form: true }
    } else {
        IndexedImageFilter::Unsupported
    })
}

fn bounded_indexed_image_data_proof(
    document: &PdfDocument,
    dictionary: &HashMap<String, Object>,
    data: &[u8],
    expected_bytes: usize,
    remaining_decoded_bytes: usize,
) -> Result<IndexedImageDataAttempt, ValidationError> {
    let filter = indexed_image_filter(document, dictionary.get("Filter"))?;
    let parameters = dictionary.get("DecodeParms");
    match filter {
        IndexedImageFilter::Jpx => Ok(IndexedImageDataAttempt {
            proof: IndexedImageDataProof::DeferredJpx,
            decoded_bytes: 0,
        }),
        IndexedImageFilter::Unsupported => Ok(IndexedImageDataAttempt {
            proof: IndexedImageDataProof::Invalid,
            decoded_bytes: 0,
        }),
        IndexedImageFilter::Raw => {
            if !raw_decode_parameters_are_empty(document, parameters)? {
                return Ok(IndexedImageDataAttempt {
                    proof: IndexedImageDataProof::Invalid,
                    decoded_bytes: 0,
                });
            }
            Ok(IndexedImageDataAttempt {
                proof: IndexedImageDataProof::Decoded(data.len()),
                decoded_bytes: data.len(),
            })
        }
        IndexedImageFilter::Flate { array_form } => {
            if !indexed_image_flate_parameters_are_noop(document, parameters, array_form)? {
                return Ok(IndexedImageDataAttempt {
                    proof: IndexedImageDataProof::Invalid,
                    decoded_bytes: 0,
                });
            }
            let read_limit = expected_bytes
                .checked_add(1)
                .and_then(|limit| {
                    remaining_decoded_bytes
                        .checked_add(1)
                        .map(|remaining| limit.min(remaining))
                })
                .ok_or(ValidationError::OutputLimitExceeded)?;
            let mut inflater = flate2::Decompress::new(true);
            let mut output = vec![0; read_limit];
            let inflate_result =
                inflater.decompress(data, &mut output, flate2::FlushDecompress::Finish);
            let inflated_length = usize::try_from(inflater.total_out())
                .map_err(|_| ValidationError::OutputLimitExceeded)?;
            let complete_member = matches!(inflate_result, Ok(flate2::Status::StreamEnd))
                && usize::try_from(inflater.total_in()).ok() == Some(data.len());
            let reached_output_limit = inflated_length == read_limit;
            Ok(IndexedImageDataAttempt {
                proof: if complete_member || reached_output_limit {
                    IndexedImageDataProof::Decoded(inflated_length)
                } else {
                    IndexedImageDataProof::Invalid
                },
                decoded_bytes: inflated_length,
            })
        }
    }
}

fn raw_decode_parameters_are_empty(
    document: &PdfDocument,
    parameters: Option<&Object>,
) -> Result<bool, ValidationError> {
    let Some(parameters) = parameters else {
        return Ok(true);
    };
    let parameters = resolve_object(document, parameters)?;
    Ok(parameters.is_null() || parameters.as_array().is_some_and(Vec::is_empty))
}

fn indexed_image_flate_parameters_are_noop(
    document: &PdfDocument,
    parameters: Option<&Object>,
    array_form: bool,
) -> Result<bool, ValidationError> {
    let Some(parameters) = parameters else {
        return Ok(true);
    };
    let parameters = resolve_object(document, parameters)?;
    if array_form && let Some(values) = parameters.as_array() {
        return match values.as_slice() {
            [value] => resolved_flate_parameter_is_noop(document, value),
            _ => Ok(false),
        };
    }
    resolved_flate_parameter_is_noop(document, &parameters)
}

fn resolved_flate_parameter_is_noop(
    document: &PdfDocument,
    parameters: &Object,
) -> Result<bool, ValidationError> {
    let parameters = resolve_object(document, parameters)?;
    if parameters.is_null() {
        return Ok(true);
    }
    let Some(dictionary) = parameters.as_dict() else {
        return Ok(false);
    };
    let Some(predictor) = dictionary.get("Predictor") else {
        return Ok(true);
    };
    Ok(resolve_object(document, predictor)?.as_integer() == Some(1))
}

fn bounded_indexed_stream_length(
    dictionary: &HashMap<String, Object>,
    data: &[u8],
) -> Option<usize> {
    if data.len() > MAX_INDEXED_LOOKUP_ENCODED_BYTES {
        return None;
    }
    match dictionary.get("Filter") {
        None => Some(data.len()),
        Some(filter) if is_single_flate_filter(filter) => {
            if !flate_decode_parameters_are_noop(filter, dictionary.get("DecodeParms")) {
                return None;
            }
            let decoder = flate2::read::ZlibDecoder::new(data);
            let mut bounded = decoder.take((MAX_INDEXED_LOOKUP_BYTES + 1) as u64);
            let length = std::io::copy(&mut bounded, &mut std::io::sink()).ok()?;
            usize::try_from(length).ok()
        }
        _ => None,
    }
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

fn tint_function_is_readable(
    document: &PdfDocument,
    function: &Object,
    expected_inputs: usize,
    expected_outputs: usize,
) -> Result<bool, ValidationError> {
    tint_function_is_readable_at_depth(document, function, expected_inputs, expected_outputs, 0)
}

fn tint_function_is_readable_at_depth(
    document: &PdfDocument,
    function: &Object,
    expected_inputs: usize,
    expected_outputs: usize,
    depth: usize,
) -> Result<bool, ValidationError> {
    if expected_inputs == 0 || expected_outputs == 0 || depth > MAX_REFERENCE_DEPTH {
        return Ok(false);
    }
    let function = resolve_object(document, function)?;
    let Some(dictionary) = object_dictionary(&function) else {
        return Ok(false);
    };
    let Some(function_type) = dictionary.get("FunctionType").and_then(Object::as_integer) else {
        return Ok(false);
    };
    let Some(domain) = dictionary.get("Domain") else {
        return Ok(false);
    };
    let Some(domain) = bounded_numeric_sequence(document, domain)? else {
        return Ok(false);
    };
    if domain.len() != expected_inputs.saturating_mul(2)
        || !domain
            .as_chunks::<2>()
            .0
            .iter()
            .all(|limits| limits[0] <= limits[1])
    {
        return Ok(false);
    }
    match function_type {
        0 => sampled_tint_function_is_readable(
            document,
            &function,
            dictionary,
            expected_inputs,
            expected_outputs,
        ),
        2 => exponential_tint_function_is_readable(
            document,
            dictionary,
            &domain,
            expected_inputs,
            expected_outputs,
        ),
        3 => stitching_tint_function_is_readable(
            document,
            dictionary,
            &domain,
            expected_inputs,
            expected_outputs,
            depth,
        ),
        4 => {
            postscript_tint_function_is_readable(document, &function, dictionary, expected_outputs)
        }
        _ => Ok(false),
    }
}

fn sampled_tint_function_is_readable(
    document: &PdfDocument,
    function: &Object,
    dictionary: &HashMap<String, Object>,
    expected_inputs: usize,
    expected_outputs: usize,
) -> Result<bool, ValidationError> {
    let Object::Stream { .. } = function else {
        return Ok(false);
    };
    let Some(size) = dictionary.get("Size") else {
        return Ok(false);
    };
    let size = resolve_object(document, size)?;
    let valid_size = size.as_array().is_some_and(|values| {
        values.len() == expected_inputs
            && values
                .iter()
                .all(|value| value.as_integer().is_some_and(|value| value > 0))
    });
    let valid_bits = dictionary
        .get("BitsPerSample")
        .and_then(Object::as_integer)
        .is_some_and(|bits| matches!(bits, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32));
    Ok(valid_size
        && valid_bits
        && numeric_range_has_outputs(document, dictionary.get("Range"), expected_outputs)?)
}

fn exponential_tint_function_is_readable(
    document: &PdfDocument,
    dictionary: &HashMap<String, Object>,
    domain: &[f64],
    expected_inputs: usize,
    expected_outputs: usize,
) -> Result<bool, ValidationError> {
    let exponent = match dictionary.get("N") {
        Some(value) => numeric_value(document, value)?,
        None => None,
    };
    let c0 = optional_numeric_sequence(document, dictionary.get("C0"))?;
    let c1 = optional_numeric_sequence(document, dictionary.get("C1"))?;
    let vectors_match_output = |values: &Option<Vec<f64>>| {
        values.as_ref().map_or(expected_outputs == 1, |values| {
            values.len() == expected_outputs
        })
    };
    let valid_range = match dictionary.get("Range") {
        Some(range) => numeric_range_has_outputs(document, Some(range), expected_outputs)?,
        None => true,
    };
    Ok(expected_inputs == 1
        && exponent.is_some_and(|n| exponential_domain_is_valid(domain, n))
        && vectors_match_output(&c0)
        && vectors_match_output(&c1)
        && valid_range)
}

fn exponential_domain_is_valid(domain: &[f64], exponent: f64) -> bool {
    if domain.len() != 2 {
        return false;
    }
    let excludes_zero = domain[0] > 0.0 || domain[1] < 0.0;
    let accepts_negative_inputs = exponent.fract() == 0.0 || domain[0] >= 0.0;
    (exponent >= 0.0 || excludes_zero) && accepts_negative_inputs
}

fn stitching_tint_function_is_readable(
    document: &PdfDocument,
    dictionary: &HashMap<String, Object>,
    domain: &[f64],
    expected_inputs: usize,
    expected_outputs: usize,
    depth: usize,
) -> Result<bool, ValidationError> {
    let Some(functions) = dictionary.get("Functions") else {
        return Ok(false);
    };
    let functions = resolve_object(document, functions)?;
    let Some(functions) = functions
        .as_array()
        .filter(|functions| !functions.is_empty() && functions.len() <= 32)
    else {
        return Ok(false);
    };
    for function in functions {
        if !tint_function_is_readable_at_depth(
            document,
            function,
            1,
            expected_outputs,
            depth.saturating_add(1),
        )? {
            return Ok(false);
        }
    }
    let valid_bounds = match dictionary.get("Bounds") {
        Some(bounds) => bounded_numeric_sequence(document, bounds)?.is_some_and(|bounds| {
            bounds.len() == functions.len().saturating_sub(1)
                && bounds.windows(2).all(|pair| pair[0] <= pair[1])
                && bounds
                    .iter()
                    .all(|bound| *bound >= domain[0] && *bound <= domain[1])
        }),
        None => false,
    };
    let valid_encode = numeric_sequence_has_length(
        document,
        dictionary.get("Encode"),
        functions.len().saturating_mul(2),
    )?;
    let valid_range = match dictionary.get("Range") {
        Some(range) => numeric_range_has_outputs(document, Some(range), expected_outputs)?,
        None => true,
    };
    Ok(expected_inputs == 1 && valid_bounds && valid_encode && valid_range)
}

fn postscript_tint_function_is_readable(
    document: &PdfDocument,
    function: &Object,
    dictionary: &HashMap<String, Object>,
    expected_outputs: usize,
) -> Result<bool, ValidationError> {
    let Object::Stream { .. } = function else {
        return Ok(false);
    };
    numeric_range_has_outputs(document, dictionary.get("Range"), expected_outputs)
}

fn optional_numeric_sequence(
    document: &PdfDocument,
    value: Option<&Object>,
) -> Result<Option<Vec<f64>>, ValidationError> {
    match value {
        Some(value) => bounded_numeric_sequence(document, value),
        None => Ok(None),
    }
}

fn numeric_sequence_has_length(
    document: &PdfDocument,
    value: Option<&Object>,
    expected_length: usize,
) -> Result<bool, ValidationError> {
    let Some(value) = value else {
        return Ok(false);
    };
    Ok(bounded_numeric_sequence(document, value)?
        .is_some_and(|values| values.len() == expected_length))
}

fn numeric_range_has_outputs(
    document: &PdfDocument,
    value: Option<&Object>,
    expected_outputs: usize,
) -> Result<bool, ValidationError> {
    let Some(value) = value else {
        return Ok(false);
    };
    Ok(
        bounded_numeric_sequence(document, value)?.is_some_and(|range| {
            range.len() == expected_outputs.saturating_mul(2)
                && range
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|limits| limits[0] <= limits[1])
        }),
    )
}

fn bounded_numeric_sequence(
    document: &PdfDocument,
    object: &Object,
) -> Result<Option<Vec<f64>>, ValidationError> {
    const MAX_FUNCTION_NUMBERS: usize = 128;
    let object = resolve_object(document, object)?;
    let Some(values) = object
        .as_array()
        .filter(|values| values.len() <= MAX_FUNCTION_NUMBERS)
    else {
        return Ok(None);
    };
    let mut numbers = Vec::with_capacity(values.len());
    for value in values {
        let Some(number) = numeric_value(document, value)? else {
            return Ok(None);
        };
        numbers.push(number);
    }
    Ok(Some(numbers))
}

fn lab_parameters_are_valid(
    document: &PdfDocument,
    parameters: &HashMap<String, Object>,
) -> Result<bool, ValidationError> {
    let Some(white_point) = parameters.get("WhitePoint") else {
        return Ok(false);
    };
    let Some(white_point) = numeric_array(document, white_point, 3)? else {
        return Ok(false);
    };
    if white_point[0] <= 0.0 || (white_point[1] - 1.0).abs() > f64::EPSILON || white_point[2] <= 0.0
    {
        return Ok(false);
    }
    if let Some(black_point) = parameters.get("BlackPoint") {
        let Some(black_point) = numeric_array(document, black_point, 3)? else {
            return Ok(false);
        };
        if black_point.iter().any(|value| *value < 0.0) {
            return Ok(false);
        }
    }
    if let Some(range) = parameters.get("Range") {
        let Some(range) = numeric_array(document, range, 4)? else {
            return Ok(false);
        };
        if range[0] > range[1] || range[2] > range[3] {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cal_parameters_are_valid(
    document: &PdfDocument,
    kind: &str,
    parameters: &HashMap<String, Object>,
) -> Result<bool, ValidationError> {
    let Some(white_point) = parameters.get("WhitePoint") else {
        return Ok(false);
    };
    let Some(white_point) = numeric_array(document, white_point, 3)? else {
        return Ok(false);
    };
    if white_point[0] <= 0.0 || (white_point[1] - 1.0).abs() > f64::EPSILON || white_point[2] <= 0.0
    {
        return Ok(false);
    }
    if let Some(black_point) = parameters.get("BlackPoint") {
        let Some(black_point) = numeric_array(document, black_point, 3)? else {
            return Ok(false);
        };
        if black_point.iter().any(|value| *value < 0.0) {
            return Ok(false);
        }
    }
    match kind {
        "CalGray" => {
            if let Some(gamma) = parameters.get("Gamma")
                && numeric_value(document, gamma)?.is_none_or(|value| value <= 0.0)
            {
                return Ok(false);
            }
        }
        "CalRGB" => {
            if let Some(gamma) = parameters.get("Gamma") {
                let Some(gamma) = numeric_array(document, gamma, 3)? else {
                    return Ok(false);
                };
                if gamma.iter().any(|value| *value <= 0.0) {
                    return Ok(false);
                }
            }
            if let Some(matrix) = parameters.get("Matrix")
                && numeric_array(document, matrix, 9)?.is_none()
            {
                return Ok(false);
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn lab_alternate_identity(
    document: &PdfDocument,
    color_space: &[Object],
) -> Result<Option<String>, ValidationError> {
    if color_space.len() != 2 {
        return Ok(None);
    }
    let parameters = resolve_object(document, &color_space[1])?;
    let Some(parameters) = parameters.as_dict() else {
        return Ok(None);
    };
    if !lab_parameters_are_valid(document, parameters)? {
        return Ok(None);
    }
    let white_point = numeric_array(
        document,
        parameters
            .get("WhitePoint")
            .expect("validated Lab parameters have WhitePoint"),
        3,
    )?
    .expect("validated Lab WhitePoint is numeric");
    let black_point = if let Some(black_point) = parameters.get("BlackPoint") {
        numeric_array(document, black_point, 3)?.expect("validated Lab BlackPoint is numeric")
    } else {
        vec![0.0, 0.0, 0.0]
    };
    let range = if let Some(range) = parameters.get("Range") {
        numeric_array(document, range, 4)?.expect("validated Lab Range is numeric")
    } else {
        vec![-100.0, 100.0, -100.0, 100.0]
    };
    let mut identity = String::from("Lab");
    for value in white_point.into_iter().chain(black_point).chain(range) {
        use std::fmt::Write as _;
        write!(&mut identity, ":{:016x}", canonical_number_bits(value))
            .expect("string writes cannot fail");
    }
    Ok(Some(identity))
}

fn cal_alternate_identity(
    document: &PdfDocument,
    kind: &str,
    color_space: &[Object],
) -> Result<Option<String>, ValidationError> {
    if color_space.len() != 2 {
        return Ok(None);
    }
    let parameters = resolve_object(document, &color_space[1])?;
    let Some(parameters) = parameters.as_dict() else {
        return Ok(None);
    };
    if !cal_parameters_are_valid(document, kind, parameters)? {
        return Ok(None);
    }
    let white_point = numeric_array(
        document,
        parameters
            .get("WhitePoint")
            .expect("validated calibrated color parameters have WhitePoint"),
        3,
    )?
    .expect("validated calibrated WhitePoint is numeric");
    let black_point = if let Some(black_point) = parameters.get("BlackPoint") {
        numeric_array(document, black_point, 3)?.expect("validated BlackPoint is numeric")
    } else {
        vec![0.0, 0.0, 0.0]
    };
    let gamma = match kind {
        "CalGray" => vec![match parameters.get("Gamma") {
            Some(gamma) => numeric_value(document, gamma)?.expect("validated Gamma is numeric"),
            None => 1.0,
        }],
        "CalRGB" => match parameters.get("Gamma") {
            Some(gamma) => numeric_array(document, gamma, 3)?.expect("validated Gamma is numeric"),
            None => vec![1.0, 1.0, 1.0],
        },
        _ => return Ok(None),
    };
    let matrix = if kind == "CalRGB" {
        match parameters.get("Matrix") {
            Some(matrix) => {
                numeric_array(document, matrix, 9)?.expect("validated Matrix is numeric")
            }
            None => vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    } else {
        Vec::new()
    };
    let mut identity = kind.to_owned();
    for value in white_point
        .into_iter()
        .chain(black_point)
        .chain(gamma)
        .chain(matrix)
    {
        use std::fmt::Write as _;
        write!(&mut identity, ":{:016x}", canonical_number_bits(value))
            .expect("string writes cannot fail");
    }
    Ok(Some(identity))
}

fn canonical_number_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0_f64.to_bits()
    } else {
        value.to_bits()
    }
}

fn numeric_array(
    document: &PdfDocument,
    object: &Object,
    expected_len: usize,
) -> Result<Option<Vec<f64>>, ValidationError> {
    Ok(bounded_numeric_sequence(document, object)?.filter(|numbers| numbers.len() == expected_len))
}

fn numeric_value(document: &PdfDocument, object: &Object) -> Result<Option<f64>, ValidationError> {
    let object = resolve_object(document, object)?;
    Ok(match object {
        Object::Integer(value) => i32::try_from(value).ok().map(f64::from),
        Object::Real(value) if value.is_finite() => Some(value),
        _ => None,
    })
}

fn hex_hash(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(64);
    for byte in hash {
        write!(&mut value, "{byte:02x}").expect("string writes cannot fail");
    }
    value
}

fn normalize_device_color_space(name: &str) -> &str {
    match name {
        "G" => "DeviceGray",
        "RGB" => "DeviceRGB",
        "CMYK" => "DeviceCMYK",
        _ => name,
    }
}

fn is_device_color_space(name: &str) -> bool {
    matches!(name, "DeviceGray" | "DeviceRGB" | "DeviceCMYK")
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

fn device_to_pcs_transform_is_available(profile: &Profile, channels: i64) -> bool {
    let pcs = Profile::new_xyz();
    match channels {
        1 => Transform::<[u8; 1], [f64; 3]>::new(
            profile,
            PixelFormat::GRAY_8,
            &pcs,
            PixelFormat::XYZ_DBL,
            Intent::RelativeColorimetric,
        )
        .is_ok(),
        3 => Transform::<[u8; 3], [f64; 3]>::new(
            profile,
            PixelFormat::RGB_8,
            &pcs,
            PixelFormat::XYZ_DBL,
            Intent::RelativeColorimetric,
        )
        .is_ok(),
        4 => Transform::<[u8; 4], [f64; 3]>::new(
            profile,
            PixelFormat::CMYK_8,
            &pcs,
            PixelFormat::XYZ_DBL,
            Intent::RelativeColorimetric,
        )
        .is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use lcms2::Profile;
    use pdf_oxide::{
        document::PdfDocument,
        object::{Object, ObjectRef},
    };
    use pdf_writer::{
        Content, Filter, Finish, Name, Null, Pdf, Rect, Ref, Str, TextStr,
        types::{MaskType, OutputIntentSubtype},
        writers::ColorSpace,
    };

    use super::{
        AuditContext, ContentWalkContext, MAX_DISTINCT_SPOT_COLORANTS, MAX_INDEXED_LOOKUP_BYTES,
        MAX_REFERENCE_DEPTH, PaintColorState, audit_active_icc_profiles, cal_alternate_identity,
        color_space_component_count, lab_alternate_identity, tint_function_is_readable,
    };

    const CMYK_OUTPUT_PROFILE: &[u8] =
        include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");

    fn numbers(values: &[f64]) -> Object {
        Object::Array(values.iter().copied().map(Object::Real).collect())
    }

    fn deflate(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;

        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).expect("fixture compresses");
        encoder.finish().expect("fixture compression finishes")
    }

    fn replace_first_equal_length(candidate: &mut [u8], from: &[u8], to: &[u8]) {
        assert_eq!(
            from.len(),
            to.len(),
            "fixture mutation preserves xref offsets"
        );
        let start = candidate
            .windows(from.len())
            .position(|window| window == from)
            .expect("fixture contains mutation target");
        candidate[start..start + to.len()].copy_from_slice(to);
    }

    fn page_with_profile(profile: &[u8], channels: i32, active: bool) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let profile_ref = Ref::new(5);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        page_writer
            .resources()
            .color_spaces()
            .insert(Name(b"PageIcc"))
            .start::<ColorSpace>()
            .icc_based(profile_ref);
        page_writer.finish();
        let mut operators = Content::new();
        if active {
            operators.set_fill_color_space(Name(b"PageIcc"));
            operators.set_fill_color([0.1, 0.2, 0.3]);
            operators.rect(10.0, 10.0, 20.0, 20.0);
            operators.fill_nonzero();
        } else {
            operators.set_fill_rgb(0.1, 0.2, 0.3);
        }
        pdf.stream(content, &operators.finish());
        pdf.icc_profile(profile_ref, profile).n(channels);
        pdf.finish()
    }

    fn page_with_profiled_image(profile: &[u8], channels: i32) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let profile_ref = Ref::new(5);
        let image_ref = Ref::new(6);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        page_writer
            .resources()
            .x_objects()
            .pair(Name(b"Picture"), image_ref);
        page_writer.finish();
        let mut operators = Content::new();
        operators.x_object(Name(b"Picture"));
        pdf.stream(content, &operators.finish());
        let mut image = pdf.image_xobject(image_ref, &[0, 0, 0]);
        image.width(1).height(1).bits_per_component(8);
        image.color_space().icc_based(profile_ref);
        image.finish();
        pdf.icc_profile(profile_ref, profile).n(channels);
        pdf.finish()
    }

    fn page_with_raw_indexed_image(samples: &[u8]) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let image_ref = Ref::new(5);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        page_writer
            .resources()
            .x_objects()
            .pair(Name(b"Picture"), image_ref);
        page_writer.finish();
        pdf.stream(content, b"/Picture Do");

        let mut image = pdf.stream(image_ref, samples);
        image.pair(Name(b"Type"), Name(b"XObject"));
        image.pair(Name(b"Subtype"), Name(b"Image"));
        image.pair(Name(b"Width"), 3);
        image.pair(Name(b"Height"), 2);
        image.pair(Name(b"BitsPerComponent"), 2);
        image
            .insert(Name(b"ColorSpace"))
            .array()
            .item(Name(b"Indexed"))
            .item(Name(b"DeviceRGB"))
            .item(3)
            .item(Str(&[0; 12]));
        image.finish();
        pdf.finish()
    }

    #[derive(Clone, Copy)]
    enum IndexedImageEncodingFixture {
        Raw,
        RawNullParameters,
        EmptyFilter,
        EmptyFilterNullParameters,
        Flate,
        FlateDirectNull,
        FlateDirectPredictor(i32),
        FlateArrayDirectNull,
        FlateArrayDirectPredictor(i32),
        FlateArraySingleNull,
        FlateArraySinglePredictor(i32),
        FlateCorrupt,
        FlateTruncated,
        FlateTrailing,
        Jpx,
        JpxChain,
        Unsupported,
        ExternalFile,
        ExternalFilter,
        ExternalDecodeParameters,
    }

    #[derive(Clone, Copy)]
    enum IndexedImageDecodeFixture {
        Absent,
        Pair([f32; 2]),
        One,
        Nonnumeric,
    }

    #[derive(Clone, Copy)]
    #[expect(
        clippy::struct_excessive_bools,
        reason = "fixture axes independently exercise reachability, indirection, aliases, and mask quarantine"
    )]
    struct IndexedImageFixture {
        width: Option<i32>,
        height: Option<i32>,
        bits_per_component: Option<i32>,
        decode: IndexedImageDecodeFixture,
        encoding: IndexedImageEncodingFixture,
        active: bool,
        nested: bool,
        named_color_space: bool,
        abbreviated_color_space: bool,
        omit_color_space: bool,
        indirect_entries: bool,
        image_mask: bool,
        repeats: usize,
    }

    impl Default for IndexedImageFixture {
        fn default() -> Self {
            Self {
                width: Some(3),
                height: Some(2),
                bits_per_component: Some(2),
                decode: IndexedImageDecodeFixture::Absent,
                encoding: IndexedImageEncodingFixture::Raw,
                active: true,
                nested: false,
                named_color_space: false,
                abbreviated_color_space: false,
                omit_color_space: false,
                indirect_entries: false,
                image_mask: false,
                repeats: 1,
            }
        }
    }

    #[expect(clippy::too_many_lines)]
    fn page_with_indexed_image(samples: &[u8], fixture: IndexedImageFixture) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let page_content = Ref::new(4);
        let image_ref = Ref::new(5);
        let width_ref = Ref::new(6);
        let height_ref = Ref::new(7);
        let bits_ref = Ref::new(8);
        let decode_ref = Ref::new(9);
        let decode_low_ref = Ref::new(10);
        let decode_high_ref = Ref::new(11);
        let filter_ref = Ref::new(12);
        let parameters_ref = Ref::new(13);
        let predictor_ref = Ref::new(14);
        let form_ref = Ref::new(15);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(page_content);
        let mut page_resources = page_writer.resources();
        page_resources.x_objects().pair(
            Name(if fixture.nested {
                b"Nested"
            } else {
                b"Picture"
            }),
            if fixture.nested { form_ref } else { image_ref },
        );
        if fixture.named_color_space && !fixture.nested {
            page_resources
                .color_spaces()
                .insert(Name(b"ImageIndexed"))
                .array()
                .item(Name(b"Indexed"))
                .item(Name(b"DeviceRGB"))
                .item(3)
                .item(Str(&[0; 12]));
        }
        page_resources.finish();
        page_writer.finish();
        let invocation = if fixture.active {
            let name = if fixture.nested { "Nested" } else { "Picture" };
            format!("/{name} Do ").repeat(fixture.repeats)
        } else {
            String::new()
        };
        pdf.stream(page_content, invocation.as_bytes());

        if fixture.nested {
            let mut form = pdf.form_xobject(form_ref, b"/Picture Do");
            form.bbox(Rect::new(0.0, 0.0, 10.0, 10.0));
            let mut resources = form.resources();
            resources.x_objects().pair(Name(b"Picture"), image_ref);
            if fixture.named_color_space {
                resources
                    .color_spaces()
                    .insert(Name(b"ImageIndexed"))
                    .array()
                    .item(Name(b"Indexed"))
                    .item(Name(b"DeviceRGB"))
                    .item(3)
                    .item(Str(&[0; 12]));
            }
            resources.finish();
            form.finish();
        }

        let mut encoded = match fixture.encoding {
            IndexedImageEncodingFixture::Flate
            | IndexedImageEncodingFixture::FlateDirectNull
            | IndexedImageEncodingFixture::FlateDirectPredictor(_)
            | IndexedImageEncodingFixture::FlateArrayDirectNull
            | IndexedImageEncodingFixture::FlateArrayDirectPredictor(_)
            | IndexedImageEncodingFixture::FlateArraySingleNull
            | IndexedImageEncodingFixture::FlateArraySinglePredictor(_)
            | IndexedImageEncodingFixture::FlateTruncated
            | IndexedImageEncodingFixture::FlateTrailing => deflate(samples),
            _ => samples.to_vec(),
        };
        if matches!(fixture.encoding, IndexedImageEncodingFixture::FlateTrailing) {
            encoded.extend_from_slice(b"trailing");
        }
        if matches!(
            fixture.encoding,
            IndexedImageEncodingFixture::FlateTruncated
        ) {
            encoded.truncate(encoded.len().saturating_sub(2));
        }
        let mut image = pdf.stream(image_ref, &encoded);
        image.pair(Name(b"Type"), Name(b"XObject"));
        image.pair(Name(b"Subtype"), Name(b"Image"));
        if let Some(width) = fixture.width {
            if fixture.indirect_entries {
                image.pair(Name(b"Width"), width_ref);
            } else {
                image.pair(Name(b"Width"), width);
            }
        }
        if let Some(height) = fixture.height {
            if fixture.indirect_entries {
                image.pair(Name(b"Height"), height_ref);
            } else {
                image.pair(Name(b"Height"), height);
            }
        }
        if let Some(bits) = fixture.bits_per_component {
            if fixture.indirect_entries {
                image.pair(Name(b"BitsPerComponent"), bits_ref);
            } else {
                image.pair(Name(b"BitsPerComponent"), bits);
            }
        }
        if fixture.image_mask {
            image.pair(Name(b"ImageMask"), true);
        }
        if fixture.omit_color_space {
        } else if fixture.abbreviated_color_space {
            image
                .insert(Name(b"CS"))
                .array()
                .item(Name(b"Indexed"))
                .item(Name(b"DeviceRGB"))
                .item(3)
                .item(Str(&[0; 12]));
        } else if fixture.named_color_space {
            image.pair(Name(b"ColorSpace"), Name(b"ImageIndexed"));
        } else {
            image
                .insert(Name(b"ColorSpace"))
                .array()
                .item(Name(b"Indexed"))
                .item(Name(b"DeviceRGB"))
                .item(3)
                .item(Str(&[0; 12]));
        }
        match fixture.decode {
            IndexedImageDecodeFixture::Absent => {}
            IndexedImageDecodeFixture::Pair(_values) if fixture.indirect_entries => {
                image.pair(Name(b"Decode"), decode_ref);
            }
            IndexedImageDecodeFixture::Pair(values) => {
                image.insert(Name(b"Decode")).array().items(values);
            }
            IndexedImageDecodeFixture::One => {
                image.insert(Name(b"Decode")).array().item(0);
            }
            IndexedImageDecodeFixture::Nonnumeric => {
                image
                    .insert(Name(b"Decode"))
                    .array()
                    .item(Name(b"bad"))
                    .item(0);
            }
        }
        if fixture.indirect_entries
            && matches!(
                fixture.encoding,
                IndexedImageEncodingFixture::FlateDirectPredictor(_)
            )
        {
            image.pair(Name(b"Filter"), filter_ref);
            image.pair(Name(b"DecodeParms"), parameters_ref);
        } else {
            match fixture.encoding {
                IndexedImageEncodingFixture::Raw => {}
                IndexedImageEncodingFixture::RawNullParameters => {
                    image.pair(Name(b"DecodeParms"), Null);
                }
                IndexedImageEncodingFixture::EmptyFilter => {
                    image.insert(Name(b"Filter")).array();
                    image.insert(Name(b"DecodeParms")).array();
                }
                IndexedImageEncodingFixture::EmptyFilterNullParameters => {
                    image.insert(Name(b"Filter")).array();
                    image.pair(Name(b"DecodeParms"), Null);
                }
                IndexedImageEncodingFixture::Flate
                | IndexedImageEncodingFixture::FlateCorrupt
                | IndexedImageEncodingFixture::FlateTruncated
                | IndexedImageEncodingFixture::FlateTrailing => {
                    image.pair(Name(b"Filter"), Name(b"FlateDecode"));
                }
                IndexedImageEncodingFixture::FlateDirectNull => {
                    image.pair(Name(b"Filter"), Name(b"FlateDecode"));
                    image.pair(Name(b"DecodeParms"), Null);
                }
                IndexedImageEncodingFixture::FlateDirectPredictor(predictor) => {
                    image.pair(Name(b"Filter"), Name(b"FlateDecode"));
                    image
                        .insert(Name(b"DecodeParms"))
                        .dict()
                        .pair(Name(b"Predictor"), predictor);
                }
                IndexedImageEncodingFixture::FlateArrayDirectNull => {
                    image
                        .insert(Name(b"Filter"))
                        .array()
                        .item(Name(b"FlateDecode"));
                    image.pair(Name(b"DecodeParms"), Null);
                }
                IndexedImageEncodingFixture::FlateArrayDirectPredictor(predictor) => {
                    image
                        .insert(Name(b"Filter"))
                        .array()
                        .item(Name(b"FlateDecode"));
                    image
                        .insert(Name(b"DecodeParms"))
                        .dict()
                        .pair(Name(b"Predictor"), predictor);
                }
                IndexedImageEncodingFixture::FlateArraySingleNull => {
                    image
                        .insert(Name(b"Filter"))
                        .array()
                        .item(Name(b"FlateDecode"));
                    image.insert(Name(b"DecodeParms")).array().item(Null);
                }
                IndexedImageEncodingFixture::FlateArraySinglePredictor(predictor) => {
                    image
                        .insert(Name(b"Filter"))
                        .array()
                        .item(Name(b"FlateDecode"));
                    image
                        .insert(Name(b"DecodeParms"))
                        .array()
                        .push()
                        .dict()
                        .pair(Name(b"Predictor"), predictor);
                }
                IndexedImageEncodingFixture::JpxChain => {
                    image
                        .insert(Name(b"Filter"))
                        .array()
                        .item(Name(b"FlateDecode"))
                        .item(Name(b"JPXDecode"));
                }
                IndexedImageEncodingFixture::Jpx => {
                    image.pair(Name(b"Filter"), Name(b"JPXDecode"));
                }
                IndexedImageEncodingFixture::Unsupported => {
                    image.pair(Name(b"Filter"), Name(b"LZWDecode"));
                }
                IndexedImageEncodingFixture::ExternalFile => {
                    image.pair(Name(b"F"), Str(b"external.bin"));
                }
                IndexedImageEncodingFixture::ExternalFilter => {
                    image.pair(Name(b"FFilter"), Name(b"FlateDecode"));
                }
                IndexedImageEncodingFixture::ExternalDecodeParameters => {
                    image.pair(Name(b"FDecodeParms"), Null);
                }
            }
        }
        image.finish();
        if fixture.indirect_entries {
            if let Some(width) = fixture.width {
                pdf.indirect(width_ref).primitive(width);
            }
            if let Some(height) = fixture.height {
                pdf.indirect(height_ref).primitive(height);
            }
            if let Some(bits) = fixture.bits_per_component {
                pdf.indirect(bits_ref).primitive(bits);
            }
            if let IndexedImageDecodeFixture::Pair(values) = fixture.decode {
                pdf.indirect(decode_ref)
                    .array()
                    .items([decode_low_ref, decode_high_ref]);
                pdf.indirect(decode_low_ref).primitive(values[0]);
                pdf.indirect(decode_high_ref).primitive(values[1]);
            }
            if let IndexedImageEncodingFixture::FlateDirectPredictor(predictor) = fixture.encoding {
                pdf.indirect(filter_ref).primitive(Name(b"FlateDecode"));
                pdf.indirect(parameters_ref)
                    .dict()
                    .pair(Name(b"Predictor"), predictor_ref);
                pdf.indirect(predictor_ref).primitive(predictor);
            }
        }
        pdf.finish()
    }

    fn page_with_profiled_form(profile: &[u8], channels: i32) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let page_content = Ref::new(4);
        let profile_ref = Ref::new(5);
        let form_ref = Ref::new(6);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(page_content);
        page_writer
            .resources()
            .x_objects()
            .pair(Name(b"Nested"), form_ref);
        page_writer.finish();
        let mut page_operators = Content::new();
        page_operators.x_object(Name(b"Nested"));
        pdf.stream(page_content, &page_operators.finish());

        let mut form_operators = Content::new();
        form_operators.set_fill_color_space(Name(b"FormIcc"));
        form_operators.set_fill_color([0.1, 0.2, 0.3]);
        form_operators.rect(0.0, 0.0, 10.0, 10.0);
        form_operators.fill_nonzero();
        let form_bytes = form_operators.finish();
        let mut form = pdf.form_xobject(form_ref, &form_bytes);
        form.bbox(Rect::new(0.0, 0.0, 10.0, 10.0));
        form.resources()
            .color_spaces()
            .insert(Name(b"FormIcc"))
            .start::<ColorSpace>()
            .icc_based(profile_ref);
        form.finish();
        pdf.icc_profile(profile_ref, profile).n(channels);
        pdf.finish()
    }

    fn page_with_lab(white_point: [f32; 3]) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        page_writer
            .resources()
            .color_spaces()
            .insert(Name(b"PageLab"))
            .start::<ColorSpace>()
            .lab(
                white_point,
                Some([0.0, 0.0, 0.0]),
                Some([-128.0, 127.0, -128.0, 127.0]),
            );
        page_writer.finish();
        let mut operators = Content::new();
        operators.set_fill_color_space(Name(b"PageLab"));
        operators.set_fill_color([50.0, 0.0, 0.0]);
        operators.rect(10.0, 10.0, 20.0, 20.0);
        operators.fill_nonzero();
        pdf.stream(content, &operators.finish());
        pdf.finish()
    }

    fn page_with_missing_pattern() -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        pdf.page(page)
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        pdf.stream(content, b"/Pattern cs /Missing scn");
        pdf.finish()
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PatternFixtureVariant {
        Valid,
        MissingTilingType,
        InvalidTilingType,
        MissingBBox,
        InvalidBBox,
        MissingXStep,
        ZeroXStep,
        MissingYStep,
        ZeroYStep,
        MissingResources,
        InvalidResources,
        ImageWithoutBitsPerComponent,
        ImageWithInvalidBitsPerComponent,
        ImageWithColorSpace,
        ImageWithMask,
        ImageWithSoftMask,
        ImageWithInlineAliases,
        IndexedImageSamplesValid,
        IndexedImageSamplesShort,
        ExtGStateWithTransfer,
        WithShadingResource,
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one fixture builder keeps the paired Pattern dictionary fault matrix explicit"
    )]
    fn page_with_tiling_pattern(
        page_content: &[u8],
        pattern_content: &[u8],
        pattern_type: Option<i32>,
        paint_type: Option<i32>,
        pattern_is_stream: bool,
        pattern_image_mask: Option<bool>,
        variant: PatternFixtureVariant,
    ) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let pattern = Ref::new(5);
        let image = Ref::new(6);
        let ext_g_state = Ref::new(7);
        let shading = Ref::new(8);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        let mut resources = page_writer.resources();
        resources
            .color_spaces()
            .insert(Name(b"PatternCmyk"))
            .array()
            .items([Name(b"Pattern"), Name(b"DeviceCMYK")]);
        resources.patterns().pair(Name(b"Tile"), pattern);
        resources.finish();
        page_writer.finish();
        pdf.stream(content, page_content);

        if pattern_is_stream {
            let mut writer = pdf.stream(pattern, pattern_content);
            writer.pair(Name(b"Type"), Name(b"Pattern"));
            if let Some(pattern_type) = pattern_type {
                writer.pair(Name(b"PatternType"), pattern_type);
            }
            if let Some(paint_type) = paint_type {
                writer.pair(Name(b"PaintType"), paint_type);
            }
            if variant != PatternFixtureVariant::MissingTilingType {
                writer.pair(
                    Name(b"TilingType"),
                    if variant == PatternFixtureVariant::InvalidTilingType {
                        4
                    } else {
                        1
                    },
                );
            }
            if variant != PatternFixtureVariant::MissingBBox {
                writer.pair(
                    Name(b"BBox"),
                    if variant == PatternFixtureVariant::InvalidBBox {
                        Rect::new(0.0, 0.0, 0.0, 10.0)
                    } else {
                        Rect::new(0.0, 0.0, 10.0, 10.0)
                    },
                );
            }
            if variant != PatternFixtureVariant::MissingXStep {
                writer.pair(
                    Name(b"XStep"),
                    if variant == PatternFixtureVariant::ZeroXStep {
                        0.0
                    } else {
                        10.0
                    },
                );
            }
            if variant != PatternFixtureVariant::MissingYStep {
                writer.pair(
                    Name(b"YStep"),
                    if variant == PatternFixtureVariant::ZeroYStep {
                        0.0
                    } else {
                        10.0
                    },
                );
            }
            if variant == PatternFixtureVariant::InvalidResources {
                writer.pair(Name(b"Resources"), 7);
            } else if variant != PatternFixtureVariant::MissingResources {
                let mut pattern_resources = writer.insert(Name(b"Resources")).dict();
                pattern_resources
                    .insert(Name(b"Pattern"))
                    .dict()
                    .pair(Name(b"Tile"), pattern);
                if pattern_image_mask.is_some() {
                    pattern_resources
                        .insert(Name(b"XObject"))
                        .dict()
                        .pair(Name(b"Picture"), image);
                }
                if variant == PatternFixtureVariant::ExtGStateWithTransfer {
                    pattern_resources
                        .insert(Name(b"ExtGState"))
                        .dict()
                        .pair(Name(b"State"), ext_g_state);
                }
                if variant == PatternFixtureVariant::WithShadingResource {
                    pattern_resources
                        .insert(Name(b"Shading"))
                        .dict()
                        .pair(Name(b"Shade"), shading);
                }
            }
        } else {
            let mut writer = pdf.indirect(pattern).dict();
            writer.pair(Name(b"Type"), Name(b"Pattern"));
            if let Some(pattern_type) = pattern_type {
                writer.pair(Name(b"PatternType"), pattern_type);
            }
            if let Some(paint_type) = paint_type {
                writer.pair(Name(b"PaintType"), paint_type);
            }
        }

        write_pattern_image(&mut pdf, image, pattern_image_mask, variant);
        if variant == PatternFixtureVariant::ExtGStateWithTransfer {
            let mut state = pdf.indirect(ext_g_state).dict();
            for key in [
                b"TR".as_slice(),
                b"TR2",
                b"BG",
                b"BG2",
                b"UCR",
                b"UCR2",
                b"HT",
            ] {
                state.pair(Name(key), Name(b"Identity"));
            }
        }
        if variant == PatternFixtureVariant::WithShadingResource {
            let mut shade = pdf.indirect(shading).dict();
            shade.pair(Name(b"ShadingType"), 2);
            shade.pair(Name(b"ColorSpace"), Name(b"DeviceCMYK"));
        }
        pdf.finish()
    }

    fn write_pattern_image(
        pdf: &mut Pdf,
        image: Ref,
        image_mask: Option<bool>,
        variant: PatternFixtureVariant,
    ) {
        let Some(image_mask) = image_mask else {
            return;
        };
        let indexed_image = matches!(
            variant,
            PatternFixtureVariant::IndexedImageSamplesValid
                | PatternFixtureVariant::IndexedImageSamplesShort
        );
        let samples = if variant == PatternFixtureVariant::IndexedImageSamplesValid {
            &[0xff, 0xff][..]
        } else {
            &[0xff][..]
        };
        let mut writer = pdf.stream(image, samples);
        writer.pair(Name(b"Type"), Name(b"XObject"));
        writer.pair(Name(b"Subtype"), Name(b"Image"));
        writer.pair(Name(b"Width"), if indexed_image { 3 } else { 1 });
        writer.pair(Name(b"Height"), if indexed_image { 2 } else { 1 });
        writer.pair(
            if variant == PatternFixtureVariant::ImageWithInlineAliases {
                Name(b"IM")
            } else {
                Name(b"ImageMask")
            },
            image_mask,
        );
        if indexed_image {
            writer.pair(Name(b"BitsPerComponent"), 2);
        } else if variant != PatternFixtureVariant::ImageWithoutBitsPerComponent {
            writer.pair(
                if variant == PatternFixtureVariant::ImageWithInlineAliases {
                    Name(b"BPC")
                } else {
                    Name(b"BitsPerComponent")
                },
                if variant == PatternFixtureVariant::ImageWithInvalidBitsPerComponent {
                    2
                } else {
                    1
                },
            );
        }
        if indexed_image {
            writer
                .insert(Name(b"ColorSpace"))
                .array()
                .item(Name(b"Indexed"))
                .item(Name(b"DeviceRGB"))
                .item(3)
                .item(Str(&[0; 12]));
        } else if !image_mask || variant == PatternFixtureVariant::ImageWithColorSpace {
            writer.pair(Name(b"ColorSpace"), Name(b"DeviceGray"));
        }
        if variant == PatternFixtureVariant::ImageWithMask {
            writer.pair(Name(b"Mask"), Null);
        }
        if variant == PatternFixtureVariant::ImageWithSoftMask {
            writer.pair(Name(b"SMask"), Null);
        }
    }

    fn page_with_shading_pattern(
        shading_color_space: Option<&[u8]>,
        shading_is_dictionary: bool,
        alias_to_pattern: bool,
    ) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let pattern = Ref::new(5);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        let mut resources = page_writer.resources();
        resources.patterns().pair(Name(b"Shade"), pattern);
        if alias_to_pattern {
            resources
                .color_spaces()
                .insert(Name(b"ShadingAlias"))
                .array()
                .items([Name(b"Pattern"), Name(b"DeviceCMYK")]);
        }
        resources.finish();
        page_writer.finish();
        pdf.stream(content, b"/Pattern cs /Shade scn");

        {
            let mut writer = pdf.indirect(pattern).dict();
            writer.pair(Name(b"Type"), Name(b"Pattern"));
            writer.pair(Name(b"PatternType"), 2);
            if shading_is_dictionary {
                let mut shading = writer.insert(Name(b"Shading")).dict();
                shading.pair(Name(b"ShadingType"), 2);
                if let Some(color_space) = shading_color_space {
                    shading.pair(Name(b"ColorSpace"), Name(color_space));
                }
            } else {
                writer.pair(Name(b"Shading"), 7);
            }
        }
        pdf.finish()
    }

    fn page_with_colored_pattern_in_form(pattern_content: &[u8]) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let page_content = Ref::new(4);
        let form = Ref::new(5);
        let pattern = Ref::new(6);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(page_content);
        let mut resources = page_writer.resources();
        let mut color_spaces = resources.color_spaces();
        let mut indexed = color_spaces.insert(Name(b"PageIndexed")).array();
        indexed.item(Name(b"Indexed"));
        indexed.item(Name(b"DeviceCMYK"));
        indexed.item(1);
        indexed.item(Str(&[0, 0, 0, 0, 255, 255, 255, 255]));
        indexed.finish();
        color_spaces.finish();
        resources.patterns().pair(Name(b"Tile"), pattern);
        resources.x_objects().pair(Name(b"Nested"), form);
        resources.finish();
        page_writer.finish();
        pdf.stream(page_content, b"/PageIndexed cs /Nested Do");

        let mut form_writer = pdf.form_xobject(form, b"/Pattern cs /Tile scn");
        form_writer.bbox(Rect::new(0.0, 0.0, 10.0, 10.0));
        form_writer.finish();
        let mut pattern_writer = pdf.stream(pattern, pattern_content);
        pattern_writer.pair(Name(b"Type"), Name(b"Pattern"));
        pattern_writer.pair(Name(b"PatternType"), 1);
        pattern_writer.pair(Name(b"PaintType"), 1);
        pattern_writer.pair(Name(b"TilingType"), 1);
        pattern_writer.pair(Name(b"BBox"), Rect::new(0.0, 0.0, 10.0, 10.0));
        pattern_writer.pair(Name(b"XStep"), 10.0);
        pattern_writer.pair(Name(b"YStep"), 10.0);
        pattern_writer.insert(Name(b"Resources")).dict();
        pattern_writer.finish();
        pdf.finish()
    }

    fn page_with_uncolored_pattern_form_image(
        image_mask: bool,
        bits_per_component: i32,
    ) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let page_content = Ref::new(4);
        let pattern = Ref::new(5);
        let form = Ref::new(6);
        let image = Ref::new(7);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(page_content);
        let mut resources = page_writer.resources();
        resources
            .color_spaces()
            .insert(Name(b"PatternCmyk"))
            .array()
            .items([Name(b"Pattern"), Name(b"DeviceCMYK")]);
        resources.patterns().pair(Name(b"Tile"), pattern);
        resources.finish();
        page_writer.finish();
        pdf.stream(page_content, b"/PatternCmyk cs 0 0 0 1 /Tile scn");

        let mut pattern_writer = pdf.stream(pattern, b"/Nested Do");
        pattern_writer.pair(Name(b"Type"), Name(b"Pattern"));
        pattern_writer.pair(Name(b"PatternType"), 1);
        pattern_writer.pair(Name(b"PaintType"), 2);
        pattern_writer.pair(Name(b"TilingType"), 1);
        pattern_writer.pair(Name(b"BBox"), Rect::new(0.0, 0.0, 10.0, 10.0));
        pattern_writer.pair(Name(b"XStep"), 10.0);
        pattern_writer.pair(Name(b"YStep"), 10.0);
        pattern_writer
            .insert(Name(b"Resources"))
            .dict()
            .insert(Name(b"XObject"))
            .dict()
            .pair(Name(b"Nested"), form);
        pattern_writer.finish();
        let mut form_writer = pdf.form_xobject(form, b"/Picture Do");
        form_writer.bbox(Rect::new(0.0, 0.0, 10.0, 10.0));
        form_writer
            .resources()
            .x_objects()
            .pair(Name(b"Picture"), image);
        form_writer.finish();
        let mut image_writer = pdf.stream(image, &[0]);
        image_writer.pair(Name(b"Type"), Name(b"XObject"));
        image_writer.pair(Name(b"Subtype"), Name(b"Image"));
        image_writer.pair(Name(b"Width"), 1);
        image_writer.pair(Name(b"Height"), 1);
        image_writer.pair(Name(b"ImageMask"), image_mask);
        image_writer.pair(Name(b"BitsPerComponent"), bits_per_component);
        if !image_mask {
            image_writer.pair(Name(b"ColorSpace"), Name(b"DeviceGray"));
        }
        image_writer.finish();
        pdf.finish()
    }

    #[derive(Clone, Copy)]
    enum IndexedHival {
        Integer(i32),
        Real(f32),
    }

    #[derive(Clone, Copy)]
    enum IndexedLookup<'a> {
        String(&'a [u8]),
        Stream(&'a [u8]),
        MalformedFlateStream(&'a [u8]),
        FlateArrayStream(&'a [u8]),
        FlateArrayNullParametersStream(&'a [u8]),
        FlatePredictorStream(&'a [u8], i32),
        Integer,
    }

    fn write_indexed_lookup_stream(pdf: &mut Pdf, lookup_ref: Ref, lookup: IndexedLookup<'_>) {
        match lookup {
            IndexedLookup::Stream(bytes) => {
                pdf.stream(lookup_ref, bytes);
            }
            IndexedLookup::MalformedFlateStream(bytes) => {
                pdf.stream(lookup_ref, bytes).filter(Filter::FlateDecode);
            }
            IndexedLookup::FlateArrayStream(bytes) => {
                pdf.stream(lookup_ref, bytes)
                    .insert(Name(b"Filter"))
                    .array()
                    .item(Name(b"FlateDecode"));
            }
            IndexedLookup::FlateArrayNullParametersStream(bytes) => {
                let mut stream = pdf.stream(lookup_ref, bytes);
                stream
                    .insert(Name(b"Filter"))
                    .array()
                    .item(Name(b"FlateDecode"));
                stream.insert(Name(b"DecodeParms")).array().item(Null);
            }
            IndexedLookup::FlatePredictorStream(bytes, predictor) => {
                let mut stream = pdf.stream(lookup_ref, bytes);
                stream.filter(Filter::FlateDecode);
                stream
                    .insert(Name(b"DecodeParms"))
                    .dict()
                    .pair(Name(b"Predictor"), predictor);
            }
            IndexedLookup::String(_) | IndexedLookup::Integer => {}
        }
    }

    #[derive(Clone, Copy)]
    enum IndexedBase {
        DeviceCmyk,
        NamedDeviceCmyk,
        DeviceN,
        ShadowedPattern,
    }

    fn page_with_indexed(hival: IndexedHival, lookup: IndexedLookup<'_>) -> Vec<u8> {
        page_with_indexed_base(hival, lookup, IndexedBase::DeviceCmyk)
    }

    fn page_with_indexed_base(
        hival: IndexedHival,
        lookup: IndexedLookup<'_>,
        base: IndexedBase,
    ) -> Vec<u8> {
        page_with_indexed_base_and_content(hival, lookup, base, None, None, false)
    }

    fn page_with_indexed_content(page_content: &[u8], form_content: Option<&[u8]>) -> Vec<u8> {
        page_with_indexed_base_and_content(
            IndexedHival::Integer(1),
            IndexedLookup::String(&[0, 0, 0, 0, 255, 128, 0, 64]),
            IndexedBase::DeviceCmyk,
            Some(page_content),
            form_content,
            false,
        )
    }

    fn page_with_invalid_indexed_selection_alias(page_content: &[u8]) -> Vec<u8> {
        page_with_indexed_base_and_content(
            IndexedHival::Integer(1),
            IndexedLookup::String(&[0, 0, 0, 0, 255, 128, 0, 64]),
            IndexedBase::DeviceCmyk,
            Some(page_content),
            None,
            true,
        )
    }

    #[allow(clippy::too_many_lines)] // One builder keeps every Indexed resource and content variant structurally identical.
    fn page_with_indexed_base_and_content(
        hival: IndexedHival,
        lookup: IndexedLookup<'_>,
        base: IndexedBase,
        page_content: Option<&[u8]>,
        form_content: Option<&[u8]>,
        invalid_selection_alias: bool,
    ) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let lookup_ref = Ref::new(5);
        let tint_ref = Ref::new(6);
        let form_ref = Ref::new(7);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        let mut resources = page_writer.resources();
        let mut color_spaces = resources.color_spaces();
        match base {
            IndexedBase::NamedDeviceCmyk => color_spaces
                .insert(Name(b"PaletteBase"))
                .start::<ColorSpace>()
                .device_cmyk(),
            IndexedBase::ShadowedPattern => color_spaces
                .insert(Name(b"Pattern"))
                .start::<ColorSpace>()
                .device_cmyk(),
            IndexedBase::DeviceCmyk | IndexedBase::DeviceN => {}
        }
        let mut indexed = color_spaces.insert(Name(b"PageIndexed")).array();
        indexed.item(Name(b"Indexed"));
        match base {
            IndexedBase::DeviceCmyk => {
                indexed.item(Name(b"DeviceCMYK"));
            }
            IndexedBase::NamedDeviceCmyk => {
                indexed.item(Name(b"PaletteBase"));
            }
            IndexedBase::ShadowedPattern => {
                indexed.item(Name(b"Pattern"));
            }
            IndexedBase::DeviceN => {
                let mut device_n = indexed.push().array();
                device_n.item(Name(b"DeviceN"));
                device_n
                    .push()
                    .array()
                    .items([Name(b"Orange"), Name(b"Green")]);
                device_n.item(Name(b"DeviceCMYK"));
                device_n.item(tint_ref);
            }
        }
        match hival {
            IndexedHival::Integer(value) => indexed.item(value),
            IndexedHival::Real(value) => indexed.item(value),
        };
        match lookup {
            IndexedLookup::String(bytes) => indexed.item(Str(bytes)),
            IndexedLookup::Stream(_)
            | IndexedLookup::MalformedFlateStream(_)
            | IndexedLookup::FlateArrayStream(_)
            | IndexedLookup::FlateArrayNullParametersStream(_)
            | IndexedLookup::FlatePredictorStream(_, _) => indexed.item(lookup_ref),
            IndexedLookup::Integer => indexed.item(42),
        };
        indexed.finish();
        if invalid_selection_alias {
            color_spaces.pair(Name(b"PageAlias"), Name(b"PageIndexed"));
        }
        color_spaces.finish();
        if form_content.is_some() {
            resources.x_objects().pair(Name(b"InheritedForm"), form_ref);
        }
        resources.finish();
        page_writer.finish();
        if let Some(page_content) = page_content {
            pdf.stream(content, page_content);
        } else {
            let mut operators = Content::new();
            operators.set_fill_color_space(Name(b"PageIndexed"));
            operators.set_fill_color([0.0]);
            operators.rect(10.0, 10.0, 20.0, 20.0);
            operators.fill_nonzero();
            pdf.stream(content, &operators.finish());
        }
        if let Some(form_content) = form_content {
            pdf.form_xobject(form_ref, form_content)
                .bbox(Rect::new(0.0, 0.0, 20.0, 20.0));
        }
        write_indexed_lookup_stream(&mut pdf, lookup_ref, lookup);
        if matches!(base, IndexedBase::DeviceN) {
            let mut tint = pdf.stream(tint_ref, b"{ pop pop 0 0 0 0 }");
            tint.pair(Name(b"FunctionType"), 4);
            tint.insert(Name(b"Domain"))
                .array()
                .items([0.0, 1.0, 0.0, 1.0]);
            tint.insert(Name(b"Range"))
                .array()
                .items([0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);
        }
        pdf.finish()
    }

    fn page_with_repeated_spot(conflicting_alternate: bool) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        let mut resources = page_writer.resources();
        let mut color_spaces = resources.color_spaces();
        for (resource_name, alternate_is_rgb) in [
            (b"BrandA".as_slice(), false),
            (b"BrandB".as_slice(), conflicting_alternate),
        ] {
            let mut separation = color_spaces
                .insert(Name(resource_name))
                .start::<ColorSpace>()
                .separation(Name(b"PublisherBlue"));
            if alternate_is_rgb {
                separation.alternate_color_space().device_rgb();
            } else {
                separation.alternate_color_space().device_cmyk();
            }
            let mut tint = separation.tint_exponential();
            tint.domain([0.0, 1.0]);
            if alternate_is_rgb {
                tint.c0([0.0, 0.0, 0.0]).c1([0.2, 0.4, 0.8]).n(1.0);
            } else {
                tint.c0([0.0, 0.0, 0.0, 0.0])
                    .c1([1.0, 0.5, 0.0, 0.0])
                    .n(1.0);
            }
        }
        color_spaces.finish();
        resources.finish();
        page_writer.finish();
        let mut operators = Content::new();
        for name in [Name(b"BrandA"), Name(b"BrandB")] {
            operators.set_fill_color_space(name);
            operators.set_fill_color([0.5]);
            operators.rect(10.0, 10.0, 20.0, 20.0);
            operators.fill_nonzero();
        }
        pdf.stream(content, &operators.finish());
        pdf.finish()
    }

    fn page_with_repeated_lab_spot(conflicting_parameters: bool) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        let mut resources = page_writer.resources();
        let mut color_spaces = resources.color_spaces();
        for (resource_name, second) in [
            (b"BrandLabA".as_slice(), false),
            (b"BrandLabB".as_slice(), true),
        ] {
            let mut separation = color_spaces
                .insert(Name(resource_name))
                .start::<ColorSpace>()
                .separation(Name(b"PublisherBlue"));
            separation.alternate_color_space().lab(
                if conflicting_parameters && second {
                    [0.9642, 1.0, 0.8249]
                } else {
                    [0.9505, 1.0, 1.089]
                },
                None,
                None,
            );
            separation
                .tint_exponential()
                .domain([0.0, 1.0])
                .c0([100.0, 0.0, 0.0])
                .c1([50.0, 20.0, -40.0])
                .n(1.0);
        }
        color_spaces.finish();
        resources.finish();
        page_writer.finish();
        let mut operators = Content::new();
        for name in [Name(b"BrandLabA"), Name(b"BrandLabB")] {
            operators.set_fill_color_space(name);
            operators.set_fill_color([0.5]);
            operators.rect(10.0, 10.0, 20.0, 20.0);
            operators.fill_nonzero();
        }
        pdf.stream(content, &operators.finish());
        pdf.finish()
    }

    fn page_with_repeated_calrgb_spot(conflicting_parameters: bool) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        let mut resources = page_writer.resources();
        let mut color_spaces = resources.color_spaces();
        for (resource_name, second) in [
            (b"CalSpotA".as_slice(), false),
            (b"CalSpotB".as_slice(), true),
        ] {
            let mut separation = color_spaces
                .insert(Name(resource_name))
                .start::<ColorSpace>()
                .separation(Name(b"PublisherBlue"));
            separation.alternate_color_space().cal_rgb(
                [0.9505, 1.0, 1.089],
                second.then_some([0.0, -0.0, 0.0]),
                second.then_some(if conflicting_parameters {
                    [1.2, 1.0, 1.0]
                } else {
                    [1.0, 1.0, 1.0]
                }),
                second.then_some([1.0, 0.0, -0.0, 0.0, 1.0, 0.0, 0.0, -0.0, 1.0]),
            );
            separation
                .tint_exponential()
                .domain([0.0, 1.0])
                .c0([0.0, 0.0, 0.0])
                .c1([0.2, 0.4, 0.8])
                .n(1.0);
        }
        color_spaces.finish();
        resources.finish();
        page_writer.finish();
        let mut operators = Content::new();
        for name in [Name(b"CalSpotA"), Name(b"CalSpotB")] {
            operators.set_fill_color_space(name);
            operators.set_fill_color([0.5]);
            operators.rect(10.0, 10.0, 20.0, 20.0);
            operators.fill_nonzero();
        }
        pdf.stream(content, &operators.finish());
        pdf.finish()
    }

    fn page_with_cmyk_intent_and_device_rgb(default_rgb_override: bool) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let output_profile = Ref::new(5);
        let rgb_profile = Ref::new(6);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        {
            let mut catalog_writer = pdf.catalog(catalog);
            catalog_writer.pages(pages);
            catalog_writer
                .output_intents()
                .push()
                .subtype(OutputIntentSubtype::PDFX)
                .output_condition_identifier(TextStr("CMYK fixture"))
                .dest_output_profile(output_profile);
        }
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        if default_rgb_override {
            page_writer
                .resources()
                .color_spaces()
                .insert(Name(b"DefaultRGB"))
                .start::<ColorSpace>()
                .icc_based(rgb_profile);
        }
        page_writer.finish();
        let mut operators = Content::new();
        operators.set_fill_rgb(0.1, 0.2, 0.3);
        operators.rect(10.0, 10.0, 20.0, 20.0);
        operators.fill_nonzero();
        pdf.stream(content, &operators.finish());
        pdf.icc_profile(output_profile, CMYK_OUTPUT_PROFILE).n(4);
        if default_rgb_override {
            let profile = Profile::new_srgb()
                .icc()
                .expect("LittleCMS serializes sRGB");
            pdf.icc_profile(rgb_profile, &profile).n(3);
        }
        pdf.finish()
    }

    fn page_with_profiled_image_soft_mask(profile: &[u8]) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let image = Ref::new(5);
        let soft_mask = Ref::new(6);
        let profile_ref = Ref::new(7);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        page_writer
            .resources()
            .x_objects()
            .pair(Name(b"Picture"), image);
        page_writer.finish();
        let mut operators = Content::new();
        operators.x_object(Name(b"Picture"));
        pdf.stream(content, &operators.finish());
        let mut image_writer = pdf.image_xobject(image, &[0, 0, 0, 0]);
        image_writer
            .width(1)
            .height(1)
            .bits_per_component(8)
            .pair(Name(b"SMask"), soft_mask);
        image_writer.color_space().device_cmyk();
        image_writer.finish();
        let mut mask_writer = pdf.image_xobject(soft_mask, &[128]);
        mask_writer.width(1).height(1).bits_per_component(8);
        mask_writer.color_space().icc_based(profile_ref);
        mask_writer.finish();
        pdf.icc_profile(profile_ref, profile).n(1);
        pdf.finish()
    }

    fn page_with_profiled_soft_mask_group(profile: &[u8]) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let content = Ref::new(4);
        let state = Ref::new(5);
        let group = Ref::new(6);
        let profile_ref = Ref::new(7);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        let mut page_writer = pdf.page(page);
        page_writer
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
            .contents(content);
        page_writer
            .resources()
            .ext_g_states()
            .pair(Name(b"MaskState"), state);
        page_writer.finish();
        let mut operators = Content::new();
        operators.set_parameters(Name(b"MaskState"));
        operators.set_fill_cmyk(0.0, 0.0, 0.0, 1.0);
        operators.rect(10.0, 10.0, 20.0, 20.0);
        operators.fill_nonzero();
        pdf.stream(content, &operators.finish());
        let mut group_content = Content::new();
        group_content.set_fill_color_space(Name(b"MaskIcc"));
        group_content.set_fill_color([0.5]);
        group_content.rect(0.0, 0.0, 20.0, 20.0);
        group_content.fill_nonzero();
        let group_bytes = group_content.finish();
        let mut group_writer = pdf.form_xobject(group, &group_bytes);
        group_writer.bbox(Rect::new(0.0, 0.0, 20.0, 20.0));
        group_writer
            .resources()
            .color_spaces()
            .insert(Name(b"MaskIcc"))
            .start::<ColorSpace>()
            .icc_based(profile_ref);
        let mut group_settings = group_writer.group();
        group_settings.transparency().isolated(true);
        group_settings.color_space().device_gray();
        group_settings.finish();
        group_writer.finish();
        let mut state_writer = pdf.ext_graphics(state);
        let mut mask = state_writer.soft_mask();
        mask.subtype(MaskType::Luminosity).group(group);
        mask.finish();
        state_writer.finish();
        pdf.icc_profile(profile_ref, profile).n(1);
        pdf.finish()
    }

    #[test]
    fn malformed_active_profile_is_a_blocking_finding() {
        let candidate = page_with_profile(b"not an ICC profile", 3, true);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert_eq!(audit.profiles_checked(), 1);
        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("LittleCMS rejected"))
        );
    }

    #[test]
    fn valid_active_srgb_profile_passes() {
        let profile = Profile::new_srgb()
            .icc()
            .expect("LittleCMS serializes sRGB");
        let candidate = page_with_profile(&profile, 3, true);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert_eq!(audit.profiles_checked(), 1);
        assert!(audit.findings().is_empty());
    }

    #[test]
    fn declared_channel_mismatch_is_a_blocking_finding() {
        let profile = Profile::new_srgb()
            .icc()
            .expect("LittleCMS serializes sRGB");
        let candidate = page_with_profile(&profile, 4, true);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| { finding.contains("color space disagrees with /N 4") })
        );
    }

    #[test]
    fn malformed_unused_profile_is_not_treated_as_active() {
        let candidate = page_with_profile(b"not an ICC profile", 3, false);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert_eq!(audit.profiles_checked(), 0);
        assert!(audit.findings().is_empty());
    }

    #[test]
    fn malformed_profile_in_a_painted_image_is_rejected() {
        let candidate = page_with_profiled_image(b"not an ICC profile", 3);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert_eq!(audit.profiles_checked(), 1);
        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("image /Picture"))
        );
    }

    #[test]
    fn active_raw_indexed_image_requires_exact_row_padded_sample_extent() {
        let control = audit_active_icc_profiles(&page_with_raw_indexed_image(&[0xff, 0xff]))
            .expect("control fixture is parseable");
        assert!(control.findings().is_empty(), "{:?}", control.findings());

        for samples in [&[0xff][..], &[0xff, 0xff, 0xff][..]] {
            let fault = audit_active_icc_profiles(&page_with_raw_indexed_image(samples))
                .expect("fault fixture is parseable");
            assert!(
                fault
                    .findings()
                    .iter()
                    .any(|finding| finding.contains("must decode to exactly 2 sample bytes")),
                "{:?}",
                fault.findings()
            );
        }
    }

    #[test]
    fn active_indexed_image_validates_geometry_bits_and_decode_without_sample_range_checks() {
        for (bits_per_component, sample_bytes) in [(1, 2_usize), (2, 2), (4, 4), (8, 6), (16, 12)] {
            let candidate = page_with_indexed_image(
                &vec![0xff; sample_bytes],
                IndexedImageFixture {
                    bits_per_component: Some(bits_per_component),
                    decode: IndexedImageDecodeFixture::Pair([-100.0, 100.0]),
                    ..IndexedImageFixture::default()
                },
            );
            let audit = audit_active_icc_profiles(&candidate).expect("control is parseable");
            assert!(
                audit.findings().is_empty(),
                "{bits_per_component}: {:?}",
                audit.findings()
            );
        }

        for (fixture, expected) in [
            (
                IndexedImageFixture {
                    width: None,
                    ..IndexedImageFixture::default()
                },
                "positive integer /Width",
            ),
            (
                IndexedImageFixture {
                    width: Some(0),
                    ..IndexedImageFixture::default()
                },
                "positive integer /Width",
            ),
            (
                IndexedImageFixture {
                    height: Some(0),
                    ..IndexedImageFixture::default()
                },
                "positive integer /Height",
            ),
            (
                IndexedImageFixture {
                    bits_per_component: Some(3),
                    ..IndexedImageFixture::default()
                },
                "/BitsPerComponent must be 1, 2, 4, 8, or 16",
            ),
            (
                IndexedImageFixture {
                    decode: IndexedImageDecodeFixture::One,
                    ..IndexedImageFixture::default()
                },
                "/Decode must contain exactly two finite numbers",
            ),
            (
                IndexedImageFixture {
                    decode: IndexedImageDecodeFixture::Nonnumeric,
                    ..IndexedImageFixture::default()
                },
                "/Decode must contain exactly two finite numbers",
            ),
        ] {
            let audit = audit_active_icc_profiles(&page_with_indexed_image(&[0xff, 0xff], fixture))
                .expect("fault is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected)),
                "{expected}: {:?}",
                audit.findings()
            );
        }

        let indirect = audit_active_icc_profiles(&page_with_indexed_image(
            &[0xff, 0xff],
            IndexedImageFixture {
                decode: IndexedImageDecodeFixture::Pair([3.0, 0.0]),
                encoding: IndexedImageEncodingFixture::FlateDirectPredictor(1),
                named_color_space: true,
                indirect_entries: true,
                ..IndexedImageFixture::default()
            },
        ))
        .expect("indirect control is parseable");
        assert!(indirect.findings().is_empty(), "{:?}", indirect.findings());
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one grouped fixture matrix keeps accepted encodings and distinct stream faults together"
    )]
    fn active_indexed_image_accepts_only_complete_raw_or_single_flate_sample_streams() {
        for encoding in [
            IndexedImageEncodingFixture::Raw,
            IndexedImageEncodingFixture::RawNullParameters,
            IndexedImageEncodingFixture::EmptyFilter,
            IndexedImageEncodingFixture::EmptyFilterNullParameters,
            IndexedImageEncodingFixture::Flate,
            IndexedImageEncodingFixture::FlateDirectNull,
            IndexedImageEncodingFixture::FlateDirectPredictor(1),
            IndexedImageEncodingFixture::FlateArrayDirectNull,
            IndexedImageEncodingFixture::FlateArrayDirectPredictor(1),
            IndexedImageEncodingFixture::FlateArraySingleNull,
            IndexedImageEncodingFixture::FlateArraySinglePredictor(1),
        ] {
            let audit = audit_active_icc_profiles(&page_with_indexed_image(
                &[0xff, 0xff],
                IndexedImageFixture {
                    encoding,
                    ..IndexedImageFixture::default()
                },
            ))
            .expect("control is parseable");
            assert!(audit.findings().is_empty(), "{:?}", audit.findings());
        }

        for fixture in [
            IndexedImageFixture {
                bits_per_component: None,
                encoding: IndexedImageEncodingFixture::Jpx,
                omit_color_space: true,
                ..IndexedImageFixture::default()
            },
            IndexedImageFixture {
                bits_per_component: None,
                encoding: IndexedImageEncodingFixture::Jpx,
                ..IndexedImageFixture::default()
            },
        ] {
            let audit = audit_active_icc_profiles(&page_with_indexed_image(b"fake-jpx", fixture))
                .expect("deferred JPX control is parseable");
            assert!(audit.findings().is_empty(), "{:?}", audit.findings());
        }

        for (encoding, expected) in [
            (
                IndexedImageEncodingFixture::FlateDirectPredictor(2),
                "supported complete raw or single-Flate stream",
            ),
            (
                IndexedImageEncodingFixture::FlateCorrupt,
                "supported complete raw or single-Flate stream",
            ),
            (
                IndexedImageEncodingFixture::FlateTruncated,
                "supported complete raw or single-Flate stream",
            ),
            (
                IndexedImageEncodingFixture::FlateTrailing,
                "supported complete raw or single-Flate stream",
            ),
            (
                IndexedImageEncodingFixture::Unsupported,
                "supported complete raw or single-Flate stream",
            ),
            (
                IndexedImageEncodingFixture::JpxChain,
                "supported complete raw or single-Flate stream",
            ),
            (
                IndexedImageEncodingFixture::ExternalFile,
                "unsupported external image data entry /F",
            ),
            (
                IndexedImageEncodingFixture::ExternalFilter,
                "unsupported external image data entry /FFilter",
            ),
            (
                IndexedImageEncodingFixture::ExternalDecodeParameters,
                "unsupported external image data entry /FDecodeParms",
            ),
        ] {
            let audit = audit_active_icc_profiles(&page_with_indexed_image(
                &[0xff, 0xff],
                IndexedImageFixture {
                    encoding,
                    ..IndexedImageFixture::default()
                },
            ))
            .expect("fault is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected)),
                "{expected}: {:?}",
                audit.findings()
            );
        }

        let decoded_too_long = audit_active_icc_profiles(&page_with_indexed_image(
            &[0xff, 0xff, 0xff],
            IndexedImageFixture {
                encoding: IndexedImageEncodingFixture::Flate,
                ..IndexedImageFixture::default()
            },
        ))
        .expect("fault is parseable");
        assert!(
            decoded_too_long
                .findings()
                .iter()
                .any(|finding| finding.contains("must decode to exactly 2 sample bytes"))
        );
    }

    #[test]
    fn indexed_image_rejects_missing_noninteger_and_over_budget_intrinsic_entries() {
        for (fixture, expected) in [
            (
                IndexedImageFixture {
                    height: None,
                    ..IndexedImageFixture::default()
                },
                "positive integer /Height",
            ),
            (
                IndexedImageFixture {
                    height: Some(-1),
                    ..IndexedImageFixture::default()
                },
                "positive integer /Height",
            ),
            (
                IndexedImageFixture {
                    bits_per_component: None,
                    ..IndexedImageFixture::default()
                },
                "/BitsPerComponent must be",
            ),
        ] {
            let audit = audit_active_icc_profiles(&page_with_indexed_image(&[0xff, 0xff], fixture))
                .expect("fault is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected))
            );
        }

        let mut noninteger = page_with_indexed_image(&[0xff, 0xff], IndexedImageFixture::default());
        replace_first_equal_length(&mut noninteger, b"/Width 3", b"/Width[]");
        let audit = audit_active_icc_profiles(&noninteger).expect("fault is parseable");
        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("positive integer /Width"))
        );

        let oversized = page_with_indexed_image(
            &[],
            IndexedImageFixture {
                width: Some(i32::MAX),
                height: Some(i32::MAX),
                bits_per_component: Some(16),
                ..IndexedImageFixture::default()
            },
        );
        assert!(matches!(
            audit_active_icc_profiles(&oversized),
            Err(super::ValidationError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn indexed_image_decode_numeric_validation_rejects_nonnumeric_and_nonfinite_values() {
        let document = PdfDocument::from_bytes(page_with_raw_indexed_image(&[0xff, 0xff]))
            .expect("fixture is parseable");
        for decode in [
            Object::Array(vec![Object::Integer(0), Object::Name("bad".into())]),
            Object::Array(vec![Object::Real(0.0), Object::Real(f64::INFINITY)]),
        ] {
            assert_eq!(super::numeric_array(&document, &decode, 2).unwrap(), None);
        }
    }

    #[test]
    fn indexed_image_samples_are_checked_only_when_reached_as_ordinary_images() {
        let nested = audit_active_icc_profiles(&page_with_indexed_image(
            &[0xff],
            IndexedImageFixture {
                nested: true,
                named_color_space: true,
                ..IndexedImageFixture::default()
            },
        ))
        .expect("nested fault is parseable");
        assert!(
            nested
                .findings()
                .iter()
                .any(|finding| finding.contains("sample bytes"))
        );

        let unused = audit_active_icc_profiles(&page_with_indexed_image(
            &[],
            IndexedImageFixture {
                active: false,
                width: None,
                ..IndexedImageFixture::default()
            },
        ))
        .expect("unused control is parseable");
        assert!(unused.findings().is_empty(), "{:?}", unused.findings());

        let mask = audit_active_icc_profiles(&page_with_indexed_image(
            &[],
            IndexedImageFixture {
                bits_per_component: Some(1),
                image_mask: true,
                omit_color_space: true,
                ..IndexedImageFixture::default()
            },
        ))
        .expect("mask control is parseable");
        assert!(mask.findings().is_empty(), "{:?}", mask.findings());

        let mut nonboolean_mask = page_with_indexed_image(
            &[],
            IndexedImageFixture {
                bits_per_component: Some(1),
                image_mask: true,
                omit_color_space: true,
                ..IndexedImageFixture::default()
            },
        );
        replace_first_equal_length(&mut nonboolean_mask, b"/ImageMask true", b"/ImageMask/Name");
        let audit = audit_active_icc_profiles(&nonboolean_mask).expect("mask fault is parseable");
        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("nonboolean /ImageMask"))
        );

        for fixture in [
            IndexedImageFixture {
                width: None,
                bits_per_component: Some(1),
                image_mask: true,
                omit_color_space: true,
                ..IndexedImageFixture::default()
            },
            IndexedImageFixture {
                bits_per_component: Some(1),
                image_mask: true,
                ..IndexedImageFixture::default()
            },
            IndexedImageFixture {
                image_mask: true,
                omit_color_space: true,
                decode: IndexedImageDecodeFixture::Pair([0.0, 2.0]),
                ..IndexedImageFixture::default()
            },
        ] {
            let audit = audit_active_icc_profiles(&page_with_indexed_image(&[], fixture))
                .expect("mask fault is parseable");
            assert!(audit.findings().iter().any(|finding| {
                finding.contains("lacks required color-neutral image-mask structure")
            }));
        }

        for fixture in [
            IndexedImageFixture {
                abbreviated_color_space: true,
                ..IndexedImageFixture::default()
            },
            IndexedImageFixture {
                omit_color_space: true,
                ..IndexedImageFixture::default()
            },
        ] {
            let audit = audit_active_icc_profiles(&page_with_indexed_image(&[], fixture))
                .expect("color-space fault is parseable");
            assert!(!audit.findings().is_empty());
        }
    }

    #[test]
    fn indexed_image_data_proof_is_cached_without_caching_resource_context() {
        fn resources(color_space: Object) -> Object {
            let mut xobjects = std::collections::HashMap::new();
            xobjects.insert(
                "Picture".to_owned(),
                Object::Reference(ObjectRef { id: 5, r#gen: 0 }),
            );
            let mut color_spaces = std::collections::HashMap::new();
            color_spaces.insert("ImageIndexed".to_owned(), color_space);
            let mut resources = std::collections::HashMap::new();
            resources.insert("XObject".to_owned(), Object::Dictionary(xobjects));
            resources.insert("ColorSpace".to_owned(), Object::Dictionary(color_spaces));
            Object::Dictionary(resources)
        }

        let candidate = page_with_indexed_image(
            &[0xff, 0xff],
            IndexedImageFixture {
                active: false,
                named_color_space: true,
                ..IndexedImageFixture::default()
            },
        );
        let document = PdfDocument::from_bytes(candidate).expect("fixture is parseable");
        let good = resources(Object::Array(vec![
            Object::Name("Indexed".into()),
            Object::Name("DeviceRGB".into()),
            Object::Integer(3),
            Object::String(vec![0; 12]),
        ]));
        let bad = resources(Object::Array(vec![Object::Name("Bogus".into())]));
        let mut context = AuditContext::default();
        for active_resources in [&good, &good, &bad] {
            context
                .walk_content(
                    b"/Picture Do",
                    ContentWalkContext {
                        document: &document,
                        resources: active_resources,
                        page_number: 1,
                        owner: "test",
                        depth: 0,
                        uncolored_pattern: false,
                    },
                    PaintColorState::default(),
                )
                .expect("walk succeeds");
        }
        assert_eq!(context.indexed_image_data.len(), 1);
        assert_eq!(context.indexed_image_encoded_bytes, 2);
        assert_eq!(context.indexed_image_decoded_bytes, 2);
        assert!(
            context
                .findings
                .iter()
                .any(|finding| finding.contains("/Bogus"))
        );
    }

    #[test]
    fn distinct_flate_attempts_share_the_aggregate_decoded_work_budget() {
        let document = PdfDocument::from_bytes(page_with_raw_indexed_image(&[0xff, 0xff]))
            .expect("fixture is parseable");
        let dictionary = std::collections::HashMap::from([(
            "Filter".to_owned(),
            Object::Name("FlateDecode".to_owned()),
        )]);
        let over_expanding = deflate(&[0; 8]);
        let mut context = AuditContext {
            indexed_image_decoded_bytes: super::MAX_INDEXED_IMAGE_BYTES - 4,
            ..AuditContext::default()
        };
        assert!(matches!(
            context.indexed_image_data_proof(
                &document,
                Some(ObjectRef { id: 40, r#gen: 0 }),
                &dictionary,
                &over_expanding,
                2,
            ),
            Ok(super::IndexedImageDataProof::Decoded(3))
        ));
        assert!(matches!(
            context.indexed_image_data_proof(
                &document,
                Some(ObjectRef { id: 41, r#gen: 0 }),
                &dictionary,
                &over_expanding,
                2,
            ),
            Err(super::ValidationError::OutputLimitExceeded)
        ));

        let mut corrupt = deflate(&[0; 8]);
        *corrupt.last_mut().expect("zlib stream has a checksum") ^= 0xff;
        let mut context = AuditContext {
            indexed_image_decoded_bytes: super::MAX_INDEXED_IMAGE_BYTES - 9,
            ..AuditContext::default()
        };
        assert_eq!(
            context
                .indexed_image_data_proof(
                    &document,
                    Some(ObjectRef { id: 42, r#gen: 0 }),
                    &dictionary,
                    &corrupt,
                    8,
                )
                .expect("corrupt attempt remains a finding proof"),
            super::IndexedImageDataProof::Invalid
        );
        assert_eq!(
            context.indexed_image_decoded_bytes,
            super::MAX_INDEXED_IMAGE_BYTES - 1
        );
        assert!(matches!(
            context.indexed_image_data_proof(
                &document,
                Some(ObjectRef { id: 43, r#gen: 0 }),
                &dictionary,
                &corrupt,
                8,
            ),
            Err(super::ValidationError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn malformed_profile_in_a_painted_form_is_rejected() {
        let candidate = page_with_profiled_form(b"not an ICC profile", 3);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert_eq!(audit.profiles_checked(), 1);
        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("form /Nested"))
        );
    }

    #[test]
    fn valid_active_lab_color_space_passes() {
        let candidate = page_with_lab([0.9505, 1.0, 1.089]);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty());
    }

    #[test]
    fn malformed_active_lab_white_point_is_rejected() {
        let candidate = page_with_lab([0.9505, 0.9, 1.089]);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("malformed Lab color space"))
        );
    }

    #[test]
    fn unavailable_pattern_resource_is_rejected() {
        let candidate = page_with_missing_pattern();
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("selects unavailable Pattern /Missing")),
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn colored_and_uncolored_tiling_pattern_controls_pass() {
        for candidate in [
            page_with_tiling_pattern(
                b"/Pattern cs /Tile scn",
                b"0 g 0 0 10 10 re f",
                Some(1),
                Some(1),
                true,
                None,
                PatternFixtureVariant::Valid,
            ),
            page_with_tiling_pattern(
                b"/PatternCmyk cs 0 0 0 1 /Tile scn",
                b"0 0 10 10 re f",
                Some(1),
                Some(2),
                true,
                None,
                PatternFixtureVariant::Valid,
            ),
        ] {
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(audit.findings().is_empty(), "{:?}", audit.findings());
        }

        let control = audit_active_icc_profiles(&page_with_colored_pattern_in_form(b"0 sc"))
            .expect("fixture is parseable");
        assert!(control.findings().is_empty(), "{:?}", control.findings());
        let fault = audit_active_icc_profiles(&page_with_colored_pattern_in_form(b"0 1 sc"))
            .expect("fixture is parseable");
        assert!(
            fault
                .findings()
                .iter()
                .any(|finding| finding.contains("Indexed color space")
                    && finding.contains("exactly one finite numeric component")),
            "{:?}",
            fault.findings()
        );
    }

    #[test]
    fn colored_tiling_pattern_reaches_ordinary_indexed_image_samples() {
        for (variant, expected_finding) in [
            (PatternFixtureVariant::IndexedImageSamplesValid, false),
            (PatternFixtureVariant::IndexedImageSamplesShort, true),
        ] {
            let candidate = page_with_tiling_pattern(
                b"/Pattern cs /Tile scn",
                b"/Picture Do",
                Some(1),
                Some(1),
                true,
                Some(false),
                variant,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert_eq!(
                audit.findings().iter().any(|finding| finding
                    .contains("Indexed image must decode to exactly 2 sample bytes")),
                expected_finding,
                "{variant:?}: {:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn pattern_selection_enforces_typed_component_arity_and_scn() {
        for (content, expected) in [
            (
                b"/Pattern cs 0 /Tile scn".as_slice(),
                "exactly 0 finite numeric component(s)",
            ),
            (
                b"/PatternCmyk cs 0 0 0 /Tile scn".as_slice(),
                "exactly 4 finite numeric component(s)",
            ),
            (
                b"/PatternCmyk cs 0 0 0 1 scn".as_slice(),
                "requires scn with a pattern name",
            ),
            (
                b"/Pattern cs 0 sc".as_slice(),
                "requires scn with a pattern name",
            ),
        ] {
            let candidate = page_with_tiling_pattern(
                content,
                b"0 0 10 10 re f",
                Some(1),
                Some(if content.starts_with(b"/PatternCmyk") {
                    2
                } else {
                    1
                }),
                true,
                None,
                PatternFixtureVariant::Valid,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected)),
                "{:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn tiling_pattern_paint_type_must_match_the_selected_pattern_space() {
        for (content, paint_type) in [
            (b"/Pattern cs /Tile scn".as_slice(), 2),
            (b"/PatternCmyk cs 0 0 0 1 /Tile scn".as_slice(), 1),
        ] {
            let candidate = page_with_tiling_pattern(
                content,
                b"0 0 10 10 re f",
                Some(1),
                Some(paint_type),
                true,
                None,
                PatternFixtureVariant::Valid,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit.findings().iter().any(|finding| finding
                    .contains("paint type does not match the selected Pattern color space")),
                "{:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn malformed_tiling_pattern_structure_is_rejected() {
        for (pattern_type, paint_type, pattern_is_stream, expected) in [
            (None, Some(1), true, "no integer /PatternType"),
            (Some(3), Some(1), true, "unsupported /PatternType 3"),
            (Some(1), None, true, "no supported integer /PaintType"),
            (Some(1), Some(3), true, "no supported integer /PaintType"),
            (Some(1), Some(1), false, "is not a content stream"),
        ] {
            let candidate = page_with_tiling_pattern(
                b"/Pattern cs /Tile scn",
                b"0 0 10 10 re f",
                pattern_type,
                paint_type,
                pattern_is_stream,
                None,
                PatternFixtureVariant::Valid,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected)),
                "{:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn tiling_pattern_requires_complete_finite_geometry_and_resources() {
        for (variant, expected) in [
            (
                PatternFixtureVariant::MissingTilingType,
                "has no supported integer /TilingType",
            ),
            (
                PatternFixtureVariant::InvalidTilingType,
                "has no supported integer /TilingType",
            ),
            (PatternFixtureVariant::MissingBBox, "has no valid /BBox"),
            (PatternFixtureVariant::InvalidBBox, "has no valid /BBox"),
            (
                PatternFixtureVariant::MissingXStep,
                "has no nonzero finite /XStep",
            ),
            (
                PatternFixtureVariant::ZeroXStep,
                "has no nonzero finite /XStep",
            ),
            (
                PatternFixtureVariant::MissingYStep,
                "has no nonzero finite /YStep",
            ),
            (
                PatternFixtureVariant::ZeroYStep,
                "has no nonzero finite /YStep",
            ),
            (
                PatternFixtureVariant::MissingResources,
                "has no readable /Resources dictionary",
            ),
            (
                PatternFixtureVariant::InvalidResources,
                "has no readable /Resources dictionary",
            ),
        ] {
            let candidate = page_with_tiling_pattern(
                b"/Pattern cs /Tile scn",
                b"0 g 0 0 10 10 re f",
                Some(1),
                Some(1),
                true,
                None,
                variant,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected)),
                "{variant:?}: {:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn uncolored_tiling_pattern_rejects_color_content_and_recursive_selection() {
        for (pattern_content, variant, expected) in [
            (
                b"0 g 0 0 10 10 re f".as_slice(),
                PatternFixtureVariant::Valid,
                "uncolored tiling pattern content specifies color information",
            ),
            (
                b"/Pattern cs /Tile scn".as_slice(),
                PatternFixtureVariant::Valid,
                "recursively selects active Pattern /Tile",
            ),
            (
                b"/RelativeColorimetric ri".as_slice(),
                PatternFixtureVariant::Valid,
                "uncolored tiling pattern content specifies color information",
            ),
            (
                b"/Shade sh".as_slice(),
                PatternFixtureVariant::WithShadingResource,
                "uncolored tiling pattern content specifies color information",
            ),
        ] {
            let candidate = page_with_tiling_pattern(
                b"/PatternCmyk cs 0 0 0 1 /Tile scn",
                pattern_content,
                Some(1),
                Some(2),
                true,
                None,
                variant,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected)),
                "{:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn uncolored_tiling_pattern_allows_only_image_masks() {
        for (image_mask, variant, expected_finding) in [
            (true, PatternFixtureVariant::Valid, false),
            (
                true,
                PatternFixtureVariant::ImageWithoutBitsPerComponent,
                false,
            ),
            (false, PatternFixtureVariant::Valid, true),
            (
                true,
                PatternFixtureVariant::ImageWithInvalidBitsPerComponent,
                true,
            ),
            (true, PatternFixtureVariant::ImageWithColorSpace, true),
            (true, PatternFixtureVariant::ImageWithMask, true),
            (true, PatternFixtureVariant::ImageWithSoftMask, true),
            (true, PatternFixtureVariant::ImageWithInlineAliases, true),
        ] {
            let candidate = page_with_tiling_pattern(
                b"/PatternCmyk cs 0 0 0 1 /Tile scn",
                b"/Picture Do",
                Some(1),
                Some(2),
                true,
                Some(image_mask),
                variant,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert_eq!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains("valid image mask")),
                expected_finding,
                "{variant:?}: {:?}",
                audit.findings()
            );
        }

        for (image_mask, bits_per_component, expected_finding) in
            [(true, 1, false), (false, 1, true), (true, 2, true)]
        {
            let candidate = page_with_uncolored_pattern_form_image(image_mask, bits_per_component);
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert_eq!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains("valid image mask")),
                expected_finding,
                "{:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn uncolored_tiling_pattern_inline_images_must_be_valid_masks() {
        for (pattern_content, expected_finding) in [
            (b"BI /W 1 /H 1 /IM true ID \x00 EI".as_slice(), false),
            (b"BI /W 1 /H 1 /IM true /BPC 1 ID \x00 EI".as_slice(), false),
            (b"BI /W 1 /H 1 /IM false /BPC 1 ID \x00 EI".as_slice(), true),
            (b"BI /W 1 /H 1 /IM true /BPC 2 ID \x00 EI".as_slice(), true),
            (
                b"BI /W 1 /H 1 /IM true /CS /DeviceGray ID \x00 EI".as_slice(),
                true,
            ),
            (
                b"BI /W 1 /H 1 /IM true /Mask null ID \x00 EI".as_slice(),
                true,
            ),
            (
                b"BI /W 1 /H 1 /IM true /SMask null ID \x00 EI".as_slice(),
                true,
            ),
        ] {
            let candidate = page_with_tiling_pattern(
                b"/PatternCmyk cs 0 0 0 1 /Tile scn",
                pattern_content,
                Some(1),
                Some(2),
                true,
                None,
                PatternFixtureVariant::Valid,
            );
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert_eq!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains("valid image mask")),
                expected_finding,
                "{:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn uncolored_tiling_pattern_rejects_color_related_ext_g_state_entries() {
        let candidate = page_with_tiling_pattern(
            b"/PatternCmyk cs 0 0 0 1 /Tile scn",
            b"/State gs 0 0 10 10 re f",
            Some(1),
            Some(2),
            true,
            None,
            PatternFixtureVariant::ExtGStateWithTransfer,
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        for key in ["TR", "TR2", "BG", "BG2", "UCR", "UCR2", "HT"] {
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(&format!("/{key}"))),
                "{key}: {:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn shading_pattern_requires_a_non_pattern_shading_color_space() {
        for (color_space, shading_is_dictionary, alias_to_pattern, expected) in [
            (Some(b"DeviceCMYK".as_slice()), true, false, None),
            (
                Some(b"Pattern".as_slice()),
                true,
                false,
                Some("cannot use a Pattern color space"),
            ),
            (
                Some(b"ShadingAlias".as_slice()),
                true,
                true,
                Some("cannot use a Pattern color space"),
            ),
            (None, true, false, Some("has no color space")),
            (
                Some(b"DeviceCMYK".as_slice()),
                false,
                false,
                Some("unreadable shading dictionary"),
            ),
        ] {
            let candidate =
                page_with_shading_pattern(color_space, shading_is_dictionary, alias_to_pattern);
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            match expected {
                Some(expected) => assert!(
                    audit
                        .findings()
                        .iter()
                        .any(|finding| finding.contains(expected)),
                    "{:?}",
                    audit.findings()
                ),
                None => assert!(audit.findings().is_empty(), "{:?}", audit.findings()),
            }
        }
    }

    #[test]
    fn nested_pattern_base_color_spaces_obey_the_reference_depth_limit() {
        let document =
            PdfDocument::from_bytes(page_with_missing_pattern()).expect("fixture is parseable");
        let resources = Object::Dictionary(std::collections::HashMap::new());
        let mut color_space = Object::Name("DeviceCMYK".to_owned());
        for _ in 0..=MAX_REFERENCE_DEPTH {
            color_space = Object::Array(vec![Object::Name("Pattern".to_owned()), color_space]);
        }
        let mut context = AuditContext::default();

        assert!(matches!(
            context.inspect_color_space(&document, &resources, &color_space, 1, "test", 0),
            Err(super::ValidationError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn valid_indexed_string_and_stream_lookups_pass() {
        for lookup in [
            IndexedLookup::String(&[0, 0, 0, 0, 255, 128, 0, 64]),
            IndexedLookup::Stream(&[0, 0, 0, 0, 255, 128, 0, 64]),
        ] {
            let candidate = page_with_indexed(IndexedHival::Integer(1), lookup);
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(audit.findings().is_empty(), "{:?}", audit.findings());
        }
    }

    #[test]
    fn indexed_content_operators_accept_values_that_pdf_normalizes() {
        let candidate = page_with_indexed_content(
            b"/PageIndexed cs -1 sc 0.5 scn 2 sc /PageIndexed CS -1 SC 0.5 SCN 2 SC 10 10 20 20 re f",
            None,
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty(), "{:?}", audit.findings());
    }

    #[test]
    fn indexed_content_operators_require_one_numeric_component() {
        for content in [
            b"/PageIndexed cs sc".as_slice(),
            b"/PageIndexed cs 0 1 sc".as_slice(),
            b"/PageIndexed cs scn".as_slice(),
            b"/PageIndexed cs 0 1 scn".as_slice(),
            b"/PageIndexed CS SC".as_slice(),
            b"/PageIndexed CS 0 1 SC".as_slice(),
            b"/PageIndexed CS SCN".as_slice(),
            b"/PageIndexed CS 0 1 SCN".as_slice(),
        ] {
            let candidate = page_with_indexed_content(content, None);
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding
                        .contains("must provide exactly one finite numeric component")),
                "{:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn indexed_fill_and_stroke_state_are_independent() {
        let candidate = page_with_indexed_content(
            b"/PageIndexed cs 0 0 0 0 K 0 0 0 0 SC /PageIndexed CS 0 0 0 0 k 0 0 0 0 sc",
            None,
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty(), "{:?}", audit.findings());
    }

    #[test]
    fn indexed_color_state_is_restored_after_save_and_restore() {
        let candidate = page_with_indexed_content(b"/PageIndexed cs q 0 0 0 0 k Q 0 1 sc", None);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit.findings().iter().any(
                |finding| finding.contains("must provide exactly one finite numeric component")
            ),
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn unmatched_restore_cannot_clear_indexed_color_state() {
        let candidate = page_with_indexed_content(b"/PageIndexed cs Q 0 1 sc", None);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("Q without a matching q")),
            "{:?}",
            audit.findings()
        );
        assert!(
            audit.findings().iter().any(
                |finding| finding.contains("must provide exactly one finite numeric component")
            ),
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn unmatched_graphics_state_save_is_rejected() {
        let candidate = page_with_indexed_content(b"q /PageIndexed cs 0 sc", None);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("unmatched q graphics-state save")),
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn form_xobject_inherits_indexed_color_state_without_leaking_changes() {
        let candidate = page_with_indexed_content(
            b"/PageIndexed cs /InheritedForm Do 0 1 sc",
            Some(b"0 1 scn 0 0 0 0 k"),
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert_eq!(
            audit
                .findings()
                .iter()
                .filter(
                    |finding| finding.contains("must provide exactly one finite numeric component")
                )
                .count(),
            2,
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn indexed_color_state_rejects_pattern_name_operands() {
        let candidate =
            page_with_indexed_content(b"/PageIndexed cs 0 /UnexpectedPattern scn", None);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("cannot select pattern /UnexpectedPattern")),
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn indexed_selection_cannot_chain_color_space_resource_names() {
        let candidate =
            page_with_invalid_indexed_selection_alias(b"/PageAlias cs 0 sc 10 10 20 20 re f");
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("aliases non-device color space /PageIndexed")),
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn valid_indexed_single_flate_filter_array_passes() {
        let lookup = [0, 0, 0, 0, 255, 128, 0, 64];
        let compressed = deflate(&lookup);
        let candidate = page_with_indexed(
            IndexedHival::Integer(1),
            IndexedLookup::FlateArrayStream(&compressed),
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty(), "{:?}", audit.findings());
    }

    #[test]
    fn valid_indexed_noop_flate_decode_parameters_pass() {
        let lookup = [0, 0, 0, 0, 255, 128, 0, 64];
        let compressed = deflate(&lookup);
        for lookup in [
            IndexedLookup::FlateArrayNullParametersStream(&compressed),
            IndexedLookup::FlatePredictorStream(&compressed, 1),
        ] {
            let candidate = page_with_indexed(IndexedHival::Integer(1), lookup);
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(audit.findings().is_empty(), "{:?}", audit.findings());
        }
    }

    #[test]
    fn indexed_nondefault_flate_predictor_fails_closed() {
        let lookup = [0, 0, 0, 0, 255, 128, 0, 64];
        let compressed = deflate(&lookup);
        let candidate = page_with_indexed(
            IndexedHival::Integer(1),
            IndexedLookup::FlatePredictorStream(&compressed, 2),
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit.findings().iter().any(|finding| {
                finding.contains("Indexed lookup") && finding.contains("decoded length is None")
            }),
            "{:?}",
            audit.findings()
        );
    }

    #[test]
    fn valid_indexed_named_base_alias_passes() {
        let candidate = page_with_indexed_base(
            IndexedHival::Integer(1),
            IndexedLookup::String(&[0, 0, 0, 0, 255, 128, 0, 64]),
            IndexedBase::NamedDeviceCmyk,
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty(), "{:?}", audit.findings());
    }

    #[test]
    fn valid_indexed_devicen_base_uses_the_colorant_count() {
        let candidate = page_with_indexed_base(
            IndexedHival::Integer(1),
            IndexedLookup::String(&[0, 0, 255, 128]),
            IndexedBase::DeviceN,
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty(), "{:?}", audit.findings());
    }

    #[test]
    fn indexed_base_cannot_shadow_the_reserved_pattern_name() {
        let candidate = page_with_indexed_base(
            IndexedHival::Integer(1),
            IndexedLookup::String(&[0, 0, 0, 0, 255, 128, 0, 64]),
            IndexedBase::ShadowedPattern,
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("no provable component count"))
        );
    }

    #[test]
    fn indexed_hival_must_be_an_integer_from_zero_through_255() {
        for hival in [
            IndexedHival::Integer(-1),
            IndexedHival::Real(1.5),
            IndexedHival::Integer(256),
        ] {
            let candidate =
                page_with_indexed(hival, IndexedLookup::String(&[0, 0, 0, 0, 255, 128, 0, 64]));
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains("hival"))
            );
        }
    }

    #[test]
    fn indexed_lookup_requires_exact_decoded_length_and_readable_storage() {
        for lookup in [
            IndexedLookup::String(&[0; 7]),
            IndexedLookup::String(&[0; 9]),
            IndexedLookup::MalformedFlateStream(b"not deflate data"),
            IndexedLookup::Integer,
        ] {
            let candidate = page_with_indexed(IndexedHival::Integer(1), lookup);
            let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains("exactly 8 bytes"))
            );
        }
    }

    #[test]
    fn indexed_flate_lookup_decoding_is_bounded() {
        let expanded = vec![0; MAX_INDEXED_LOOKUP_BYTES + 1];
        let compressed = deflate(&expanded);
        let candidate = page_with_indexed(
            IndexedHival::Integer(1),
            IndexedLookup::MalformedFlateStream(&compressed),
        );
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("exactly 8 bytes"))
        );
    }

    #[test]
    fn repeated_indexed_stream_selection_reuses_the_cached_length() {
        let candidate = page_with_indexed(
            IndexedHival::Integer(1),
            IndexedLookup::Stream(&[0, 0, 0, 0, 255, 128, 0, 64]),
        );
        let document = PdfDocument::from_bytes(candidate).expect("fixture is parseable");
        let lookup = Object::Reference(ObjectRef { id: 5, r#gen: 0 });
        let mut context = AuditContext::default();

        assert_eq!(
            context
                .indexed_lookup_length(&document, &lookup)
                .expect("first lookup completes"),
            Some(8)
        );
        assert_eq!(
            context
                .indexed_lookup_length(&document, &lookup)
                .expect("cached lookup completes"),
            Some(8)
        );
        assert_eq!(context.indexed_lookup_lengths.len(), 1);
    }

    #[test]
    fn repeated_spot_color_with_the_same_alternate_passes() {
        let candidate = page_with_repeated_spot(false);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty());
    }

    #[test]
    fn repeated_spot_color_with_conflicting_alternates_is_rejected() {
        let candidate = page_with_repeated_spot(true);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("conflicting alternate color spaces"))
        );
    }

    #[test]
    fn repeated_lab_spot_with_conflicting_parameters_is_rejected() {
        let candidate = page_with_repeated_lab_spot(true);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("conflicting alternate color spaces"))
        );
    }

    #[test]
    fn repeated_calrgb_spot_with_the_same_parameters_passes() {
        let candidate = page_with_repeated_calrgb_spot(false);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty());
    }

    #[test]
    fn repeated_calrgb_spot_with_conflicting_parameters_is_rejected() {
        let candidate = page_with_repeated_calrgb_spot(true);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("conflicting alternate color spaces"))
        );
    }

    #[test]
    fn calgray_identity_normalizes_defaults_and_signed_zero() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let implicit = Object::Array(vec![
            Object::Name("CalGray".to_owned()),
            Object::Dictionary(std::collections::HashMap::from([(
                "WhitePoint".to_owned(),
                numbers(&[0.9505, 1.0, 1.089]),
            )])),
        ]);
        let explicit = Object::Array(vec![
            Object::Name("CalGray".to_owned()),
            Object::Dictionary(std::collections::HashMap::from([
                ("WhitePoint".to_owned(), numbers(&[0.9505, 1.0, 1.089])),
                ("BlackPoint".to_owned(), numbers(&[-0.0, 0.0, -0.0])),
                ("Gamma".to_owned(), Object::Real(1.0)),
            ])),
        ]);
        let different = Object::Array(vec![
            Object::Name("CalGray".to_owned()),
            Object::Dictionary(std::collections::HashMap::from([
                ("WhitePoint".to_owned(), numbers(&[0.9505, 1.0, 1.089])),
                ("Gamma".to_owned(), Object::Real(1.2)),
            ])),
        ]);
        let identity = |color_space: &Object| {
            cal_alternate_identity(
                &document,
                "CalGray",
                color_space.as_array().expect("fixture is an array"),
            )
            .expect("identity audit completes")
            .expect("valid CalGray has an identity")
        };

        assert_eq!(identity(&implicit), identity(&explicit));
        assert_ne!(identity(&implicit), identity(&different));
    }

    #[test]
    fn lab_identity_normalizes_default_and_signed_zero_black_points() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let make_lab = |black_point: Option<Object>| {
            let mut parameters = std::collections::HashMap::from([(
                "WhitePoint".to_owned(),
                numbers(&[0.9505, 1.0, 1.089]),
            )]);
            if let Some(black_point) = black_point {
                parameters.insert("BlackPoint".to_owned(), black_point);
            }
            Object::Array(vec![
                Object::Name("Lab".to_owned()),
                Object::Dictionary(parameters),
            ])
        };
        let implicit = make_lab(None);
        let explicit = make_lab(Some(numbers(&[-0.0, 0.0, -0.0])));
        let identity = |color_space: &Object| {
            lab_alternate_identity(
                &document,
                color_space.as_array().expect("fixture is an array"),
            )
            .expect("identity audit completes")
            .expect("valid Lab has an identity")
        };

        assert_eq!(identity(&implicit), identity(&explicit));
    }

    #[test]
    fn devicen_component_count_uses_its_colorant_names() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let resources = Object::Dictionary(std::collections::HashMap::new());
        let color_space = Object::Array(vec![
            Object::Name("DeviceN".to_owned()),
            Object::Array(vec![
                Object::Name("Orange".to_owned()),
                Object::Name("Green".to_owned()),
            ]),
            Object::Name("DeviceCMYK".to_owned()),
            Object::Null,
        ]);

        assert_eq!(
            color_space_component_count(&document, &resources, &color_space, 0)
                .expect("component count completes"),
            Some(2)
        );
    }

    #[test]
    fn unknown_active_color_space_type_is_rejected() {
        let mut candidate = page_with_lab([0.9505, 1.0, 1.089]);
        replace_first_equal_length(&mut candidate, b"/Lab", b"/Bad");
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("unsupported or malformed"))
        );
    }

    #[test]
    fn negative_exponential_tint_is_valid_when_domain_excludes_zero() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let function = Object::Dictionary(std::collections::HashMap::from([
            ("FunctionType".to_owned(), Object::Integer(2)),
            ("Domain".to_owned(), numbers(&[1.0, 2.0])),
            ("C0".to_owned(), numbers(&[0.0, 0.0, 0.0, 0.0])),
            ("C1".to_owned(), numbers(&[1.0, 0.5, 0.0, 0.0])),
            ("N".to_owned(), Object::Real(-1.0)),
        ]));

        assert!(
            tint_function_is_readable(&document, &function, 1, 4)
                .expect("function audit completes")
        );
    }

    #[test]
    fn zero_exponential_tint_is_valid() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let function = Object::Dictionary(std::collections::HashMap::from([
            ("FunctionType".to_owned(), Object::Integer(2)),
            ("Domain".to_owned(), numbers(&[0.0, 1.0])),
            ("N".to_owned(), Object::Real(0.0)),
        ]));

        assert!(
            tint_function_is_readable(&document, &function, 1, 1)
                .expect("function audit completes")
        );
    }

    #[test]
    fn reversed_tint_range_is_rejected() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let function = Object::Dictionary(std::collections::HashMap::from([
            ("FunctionType".to_owned(), Object::Integer(2)),
            ("Domain".to_owned(), numbers(&[0.0, 1.0])),
            ("C0".to_owned(), numbers(&[0.0, 0.0, 0.0, 0.0])),
            ("C1".to_owned(), numbers(&[1.0, 0.5, 0.0, 0.0])),
            ("N".to_owned(), Object::Real(1.0)),
            (
                "Range".to_owned(),
                numbers(&[1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]),
            ),
        ]));

        assert!(
            !tint_function_is_readable(&document, &function, 1, 4)
                .expect("function audit completes")
        );
    }

    #[test]
    fn default_one_component_tint_vectors_do_not_satisfy_cmyk_output() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let function = Object::Dictionary(std::collections::HashMap::from([
            ("FunctionType".to_owned(), Object::Integer(2)),
            ("Domain".to_owned(), numbers(&[0.0, 1.0])),
            ("N".to_owned(), Object::Real(1.0)),
        ]));

        assert!(
            !tint_function_is_readable(&document, &function, 1, 4)
                .expect("function audit completes")
        );
    }

    #[test]
    fn stitching_tint_rejects_a_structurally_invalid_child() {
        let document = PdfDocument::from_bytes(page_with_lab([0.9505, 1.0, 1.089]))
            .expect("fixture is parseable");
        let invalid_child = Object::Dictionary(std::collections::HashMap::from([
            ("FunctionType".to_owned(), Object::Integer(2)),
            ("Domain".to_owned(), numbers(&[0.0, 1.0])),
        ]));
        let function = Object::Dictionary(std::collections::HashMap::from([
            ("FunctionType".to_owned(), Object::Integer(3)),
            ("Domain".to_owned(), numbers(&[0.0, 1.0])),
            ("Functions".to_owned(), Object::Array(vec![invalid_child])),
            ("Bounds".to_owned(), Object::Array(Vec::new())),
            ("Encode".to_owned(), numbers(&[0.0, 1.0])),
        ]));

        assert!(
            !tint_function_is_readable(&document, &function, 1, 4)
                .expect("function audit completes")
        );
    }

    #[test]
    fn distinct_spot_colorant_tracking_is_bounded() {
        let mut context = AuditContext::default();
        for index in 0..MAX_DISTINCT_SPOT_COLORANTS {
            context
                .record_spot_alternate(
                    format!("Separation:/Spot{index}"),
                    "DeviceCMYK".to_owned(),
                    1,
                    "test",
                )
                .expect("entries inside the bound are accepted");
        }

        assert!(matches!(
            context.record_spot_alternate(
                "Separation:/OneTooMany".to_owned(),
                "DeviceCMYK".to_owned(),
                1,
                "test",
            ),
            Err(super::ValidationError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn device_rgb_without_a_default_conflicts_with_cmyk_output_intent() {
        let candidate = page_with_cmyk_intent_and_device_rgb(false);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("DeviceRGB") && finding.contains("CMYK"))
        );
    }

    #[test]
    fn default_rgb_icc_override_is_managed_under_cmyk_output_intent() {
        let candidate = page_with_cmyk_intent_and_device_rgb(true);
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(audit.findings().is_empty());
        assert_eq!(audit.profiles_checked(), 1);
    }

    #[test]
    fn malformed_profile_in_an_active_image_soft_mask_is_rejected() {
        let candidate = page_with_profiled_image_soft_mask(b"not an ICC profile");
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("image /Picture soft mask"))
        );
    }

    #[test]
    fn malformed_profile_in_an_applied_soft_mask_group_is_rejected() {
        let candidate = page_with_profiled_soft_mask_group(b"not an ICC profile");
        let audit = audit_active_icc_profiles(&candidate).expect("fixture is parseable");

        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("soft-mask group"))
        );
    }
}
