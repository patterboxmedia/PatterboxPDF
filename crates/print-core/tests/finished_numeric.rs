//! Public producer regressions for relationships in the actual PDF operands.
use print_core::finished::*;
use print_core::{FinishedPdfWriter, PdfCandidateMetadata};

fn export(matrix: [f32; 6]) -> Result<Vec<u8>, FinishedPdfError> {
    let bounds = PdfBox::new(0.0, 0.0, 100.0, 100.0);
    let mut item = FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
        path: FinishedPath {
            commands: vec![
                PathCommand::MoveTo(Point::new(0.0, 0.0)),
                PathCommand::LineTo(Point::new(1.0, 1.0)),
            ],
            fill_rule: FillRule::NonZero,
        },
        fill: Some(Paint::gray(0.0, 1.0)),
        stroke: None,
    }));
    item.transform = AffineTransform(matrix);
    FinishedPdfWriter::write(
        &FinishedDocument {
            pages: vec![FinishedPage {
                boxes: FinishedPageBoxes {
                    media: bounds,
                    bleed: bounds,
                    trim: bounds,
                },
                items: vec![item],
            }],
            resources: FinishedResources::default(),
        },
        &FinishedPdfOptions {
            metadata: PdfCandidateMetadata::at_utc("Numeric operands", "2026-10-04T00:00:00Z")
                .unwrap(),
            mode: FinishedOutputMode::Ordinary,
        },
    )
    .map(print_core::FinishedPdf::into_bytes)
}

fn matrix_operands(bytes: &[u8]) -> Vec<&str> {
    let line = bytes
        .split(|byte| *byte == b'\n')
        .find(|line| line.ends_with(b" cm"))
        .unwrap();
    std::str::from_utf8(line)
        .unwrap()
        .split_ascii_whitespace()
        .take(6)
        .collect()
}

#[test]
fn accepted_binary32_matrix_keeps_its_nonzero_determinant_in_encoded_decimals() {
    // These four exact binary32 values have a nonzero determinant, whereas
    // decimal .1*.9 - .3*.3 is exactly zero. Each expected number below is the
    // finite decimal expansion of the authored binary32 value.
    let bytes = export([0.1, 0.3, 0.3, 0.9, 0.0, 0.0]).unwrap();
    assert_eq!(
        matrix_operands(&bytes),
        [
            "0.100000001490116119384765625",
            "0.300000011920928955078125",
            "0.300000011920928955078125",
            "0.89999997615814208984375",
            "0",
            "0",
        ]
    );
    if let Some(directory) = std::env::var_os("PDF_LIBRARY_NUMERIC_ARTIFACTS") {
        std::fs::write(
            std::path::Path::new(&directory).join("nonzero-matrix.pdf"),
            bytes,
        )
        .unwrap();
    }
}

#[test]
fn ordinary_neighbors_and_zero_sign_keep_existing_geometry_contract() {
    let neighbor = export([0.1, 0.3, 0.3, f32::from_bits(0x3f66_6668), -0.0, 0.0]).unwrap();
    assert_eq!(
        matrix_operands(&neighbor),
        ["0.1", "0.3", "0.3", "0.9000001", "0", "0"]
    );
    // Source/encoded nonsingularity is an authoring policy. A receiver may
    // underflow these nonzero operands to zero; that does not make the file
    // malformed or justify excluding every subnormal transform.
    let tiny = export([f32::from_bits(1), 0.0, 0.0, f32::from_bits(1), 0.0, 0.0]).unwrap();
    let operands = matrix_operands(&tiny);
    assert_ne!(operands[0], "0");
    assert_eq!(operands[0], operands[3]);
    if let Some(directory) = std::env::var_os("PDF_LIBRARY_NUMERIC_ARTIFACTS") {
        std::fs::write(
            std::path::Path::new(&directory).join("permitted-underflow-matrix.pdf"),
            tiny,
        )
        .unwrap();
    }
    assert!(matches!(
        export([1.0, 2.0, 2.0, 4.0, 0.0, 0.0]),
        Err(FinishedPdfError::InvalidGeometry)
    ));
    assert!(matches!(
        export([f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]),
        Err(FinishedPdfError::InvalidGeometry)
    ));
}
