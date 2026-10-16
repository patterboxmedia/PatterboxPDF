//! Independent finished-page caller: no saved-document or layout API imports.
use print_core::finished::{
    AffineTransform, FillRule, FinishedContent, FinishedDocument, FinishedGroup, FinishedItem,
    FinishedOutputMode, FinishedPage, FinishedPageBoxes, FinishedPath, FinishedPathPaint,
    FinishedPdfOptions, FinishedResources, FontResource, ImagePlacement, ImageResource, LineCap,
    LineJoin, Paint, PathCommand, PdfBox, Point, PositionedGlyph, PositionedGlyphRun, Stroke,
};
use print_core::{FinishedPdfWriter, PdfCandidateMetadata, TrustedFontBoundary};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const TT: &[u8] = include_bytes!("../../../crates/layout-core/tests/fixtures/DejaVuSans.ttf");
const CFF: &[u8] =
    include_bytes!("../../../crates/layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");
const PROFILE: &[u8] =
    include_bytes!("../../../crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
const CREATED: &str = "2026-10-04T00:00:00Z";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("standalone client: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one explicit composition shows callers the full finished-page contract"
)]
fn run() -> Result<(), Box<dyn Error>> {
    let mut output = None;
    let mut profile = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!(
                "pdf-library-standalone-client --output NEW_DIRECTORY [--profile CMYK.icc]\nCreates ordinary.pdf and candidate.pdf with explicit repeatable metadata.\nRequires packaged font/color workers. Output is a bounded candidate awaiting independent checking."
            );
            return Ok(());
        }
        let value = args.next().ok_or("option requires a path")?;
        match flag.to_str() {
            Some("--output") if output.is_none() => output = Some(PathBuf::from(value)),
            Some("--profile") if profile.is_none() => profile = Some(PathBuf::from(value)),
            _ => return Err("unknown or repeated option; use --help".into()),
        }
    }
    let output = output.ok_or("missing --output; use --help")?;
    let profile = profile
        .map(fs::read)
        .transpose()?
        .unwrap_or_else(|| PROFILE.to_vec());
    let (tt, tt_run) = positioned_text("truetype", TT, "Standalone TrueType ffi", 28.0, 230.0)?;
    let (cff, cff_run) = positioned_text("cff", CFF, "Standalone CFF 中文", 28.0, 185.0)?;
    let image = ImageResource::from_decoded_cmyk(
        "pixels".into(),
        3,
        2,
        &[
            0, 255, 255, 0, 255, 0, 255, 0, 255, 255, 0, 0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 128, 255,
            32,
        ],
        Some(&[255, 160, 80, 32, 128, 255]),
    )?;
    let mut curve = FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
        path: FinishedPath {
            fill_rule: FillRule::NonZero,
            commands: vec![
                PathCommand::MoveTo(Point::new(0.0, 0.0)),
                PathCommand::CubicTo(
                    Point::new(20.0, 70.0),
                    Point::new(80.0, -20.0),
                    Point::new(110.0, 35.0),
                ),
                PathCommand::LineTo(Point::new(110.0, 0.0)),
                PathCommand::Close,
            ],
        },
        fill: Some(Paint::cmyk([0.9, 0.3, 0.1, 0.0], 0.6)),
        stroke: Some(Stroke {
            paint: Paint::cmyk([0.0, 0.0, 0.0, 1.0], 1.0),
            width: 2.0,
            cap: LineCap::Round,
            join: LineJoin::Round,
            miter_limit: 10.0,
            dash: vec![5.0, 2.0],
            dash_phase: 1.0,
        }),
    }));
    curve.transform = AffineTransform([1.0, 0.15, 0.2, 1.0, 32.0, 55.0]);
    let mut picture = FinishedItem::new(FinishedContent::Image(ImagePlacement {
        image: "pixels".into(),
        alpha: 1.0,
    }));
    picture.transform = AffineTransform([95.0, 0.0, 0.0, 58.0, 135.0, 70.0]);
    picture.clip = Some(rectangle(0.0, 0.0, 0.85, 1.0));
    let overlap = FinishedItem::new(FinishedContent::Group(FinishedGroup {
        bounds: PdfBox::new(0.0, 0.0, 280.0, 200.0),
        opacity: 0.65,
        items: vec![curve.clone(), picture.clone()],
    }));
    let mut shadow = FinishedItem::new(FinishedContent::ImageAlpha {
        image: "pixels".into(),
        paint: Paint::cmyk([0.0, 0.0, 0.0, 1.0], 0.5),
    });
    shadow.transform = AffineTransform([95.0, 0.0, 0.0, 58.0, 145.0, 60.0]);
    shadow.clip = Some(rectangle(0.0, 0.0, 0.85, 1.0));
    let document = FinishedDocument {
        resources: FinishedResources {
            fonts: vec![tt, cff],
            images: vec![image],
        },
        pages: vec![
            FinishedPage {
                boxes: FinishedPageBoxes {
                    media: PdfBox::new(-10.0, -10.0, 310.0, 290.0),
                    bleed: PdfBox::new(-5.0, -5.0, 305.0, 285.0),
                    trim: PdfBox::new(0.0, 0.0, 300.0, 280.0),
                },
                items: vec![tt_run, cff_run, shadow, overlap],
            },
            FinishedPage {
                boxes: FinishedPageBoxes {
                    media: PdfBox::new(0.0, 0.0, 420.0, 240.0),
                    bleed: PdfBox::new(8.0, 8.0, 412.0, 232.0),
                    trim: PdfBox::new(12.0, 12.0, 408.0, 228.0),
                },
                items: vec![curve, picture],
            },
        ],
    };
    let metadata = PdfCandidateMetadata::at_utc("Standalone finished pages", CREATED)
        .ok_or("invalid fixed timestamp")?;
    let ordinary = FinishedPdfWriter::write(
        &document,
        &FinishedPdfOptions {
            metadata: metadata.clone(),
            mode: FinishedOutputMode::Ordinary,
        },
    )?;
    let candidate = FinishedPdfWriter::write(
        &document,
        &FinishedPdfOptions {
            metadata,
            mode: FinishedOutputMode::PdfX4 {
                output_profile: &profile,
            },
        },
    )?;
    // Generate both completely before creating the output bundle. Existing output
    // is preserved; callers inspect exact bytes using the accompanying verifier.
    fs::create_dir(&output)?;
    for (name, bytes) in [
        ("ordinary.pdf", ordinary.bytes()),
        ("candidate.pdf", candidate.bytes()),
    ] {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    println!("created ordinary.pdf and candidate.pdf; pages=2; metadata_utc={CREATED}");
    Ok(())
}

