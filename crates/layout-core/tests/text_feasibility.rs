use layout_core::text::{
    HorizontalAlignment, Millipoints, SourceRange, StoryParagraphStyle, StorySpan, StoryStyle,
    TextLayoutEngine, TextRect, TextRegion, TextStory,
};

fn story(text: &str) -> TextStory {
    TextStory {
        text: text.to_owned(),
        base_style: StoryStyle::new(1, "Arial, sans-serif"),
        spans: Vec::new(),
    }
}

fn region(width: i64, height: i64) -> TextRegion {
    TextRegion {
        bounds: TextRect::new(0, 0, width, height).unwrap(),
        column_count: 1,
        column_gap: Millipoints::new(0),
        exclusions: Vec::new(),
    }
}

#[test]
fn mixed_styles_retain_source_and_style_identity() {
    let mut source = story("Ordinary words become bold and italic.");
    let start = source.text.find("bold").unwrap();
    let end = start + "bold".len();
    let mut emphasis = StoryStyle::new(7, "Arial, sans-serif");
    emphasis.font_weight = 700;
    emphasis.italic = true;
    source.spans.push(StorySpan {
        source: SourceRange { start, end },
        style: emphasis,
    });

    let result = TextLayoutEngine::new()
        .resolve(&source, &region(180_000, 100_000))
        .unwrap();

    assert!(result.overset.is_none());
    assert!(
        result
            .lines
            .iter()
            .flat_map(|line| &line.fragments)
            .flat_map(|fragment| &fragment.glyph_runs)
            .any(|run| run.style_id == 7 && run.source.start >= start && run.source.end <= end)
    );
    assert!(!result.fonts.is_empty());
    assert!(result.fonts.iter().all(|font| font.sha256 != [0; 32]));
}

#[test]
fn two_columns_continue_in_source_order() {
    let source = story(&"The quick brown fox jumps over the lazy dog. ".repeat(24));
    let region = TextRegion {
        bounds: TextRect::new(0, 0, 180_000, 150_000).unwrap(),
        column_count: 2,
        column_gap: Millipoints::new(12_000),
        exclusions: Vec::new(),
    };

    let result = TextLayoutEngine::new().resolve(&source, &region).unwrap();

    assert!(result.lines.iter().any(|line| line.column_index == 0));
    assert!(result.lines.iter().any(|line| line.column_index == 1));
    assert!(
        result
            .lines
            .windows(2)
            .all(|pair| pair[0].source.end <= pair[1].source.end)
    );
}

#[test]
fn rectangular_exclusion_changes_early_line_width() {
    let source = story(&"Wrap this text around the illustration. ".repeat(12));
    let mut wrapped_region = region(180_000, 180_000);
    wrapped_region
        .exclusions
        .push(TextRect::new(0, 0, 70_000, 60_000).unwrap());

    let result = TextLayoutEngine::new()
        .resolve(&source, &wrapped_region)
        .unwrap();

    let first = &result.lines[0].fragments[0];
    assert_eq!(first.x.value(), 70_000);
    assert_eq!(first.width.value(), 110_000);
    assert!(
        result
            .lines
            .iter()
            .skip(1)
            .any(|line| line.fragments[0].width.value() == 180_000)
    );
}

#[test]
fn later_display_text_does_not_block_earlier_body_lines_above_an_exclusion() {
    let mut source = story(
        "Small body copy should fill the open space above the illustration before the display words appear. BIG FINISH",
    );
    let display_start = source.text.find("BIG FINISH").unwrap();
    let mut display = StoryStyle::new(9, "Arial, sans-serif");
    display.font_size = Millipoints::new(72_000);
    source.spans.push(StorySpan {
        source: SourceRange {
            start: display_start,
            end: source.text.len(),
        },
        style: display,
    });
    let mut wrapped_region = region(180_000, 180_000);
    wrapped_region
        .exclusions
        .push(TextRect::new(0, 30_000, 180_000, 45_000).unwrap());

    let result = TextLayoutEngine::new()
        .resolve(&source, &wrapped_region)
        .unwrap();

    let lines_above_exclusion = result
        .lines
        .iter()
        .filter(|line| line.y.value().saturating_add(line.height.value()) <= 30_000)
        .count();
    assert!(
        lines_above_exclusion >= 1,
        "body copy should use the space above the exclusion"
    );
    assert!(
        result.lines.first().is_some_and(|line| line.source.end > 0),
        "the later display style must not eject the story from the column"
    );
}

#[test]
fn overset_reports_exact_utf8_byte_tail() {
    let source = story(&"Magic café — presto! ".repeat(40));
    let result = TextLayoutEngine::new()
        .resolve(&source, &region(90_000, 24_000))
        .unwrap();
    let overset = result.overset.expect("small region must overset");

    assert!(source.text.is_char_boundary(overset.source.start));
    assert_eq!(overset.source.end, source.text.len());
    assert_eq!(
        overset.source.start,
        result.lines.last().map_or(0, |line| line.source.end)
    );
}

