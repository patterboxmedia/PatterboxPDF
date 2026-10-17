//! Prepare and export one source glyph through the public native boundary.
//! Arguments: `SOURCE GID TEXT NEW_OUTPUT used|unused [PROFILE]`
//! [--face-index N] [--font-facts FILE.json]. `--profile PROFILE` is also accepted.
//! Facts bind `source_sha256`, `face_index` and either or both optional fields:
//! `accented_source_glyph_ids` and `cap_height_pdf_units`.
//! Their semantic truth is an authored input; source and transport are checked.
use print_core::finished::{
    AffineTransform, FinishedContent, FinishedDocument, FinishedItem, FinishedOutputMode,
    FinishedPage, FinishedPageBoxes, FinishedPdfOptions, FinishedResources, FontResource, Paint,
    PdfBox, PositionedGlyph, PositionedGlyphRun,
};
use print_core::{
    FinishedPdfWriter, PdfCandidateMetadata, PreparedPublicationFont, TrustedFontBoundary,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;

fn hex(bytes: [u8; 32]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut text, value| {
            write!(text, "{value:02x}").expect("writing to a String");
            text
        })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FontFacts {
    source_sha256: String,
    face_index: u32,
    accented_source_glyph_ids: Option<Vec<u16>>,
    cap_height_pdf_units: Option<f32>,
}

#[derive(Default)]
struct FontOptions {
    face_index: Option<u32>,
    profile: Option<PathBuf>,
    font_facts: Option<PathBuf>,
}

fn font_options(mut args: impl Iterator<Item = OsString>) -> Result<FontOptions, Box<dyn Error>> {
    let mut options = FontOptions::default();
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--face-index") if options.face_index.is_none() => {
                options.face_index = Some(
                    args.next()
                        .ok_or("missing face index")?
                        .to_str()
                        .ok_or("nontext face index")?
                        .parse()?,
                );
            }
            Some("--font-facts" | "--ascent-facts") if options.font_facts.is_none() => {
                options.font_facts =
                    Some(PathBuf::from(args.next().ok_or("missing font facts file")?));
            }
            Some("--profile") if options.profile.is_none() => {
                options.profile = Some(PathBuf::from(args.next().ok_or("missing profile path")?));
            }
            Some(value) if !value.starts_with("--") && options.profile.is_none() => {
                options.profile = Some(PathBuf::from(flag));
            }
            _ => return Err("unknown or repeated font option".into()),
        }
    }
    Ok(options)
}

fn prepare_font(
    bytes: &[u8],
    face: u32,
    gid: u16,
    facts: Option<&FontFacts>,
) -> Result<PreparedPublicationFont, Box<dyn Error>> {
    let boundary = TrustedFontBoundary::resolve_packaged();
    let prepared = if let Some(facts) = facts {
        if facts.face_index != face {
            return Err("font facts identify a different source face".into());
        }
        if facts.accented_source_glyph_ids.is_none() && facts.cap_height_pdf_units.is_none() {
            return Err("font facts require at least one authored fact field".into());
        }
        boundary.prepare_publication_with_font_facts(
            bytes,
            face,
            &[gid],
            facts.accented_source_glyph_ids.as_deref(),
            facts.cap_height_pdf_units,
        )
    } else {
        boundary.prepare_publication(bytes, face, &[gid])
    }
    .map_err(|error| format!("font preparation: {error:?}"))?;
    if let Some(facts) = facts
        && facts.source_sha256 != hex(prepared.source_sha256())
    {
        return Err("font facts identify different source font bytes".into());
    }
    Ok(prepared)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let source = PathBuf::from(args.next().ok_or("source font missing")?);
    let gid: u16 = args
        .next()
        .ok_or("source glyph missing")?
        .to_str()
        .ok_or("nontext glyph")?
        .parse()?;
    let unicode = args
        .next()
        .ok_or("Unicode mapping missing")?
        .into_string()
        .map_err(|_| "nontext Unicode mapping")?;
    let output = PathBuf::from(args.next().ok_or("output directory missing")?);
    let usage = args.next().ok_or("usage missing")?;
    if usage != "used" && usage != "unused" {
        return Err("unknown usage".into());
    }
    let options = font_options(args)?;
    let face_index = options.face_index.unwrap_or(0);
    let profile = options.profile.map(std::fs::read).transpose()?;
    let facts: Option<FontFacts> = options
        .font_facts
        .map(|path| -> Result<FontFacts, Box<dyn Error>> {
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        })
        .transpose()?;
    std::fs::create_dir(&output)?;
    let source_bytes = std::fs::read(source)?;
    let prepared = prepare_font(&source_bytes, face_index, gid, facts.as_ref())?;
    std::fs::write(output.join("prepared.font"), prepared.prepared_bytes())?;
    let report = preparation_report(&prepared);
    std::fs::write(output.join("prepared.json"), report)?;
    let cid = prepared
        .remapped_glyphs()
        .iter()
        .find(|entry| entry.source_glyph_id() == gid)
        .ok_or("source glyph mapping missing")?
        .cid();
    let font = FontResource::from_prepared(
        "font".into(),
        prepared,
        BTreeMap::from([(cid, unicode.clone())]),
    )?;
    let area = PdfBox::new(0., 0., 612., 792.);
    let items = if usage == "used" {
        vec![FinishedItem::new(FinishedContent::Glyphs(
            PositionedGlyphRun {
                font: "font".into(),
                font_size: 24.,
                glyphs: vec![PositionedGlyph {
                    cid,
                    matrix: AffineTransform([1., 0., 0., 1., 30., 50.]),
                }],
                actual_text: unicode,
                paint: Paint::cmyk([0., 0., 0., 1.], 1.),
            },
        ))]
    } else {
        vec![]
    };
    let document = FinishedDocument {
        pages: vec![FinishedPage {
            boxes: FinishedPageBoxes {
                media: area,
                bleed: area,
                trim: area,
            },
            items,
        }],
        resources: FinishedResources {
            fonts: vec![font],
            images: vec![],
        },
    };
    let mut modes = vec![("ordinary", FinishedOutputMode::Ordinary)];
    if let Some(profile) = &profile {
        modes.push((
            "print",
            FinishedOutputMode::PdfX4 {
                output_profile: profile,
            },
        ));
    }
    for (name, mode) in modes {
        let options = FinishedPdfOptions {
            metadata: PdfCandidateMetadata::at_utc("Accepted font input", "2026-10-16T22:00:00Z")
                .ok_or("invalid metadata")?,
            mode,
        };
        let pdf = FinishedPdfWriter::write(&document, &options)?;
        std::fs::write(output.join(format!("{name}.pdf")), pdf.bytes())?;
    }
    Ok(())
}

