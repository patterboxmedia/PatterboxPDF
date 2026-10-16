//! Public PDF/X metadata identity and trapping audit.

use pdf_oxide::{document::PdfDocument, object::Object};

use crate::ValidationError;

const MAX_REFERENCE_DEPTH: usize = 32;
const MAX_XMP_BYTES: usize = 4 * 1024 * 1024;
const PDFX_ID_NAMESPACE: &str = "http://www.npes.org/pdfx/ns/id/";

#[derive(Debug, Default)]
pub struct PublicPdfxMetadataAudit {
    findings: Vec<String>,
}

impl PublicPdfxMetadataAudit {
    #[must_use]
    pub fn findings(&self) -> &[String] {
        &self.findings
    }

    #[must_use]
    pub fn into_findings(self) -> Vec<String> {
        self.findings
    }
}

/// Validates the PDF/X identity in both the Info dictionary and catalog XMP,
/// plus the required trapped/not-trapped declaration.
///
/// # Errors
///
/// Returns an execution error only when the PDF object graph cannot be parsed
/// or resolved safely. Missing, malformed, or contradictory metadata is a
/// blocking finding.
pub fn audit_public_pdfx_metadata(
    candidate: &[u8],
) -> Result<PublicPdfxMetadataAudit, ValidationError> {
    let document = PdfDocument::from_bytes(candidate.to_vec())
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let mut audit = PublicPdfxMetadataAudit::default();
    audit_info_dictionary(&document, &mut audit)?;
    audit_catalog_xmp(&document, &mut audit)?;
    Ok(audit)
}

fn audit_info_dictionary(
    document: &PdfDocument,
    audit: &mut PublicPdfxMetadataAudit,
) -> Result<(), ValidationError> {
    let Some(trailer) = document.trailer().as_dict() else {
        return Err(ValidationError::MalformedReport(
            "PDF trailer is not a dictionary".into(),
        ));
    };
    let Some(info) = trailer.get("Info") else {
        audit
            .findings
            .push("PX4-META-001: document Info dictionary is missing".into());
        return Ok(());
    };
    let info = resolve(document, info)?;
    let Some(dictionary) = info.as_dict() else {
        audit
            .findings
            .push("PX4-META-001: document Info entry is not a dictionary".into());
        return Ok(());
    };
    let version = dictionary
        .get("GTS_PDFXVersion")
        .map(|value| resolve(document, value))
        .transpose()?;
    if version
        .as_ref()
        .is_none_or(|value| pdf_text(value).as_deref() != Some("PDF/X-4"))
    {
        audit
            .findings
            .push("PX4-META-002: Info /GTS_PDFXVersion must be the text string PDF/X-4".into());
    }
    let trapped = dictionary
        .get("Trapped")
        .map(|value| resolve(document, value))
        .transpose()?;
    if !trapped
        .as_ref()
        .and_then(Object::as_name)
        .is_some_and(|value| matches!(value, "True" | "False"))
    {
        audit
            .findings
            .push("PX4-META-003: Info /Trapped must explicitly be /True or /False".into());
    }
    Ok(())
}