fn positioned_text(
    id: &str,
    bytes: &[u8],
    text: &str,
    x: f32,
    y: f32,
) -> Result<(FontResource, FinishedItem), Box<dyn Error>> {
    // This small caller composes a fixed line using font advances. Full text
    // shaping is a caller responsibility, independent of the writer interface.
    let face = ttf_parser::Face::parse(bytes, 0)?;
    let sources = text
        .chars()
        .map(|ch| {
            face.glyph_index(ch)
                .map(|g| g.0)
                .ok_or("font lacks text glyph")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ids = sources
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let program = TrustedFontBoundary::resolve_packaged()
        .prepare_publication(bytes, 0, &ids)
        .map_err(|error| format!("font preparation rejected: {error:?}"))?;
    let mut unicode = BTreeMap::new();
    let mut glyphs = Vec::new();
    let mut cursor = x;
    for (source, ch) in sources.into_iter().zip(text.chars()) {
        let cid = program
            .remapped_glyphs()
            .iter()
            .find(|g| g.source_glyph_id() == source)
            .ok_or("missing CID")?
            .cid();
        unicode.entry(cid).or_insert_with(|| ch.to_string());
        glyphs.push(PositionedGlyph {
            cid,
            matrix: AffineTransform([1.0, 0.0, 0.0, 1.0, cursor, y]),
        });
        cursor += f32::from(
            face.glyph_hor_advance(ttf_parser::GlyphId(source))
                .unwrap_or(0),
        ) * 12.0
            / f32::from(face.units_per_em());
    }
    let resource = FontResource::from_prepared(id.into(), program, unicode)?;
    Ok((
        resource,
        FinishedItem::new(FinishedContent::Glyphs(PositionedGlyphRun {
            font: id.into(),
            font_size: 12.0,
            glyphs,
            actual_text: text.into(),
            paint: Paint::cmyk([0.0, 0.0, 0.0, 1.0], 1.0),
        })),
    ))
}

fn rectangle(x1: f32, y1: f32, x2: f32, y2: f32) -> FinishedPath {
    FinishedPath {
        fill_rule: FillRule::NonZero,
        commands: vec![
            PathCommand::MoveTo(Point::new(x1, y1)),
            PathCommand::LineTo(Point::new(x2, y1)),
            PathCommand::LineTo(Point::new(x2, y2)),
            PathCommand::LineTo(Point::new(x1, y2)),
            PathCommand::Close,
        ],
    }
}
