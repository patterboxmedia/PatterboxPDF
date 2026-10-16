use layout_core::{
    FontSubstitutionStatus, RequestedFontRangeBindingPreview, ResolvedGlyphRunPreview,
    ResolvedTextBoxPreview,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FontContractFailure {
    NonCanonicalBindings {
        source_start: usize,
        source_end: usize,
    },
    UnresolvedBinding {
        source_start: usize,
        source_end: usize,
    },
    MissingResolvedFont {
        font_index: usize,
    },
    IdentityMismatch {
        source_start: usize,
        source_end: usize,
    },
    Synthesis {
        source_start: usize,
        source_end: usize,
    },
}

pub(crate) fn validate_layout_font_contract(
    layout: &ResolvedTextBoxPreview,
) -> Result<(), FontContractFailure> {
    validate_canonical_bindings(&layout.font_bindings)?;
    for run in layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .filter(|run| !run.text.chars().all(char::is_whitespace))
    {
        validate_run(layout, run)?;
    }
    Ok(())
}

fn validate_canonical_bindings(
    bindings: &[RequestedFontRangeBindingPreview],
) -> Result<(), FontContractFailure> {
    for (index, binding) in bindings.iter().enumerate() {
        let invalid_range = binding.source.start > binding.source.end
            || (binding.source.start == binding.source.end && bindings.len() > 1);
        let invalid_order = index.checked_sub(1).is_some_and(|previous_index| {
            let previous = &bindings[previous_index];
            previous.source.end > binding.source.start
                || (previous.source.end == binding.source.start
                    && same_binding_facts(previous, binding))
        });
        if invalid_range || invalid_order {
            return Err(FontContractFailure::NonCanonicalBindings {
                source_start: binding.source.start,
                source_end: binding.source.end,
            });
        }
    }
    Ok(())
}

fn same_binding_facts(
    left: &RequestedFontRangeBindingPreview,
    right: &RequestedFontRangeBindingPreview,
) -> bool {
    left.requested_family == right.requested_family
        && left.requested_weight == right.requested_weight
        && left.requested_italic == right.requested_italic
        && left.exact_face == right.exact_face
        && left.substitution_status == right.substitution_status
}

fn validate_run(
    layout: &ResolvedTextBoxPreview,
    run: &ResolvedGlyphRunPreview,
) -> Result<(), FontContractFailure> {
    let binding = if run.list_marker_paragraph.is_some() || layout.font_bindings.is_empty() {
        None
    } else {
        let binding = layout
            .font_bindings
            .partition_point(|binding| binding.source.start <= run.source.start)
            .checked_sub(1)
            .and_then(|index| layout.font_bindings.get(index));
        if binding.is_none_or(|binding| run.source.end > binding.source.end) {
            return Err(FontContractFailure::NonCanonicalBindings {
                source_start: run.source.start,
                source_end: run.source.end,
            });
        }
        binding
    };
    let (source_start, source_end) = binding
        .map_or((run.source.start, run.source.end), |binding| {
            (binding.source.start, binding.source.end)
        });
    if run.requires_font_synthesis {
        return Err(FontContractFailure::Synthesis {
            source_start,
            source_end,
        });
    }
    let (status, expected) = binding.map_or_else(
        || {
            (
                layout.font_binding.substitution_status,
                layout.font_binding.exact_face.as_ref(),
            )
        },
        |binding| (binding.substitution_status, binding.exact_face.as_ref()),
    );
    if status != FontSubstitutionStatus::Exact || expected.is_none() {
        return Err(FontContractFailure::UnresolvedBinding {
            source_start,
            source_end,
        });
    }
    let actual = layout.resolved_fonts.get(run.font_index).ok_or(
        FontContractFailure::MissingResolvedFont {
            font_index: run.font_index,
        },
    )?;
    if Some(actual) != expected && !layout.permitted_fallback_fonts.contains(actual) {
        return Err(FontContractFailure::IdentityMismatch {
            source_start,
            source_end,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use document_core::ObjectId;
    use document_core::model::{PhysicalSize, TextBoxSizingMode};
    use layout_core::text::{Millipoints, SourceRange};
    use layout_core::{
        RequestedFontBindingPreview, RequestedFontRangeBindingPreview, ResolvedFontFacePreview,
        ResolvedGlyphRunPreview, ResolvedTextBoxPreview, ResolvedTextLinePreview,
        TextBoxPreviewState,
    };

    use super::{FontContractFailure, validate_layout_font_contract};
    use layout_core::FontSubstitutionStatus;

    fn face(hash: &str) -> ResolvedFontFacePreview {
        ResolvedFontFacePreview {
            sha256_hex: hash.repeat(64 / hash.len()),
            collection_index: 0,
        }
    }

    fn one_run_layout(
        status: FontSubstitutionStatus,
        expected: ResolvedFontFacePreview,
        actual: ResolvedFontFacePreview,
        synthesis: bool,
    ) -> ResolvedTextBoxPreview {
        let binding = RequestedFontBindingPreview {
            requested_family: "Test family".to_owned(),
            exact_face: Some(expected.clone()),
            substitution_status: status,
        };
        ResolvedTextBoxPreview {
            object_id: ObjectId::parse("font-contract-test").unwrap(),
            sizing_mode: TextBoxSizingMode::Fixed,
            composed_size: PhysicalSize::new(10_000, 10_000).unwrap(),
            content_bounds: layout_core::geometry::PhysicalBounds {
                x: document_core::model::Micrometres::new(0),
                y: document_core::model::Micrometres::new(0),
                width: document_core::model::Micrometres::new(10_000),
                height: document_core::model::Micrometres::new(10_000),
            },
            vertical_offset: Millipoints::new(0),
            font_binding: binding,
            font_bindings: vec![RequestedFontRangeBindingPreview {
                source: SourceRange { start: 0, end: 1 },
                requested_family: "Test family".to_owned(),
                requested_weight: 400,
                requested_italic: false,
                exact_face: Some(expected),
                substitution_status: status,
            }],
            permitted_fallback_fonts: Vec::new(),
            resolved_fonts: vec![actual],
            lines: vec![ResolvedTextLinePreview {
                source: SourceRange { start: 0, end: 1 },
                text: "A".to_owned(),
                column_index: 0,
                x: Millipoints::new(0),
                y: Millipoints::new(0),
                width: Millipoints::new(1_000),
                height: Millipoints::new(1_000),
                baseline: Millipoints::new(800),
                glyph_runs: vec![ResolvedGlyphRunPreview {
                    source: SourceRange { start: 0, end: 1 },
                    text: "A".to_owned(),
                    glyph_text: "A".to_owned(),
                    font_index: 0,
                    font_size: Millipoints::new(12_000),
                    color: document_core::model::Color {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                    underline: None,
                    requires_font_synthesis: synthesis,
                    list_marker_paragraph: None,
                    right_to_left: false,
                    glyphs: Vec::new(),
                }],
            }],
            overset: false,
            overset_source: None,
            preview_state: TextBoxPreviewState::Complete,
        }
    }

    #[test]
    fn exact_range_rejects_an_actual_face_with_another_identity() {
        let layout = one_run_layout(FontSubstitutionStatus::Exact, face("a"), face("b"), false);
        assert!(matches!(
            validate_layout_font_contract(&layout),
            Err(FontContractFailure::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn exact_range_accepts_only_an_explicitly_permitted_fallback_identity() {
        let fallback = face("b");
        let mut layout = one_run_layout(
            FontSubstitutionStatus::Exact,
            face("a"),
            fallback.clone(),
            false,
        );
        layout.permitted_fallback_fonts.push(fallback);

        assert_eq!(validate_layout_font_contract(&layout), Ok(()));
    }

    #[test]
    fn unresolved_and_synthetic_ranges_have_distinct_failures() {
        let unresolved =
            one_run_layout(FontSubstitutionStatus::Unbound, face("a"), face("a"), false);
        assert!(matches!(
            validate_layout_font_contract(&unresolved),
            Err(FontContractFailure::UnresolvedBinding { .. })
        ));
        let synthetic = one_run_layout(
            FontSubstitutionStatus::Substituted,
            face("a"),
            face("a"),
            true,
        );
        assert!(matches!(
            validate_layout_font_contract(&synthetic),
            Err(FontContractFailure::Synthesis { .. })
        ));
    }
}