fn audit_catalog_xmp(
    document: &PdfDocument,
    audit: &mut PublicPdfxMetadataAudit,
) -> Result<(), ValidationError> {
    let Some(trailer) = document.trailer().as_dict() else {
        return Err(ValidationError::MalformedReport(
            "PDF trailer is not a dictionary".into(),
        ));
    };
    let Some(root) = trailer.get("Root") else {
        audit
            .findings
            .push("PX4-META-004: document catalog is missing".into());
        return Ok(());
    };
    let root = resolve(document, root)?;
    let Some(catalog) = root.as_dict() else {
        audit
            .findings
            .push("PX4-META-004: document catalog is not a dictionary".into());
        return Ok(());
    };
    let Some(metadata) = catalog.get("Metadata") else {
        audit
            .findings
            .push("PX4-META-005: catalog XMP metadata stream is missing".into());
        return Ok(());
    };
    let metadata = resolve(document, metadata)?;
    let bytes = match metadata.decode_stream_data() {
        Ok(bytes) if bytes.len() <= MAX_XMP_BYTES => bytes,
        Ok(_) => return Err(ValidationError::OutputLimitExceeded),
        Err(error) => {
            audit.findings.push(format!(
                "PX4-META-006: catalog XMP metadata stream is unreadable: {error}"
            ));
            return Ok(());
        }
    };
    let Ok(xml) = std::str::from_utf8(&bytes) else {
        audit
            .findings
            .push("PX4-META-007: catalog XMP metadata is not UTF-8 XML".into());
        return Ok(());
    };
    let tree = match roxmltree::Document::parse(xml) {
        Ok(tree) => tree,
        Err(error) => {
            audit.findings.push(format!(
                "PX4-META-007: catalog XMP metadata is invalid XML: {error}"
            ));
            return Ok(());
        }
    };
    let mut identities = Vec::new();
    for node in tree.descendants().filter(roxmltree::Node::is_element) {
        if node.tag_name().namespace() == Some(PDFX_ID_NAMESPACE)
            && node.tag_name().name() == "GTS_PDFXVersion"
            && let Some(text) = node.text()
        {
            identities.push(text.trim());
        }
        for attribute in node.attributes() {
            if attribute.namespace() == Some(PDFX_ID_NAMESPACE)
                && attribute.name() == "GTS_PDFXVersion"
            {
                identities.push(attribute.value().trim());
            }
        }
    }
    if identities.is_empty() || identities.iter().any(|identity| *identity != "PDF/X-4") {
        audit
            .findings
            .push("PX4-META-008: XMP pdfxid:GTS_PDFXVersion must consistently be PDF/X-4".into());
    }
    Ok(())
}

fn resolve(document: &PdfDocument, object: &Object) -> Result<Object, ValidationError> {
    // Metadata needs the requested dictionary or value, not an expanded copy
    // of all its children. Recursive Catalog resolution expands Page/Parent
    // cycles and duplicates the whole publication until the worker runs out of
    // memory. Leave child references intact and resolve each inspected value.
    let mut current = object.clone();
    for _ in 0..MAX_REFERENCE_DEPTH {
        let Object::Reference(reference) = current else {
            return Ok(current);
        };
        current = document
            .load_object(reference)
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    }
    if matches!(current, Object::Reference(_)) {
        return Err(ValidationError::MalformedReport(
            "metadata indirect reference depth limit exceeded".into(),
        ));
    }
    Ok(current)
}