#[test]
fn identical_inputs_canonicalize_identically_on_same_font_set() {
    let source = story("Deterministic punctuation: ‘quotes’, em—dash, café, fi.");
    let region = region(160_000, 100_000);
    let mut engine = TextLayoutEngine::new();

    let first = engine.resolve(&source, &region).unwrap();
    let second = engine.resolve(&source, &region).unwrap();

    assert_eq!(first, second);
}

#[test]
fn alignment_changes_canonical_glyph_geometry_without_renderer_reflow() {
    let source = story("Magic words");
    let region = region(180_000, 60_000);
    let mut engine = TextLayoutEngine::new();
    let left = engine
        .resolve_aligned(&source, &region, HorizontalAlignment::Left)
        .unwrap();
    let center = engine
        .resolve_aligned(&source, &region, HorizontalAlignment::Center)
        .unwrap();
    let right = engine
        .resolve_aligned(&source, &region, HorizontalAlignment::Right)
        .unwrap();
    let first_x = |layout: &layout_core::text::ResolvedTextLayout| {
        layout.lines[0].fragments[0].glyph_runs[0].glyphs[0]
            .x
            .value()
    };

    assert!(first_x(&left) < first_x(&center));
    assert!(first_x(&center) < first_x(&right));
    assert!(
        left.lines[0].fragments[0]
            .glyph_runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .any(|glyph| glyph.outline.is_some()),
        "native preview requires canonical vector outlines, not WebView shaping"
    );
}

#[test]
fn justification_moves_word_glyphs_in_canonical_geometry() {
    let source = story("one two three four five six seven eight nine ten");
    let region = region(100_000, 100_000);
    let mut engine = TextLayoutEngine::new();
    let left = engine
        .resolve_aligned(&source, &region, HorizontalAlignment::Left)
        .unwrap();
    let justified = engine
        .resolve_aligned(&source, &region, HorizontalAlignment::Justify)
        .unwrap();
    let glyph_x_at_source = |layout: &layout_core::text::ResolvedTextLayout, source_start| {
        layout.lines[0].fragments[0]
            .glyph_runs
            .iter()
            .find(|run| run.source.start == source_start)
            .and_then(|run| run.glyphs.first())
            .map(|glyph| glyph.x.value())
            .expect("word cluster on first line")
    };

    assert!(glyph_x_at_source(&justified, 4) > glyph_x_at_source(&left, 4));
}

#[test]
fn paragraph_styles_keep_global_utf8_ranges_while_changing_native_geometry() {
    let mut source = story(
        "First paragraph.\nMagic café instructions continue across several words on another line.",
    );
    let emphasis_start = source.text.find("café").unwrap();
    let mut emphasis = source.base_style.clone();
    emphasis.id = 3;
    emphasis.font_weight = 700;
    emphasis.italic = true;
    source.spans.push(StorySpan {
        source: SourceRange {
            start: emphasis_start,
            end: emphasis_start + "café".len(),
        },
        style: emphasis,
    });
    let ordinary = StoryParagraphStyle::new(HorizontalAlignment::Left, 1_200);
    let mut formatted = StoryParagraphStyle::new(HorizontalAlignment::Right, 1_800);
    formatted.start_indent = Millipoints::new(10_000);
    formatted.end_indent = Millipoints::new(5_000);
    formatted.first_line_indent = Millipoints::new(5_000);
    formatted.space_before = Millipoints::new(9_000);

    let resolved = TextLayoutEngine::new()
        .resolve_paragraphs(&source, &region(120_000, 180_000), &[ordinary, formatted])
        .unwrap();
    let second_start = source.text.find("Magic").unwrap();
    let first_paragraph_line = resolved
        .lines
        .iter()
        .find(|line| line.source.start < second_start)
        .unwrap();
    let second_lines = resolved
        .lines
        .iter()
        .filter(|line| line.source.start >= second_start)
        .collect::<Vec<_>>();

    assert!(second_lines.len() >= 2);
    assert_eq!(second_lines[0].source.start, second_start);
    assert_eq!(second_lines[0].fragments[0].x.value(), 15_000);
    assert_eq!(second_lines[1].fragments[0].x.value(), 10_000);
    assert!(second_lines[0].height > first_paragraph_line.height);
    assert!(
        second_lines[0].y.value()
            >= first_paragraph_line
                .y
                .value()
                .saturating_add(first_paragraph_line.height.value())
                .saturating_add(9_000)
    );
    assert!(
        second_lines
            .iter()
            .flat_map(|line| &line.fragments)
            .flat_map(|fragment| &fragment.glyph_runs)
            .any(|run| run.style_id == 3
                && run.source.start >= emphasis_start
                && run.source.end <= emphasis_start + "café".len())
    );
    assert!(resolved.lines.iter().all(|line| {
        source.text.is_char_boundary(line.source.start)
            && source.text.is_char_boundary(line.source.end)
    }));
}