fn preparation_report(prepared: &PreparedPublicationFont) -> String {
    let metrics = prepared.pdf_metrics();
    let mappings = prepared
        .remapped_glyphs()
        .iter()
        .map(|entry| {
            format!(
                "{{\"source_gid\":{},\"cid\":{}}}",
                entry.source_glyph_id(),
                entry.cid()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "{{\n\"schema\":\"patterboxpdf-prepared-font-input/v1\",\n\"face_index\":{},\n\"source_sha256\":\"{}\",\n\"prepared_sha256\":\"{}\",\n",
            "\"worker_sha256\":\"{}\",\n\"engine_sha256\":\"{}\",\n",
            "\"rules_sha256\":\"{}\",\n\"rights_sha256\":\"{}\",\n",
            "\"requires_ascent_facts\":{},\n\"ascent_facts_sha256\":{},\n",
            "\"requires_cap_height_fact\":{},\n\"font_facts_sha256\":{},\n",
            "\"outline_kind\":\"{:?}\",\n\"preparation_kind\":\"{:?}\",\n",
            "\"embedded_glyph_count\":{},\n\"used_glyph_count\":{},\n",
            "\"metrics\":{{\"bbox\":{:?},\"ascent\":{},\"descent\":{},",
            "\"cap_height\":{},\"italic_angle\":{},\"stem_v\":{},\"widths\":{:?}}},\n",
            "\"remapping\":[{}]\n}}\n"
        ),
        prepared.face_index(),
        hex(prepared.source_sha256()),
        hex(prepared.prepared_sha256()),
        hex(prepared.worker_executable_sha256()),
        hex(prepared.engine_identity_sha256()),
        hex(prepared.rules_identity_sha256()),
        hex(prepared.rights_identity_sha256()),
        prepared.requires_ascent_facts(),
        prepared
            .ascent_facts_sha256()
            .map_or_else(|| "null".into(), |hash| format!("\"{}\"", hex(hash))),
        prepared.requires_cap_height_fact(),
        prepared
            .font_facts_sha256()
            .map_or_else(|| "null".into(), |hash| format!("\"{}\"", hex(hash))),
        prepared.outline_kind(),
        prepared.preparation_kind(),
        prepared.embedded_glyph_count(),
        prepared.used_glyph_count(),
        metrics.bbox(),
        metrics.ascent(),
        metrics.descent(),
        metrics
            .cap_height()
            .map_or_else(|| "null".into(), |value| value.to_string()),
        metrics.italic_angle(),
        metrics.stem_v(),
        metrics.widths(),
        mappings
    )
}