fn pdf_text(object: &Object) -> Option<String> {
    let bytes = object.as_string()?;
    if let Some(encoded) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        if encoded.len() % 2 != 0 {
            return None;
        }
        let units = (0..encoded.len())
            .step_by(2)
            .map(|index| u16::from_be_bytes([encoded[index], encoded[index + 1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&units).ok();
    }
    std::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use pdf_writer::{Name, Pdf, Rect, Ref, TextStr, types::TrappingStatus};

    use super::{audit_public_pdfx_metadata, resolve};
    use pdf_oxide::{document::PdfDocument, object::Object};

    const VALID_XMP: &[u8] = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/"><pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion></rdf:Description></rdf:RDF></x:xmpmeta>"#;
    const ATTRIBUTE_XMP: &[u8] = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/" pdfxid:GTS_PDFXVersion="PDF/X-4"/></rdf:RDF></x:xmpmeta>"#;
    const CONFLICTING_XMP: &[u8] = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/"><pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion><pdfxid:GTS_PDFXVersion>PDF/X-3</pdfxid:GTS_PDFXVersion></rdf:Description></rdf:RDF></x:xmpmeta>"#;

    #[derive(Clone, Copy)]
    enum Fault {
        None,
        MissingInfoVersion,
        WrongInfoVersion,
        MissingTrapped,
        InvalidTrapped,
        MissingXmp,
        InvalidXmp,
        WrongXmpVersion,
        AttributeXmpVersion,
        ConflictingXmpVersion,
    }

    fn fixture(fault: Fault) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let metadata = Ref::new(4);
        let info = Ref::new(5);
        let mut pdf = Pdf::new();
        {
            let mut root = pdf.catalog(catalog);
            root.pages(pages);
            if !matches!(fault, Fault::MissingXmp) {
                root.metadata(metadata);
            }
        }
        pdf.pages(pages).kids([page]).count(1);
        pdf.page(page)
            .parent(pages)
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0));
        if !matches!(fault, Fault::MissingXmp) {
            let xmp = match fault {
                Fault::InvalidXmp => b"<broken".to_vec(),
                Fault::WrongXmpVersion => String::from_utf8_lossy(VALID_XMP)
                    .replace("PDF/X-4", "PDF/X-3")
                    .into_bytes(),
                Fault::AttributeXmpVersion => ATTRIBUTE_XMP.to_vec(),
                Fault::ConflictingXmpVersion => CONFLICTING_XMP.to_vec(),
                _ => VALID_XMP.to_vec(),
            };
            pdf.metadata(metadata, &xmp);
        }
        {
            let mut document_info = pdf.document_info(info);
            if !matches!(fault, Fault::MissingInfoVersion) {
                document_info.pair(
                    Name(b"GTS_PDFXVersion"),
                    TextStr(if matches!(fault, Fault::WrongInfoVersion) {
                        "PDF/X-3"
                    } else {
                        "PDF/X-4"
                    }),
                );
            }
            if matches!(fault, Fault::InvalidTrapped) {
                document_info.pair(Name(b"Trapped"), Name(b"Unknown"));
            } else if !matches!(fault, Fault::MissingTrapped) {
                document_info.trapped(TrappingStatus::NotTrapped);
            }
        }
        pdf.finish()
    }

    #[test]
    fn matching_info_xmp_and_trapping_pass() {
        for fault in [Fault::None, Fault::AttributeXmpVersion] {
            assert!(
                audit_public_pdfx_metadata(&fixture(fault))
                    .expect("audit control")
                    .findings()
                    .is_empty()
            );
        }
    }

    #[test]
    fn catalog_resolution_keeps_uninspected_page_tree_indirect() {
        let document = PdfDocument::from_bytes(fixture(Fault::None)).unwrap();
        let root = &document.trailer().as_dict().unwrap()["Root"];
        let catalog = resolve(&document, root).unwrap();
        let dictionary = catalog.as_dict().unwrap();
        assert!(
            matches!(dictionary.get("Pages"), Some(Object::Reference(_))),
            "metadata lookup must not recursively expand the Page/Parent graph"
        );
        assert!(matches!(
            dictionary.get("Metadata"),
            Some(Object::Reference(_))
        ));
    }

    #[test]
    fn indirect_info_identity_and_trapping_keep_their_meaning() {
        let mut pdf = Pdf::new();
        pdf.catalog(Ref::new(1))
            .pages(Ref::new(2))
            .metadata(Ref::new(4));
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        pdf.page(Ref::new(3))
            .parent(Ref::new(2))
            .media_box(Rect::new(0.0, 0.0, 612.0, 792.0));
        pdf.metadata(Ref::new(4), VALID_XMP);
        pdf.document_info(Ref::new(5))
            .pair(Name(b"GTS_PDFXVersion"), Ref::new(6))
            .pair(Name(b"Trapped"), Ref::new(7));
        pdf.indirect(Ref::new(6)).primitive(TextStr("PDF/X-4"));
        pdf.indirect(Ref::new(7)).primitive(Name(b"False"));
        assert!(
            audit_public_pdfx_metadata(&pdf.finish())
                .unwrap()
                .findings()
                .is_empty()
        );
    }

    #[test]
    fn missing_or_wrong_info_identity_and_trapping_are_rejected() {
        for (fault, code) in [
            (Fault::MissingInfoVersion, "PX4-META-002"),
            (Fault::WrongInfoVersion, "PX4-META-002"),
            (Fault::MissingTrapped, "PX4-META-003"),
            (Fault::InvalidTrapped, "PX4-META-003"),
        ] {
            let audit = audit_public_pdfx_metadata(&fixture(fault)).expect("audit Info fault");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(code))
            );
        }
    }

    #[test]
    fn missing_invalid_or_wrong_xmp_is_rejected() {
        for (fault, code) in [
            (Fault::MissingXmp, "PX4-META-005"),
            (Fault::InvalidXmp, "PX4-META-007"),
            (Fault::WrongXmpVersion, "PX4-META-008"),
            (Fault::ConflictingXmpVersion, "PX4-META-008"),
        ] {
            let audit = audit_public_pdfx_metadata(&fixture(fault)).expect("audit XMP fault");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(code))
            );
        }
    }
}