#[test]
fn formatted_paragraphs_preserve_exact_overset_across_columns_and_wrap() {
    let source = story(&format!(
        "Intro café.\n{}",
        "Perform the effect slowly, show both hands, and pause. ".repeat(30)
    ));
    let mut second = StoryParagraphStyle::new(HorizontalAlignment::Justify, 1_300);
    second.start_indent = Millipoints::new(4_000);
    second.end_indent = Millipoints::new(3_000);
    second.first_line_indent = Millipoints::new(3_000);
    let mut wrapped_region = TextRegion {
        bounds: TextRect::new(0, 0, 150_000, 48_000).unwrap(),
        column_count: 2,
        column_gap: Millipoints::new(8_000),
        exclusions: Vec::new(),
    };
    wrapped_region
        .exclusions
        .push(TextRect::new(0, 0, 25_000, 24_000).unwrap());
    let styles = [
        StoryParagraphStyle::new(HorizontalAlignment::Left, 1_200),
        second,
    ];
    let mut engine = TextLayoutEngine::new();
    let first = engine
        .resolve_paragraphs(&source, &wrapped_region, &styles)
        .unwrap();
    let second = engine
        .resolve_paragraphs(&source, &wrapped_region, &styles)
        .unwrap();
    let overset = first.overset.as_ref().expect("small columns must overset");

    assert_eq!(first, second);
    assert_eq!(overset.source.end, source.text.len());
    assert_eq!(
        overset.source.start,
        first.lines.last().map_or(0, |line| line.source.end)
    );
    assert!(source.text.is_char_boundary(overset.source.start));
    assert!(first.lines.iter().any(|line| line.column_index == 1));
    assert!(first.lines[0].fragments[0].x.value() >= 25_000);
}

#[test]
fn fit_paragraphs_include_indents_spacing_and_alignment_in_shrinkwrap_bounds() {
    let source = story("Short\nA substantially longer second paragraph");
    let mut first = StoryParagraphStyle::new(HorizontalAlignment::Right, 1_200);
    first.start_indent = Millipoints::new(3_000);
    first.end_indent = Millipoints::new(2_000);
    first.space_after = Millipoints::new(7_000);
    let second = StoryParagraphStyle::new(HorizontalAlignment::Left, 1_500);
    let resolved = TextLayoutEngine::new()
        .resolve_fit_paragraphs(&source, &[first, second])
        .unwrap();

    assert!(resolved.layout.overset.is_none());
    assert_eq!(resolved.layout.lines.len(), 2);
    assert!(resolved.width.value() > 0);
    assert!(
        resolved.layout.lines[0].fragments[0].glyph_runs[0].glyphs[0]
            .x
            .value()
            > resolved.layout.lines[1].fragments[0].glyph_runs[0].glyphs[0]
                .x
                .value()
    );
    assert!(
        resolved.layout.lines[1].y.value()
            >= resolved.layout.lines[0]
                .y
                .value()
                .saturating_add(resolved.layout.lines[0].height.value())
                .saturating_add(7_000)
    );
    assert!(resolved.height.value() > 7_000);
}

#[test]
fn one_default_paragraph_matches_the_existing_authoritative_path() {
    let source = story("One ordinary paragraph with enough words to wrap onto another line.");
    let region = region(90_000, 100_000);
    let mut engine = TextLayoutEngine::new();
    let existing = engine
        .resolve_aligned(&source, &region, HorizontalAlignment::Center)
        .unwrap();
    let formatted = engine
        .resolve_paragraphs(
            &source,
            &region,
            &[StoryParagraphStyle::new(
                HorizontalAlignment::Center,
                source.base_style.line_height_per_thousand,
            )],
        )
        .unwrap();

    assert_eq!(formatted, existing);
}

#[test]
fn only_lf_creates_paragraphs_and_trailing_lf_keeps_the_final_empty_paragraph() {
    let source = story("First\r\nSecond\n");
    let styles = [
        StoryParagraphStyle::new(HorizontalAlignment::Left, 1_200),
        StoryParagraphStyle::new(HorizontalAlignment::Left, 1_200),
        StoryParagraphStyle::new(HorizontalAlignment::Left, 1_200),
    ];
    let resolved = TextLayoutEngine::new()
        .resolve_fit_paragraphs(&source, &styles)
        .unwrap();

    assert_eq!(resolved.layout.lines.len(), 3);
    assert_eq!(
        resolved.layout.lines[0].source,
        SourceRange { start: 0, end: 6 }
    );
    assert_eq!(
        resolved.layout.lines[1].source,
        SourceRange { start: 7, end: 13 }
    );
    assert_eq!(
        resolved.layout.lines[2].source,
        SourceRange { start: 14, end: 14 }
    );
}
