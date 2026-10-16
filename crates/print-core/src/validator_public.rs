//! Public-core PDF/X object-rule audit for the free composite authority.

use pdf_oxide::{
    document::PdfDocument,
    object::{Object, ObjectRef},
};
use std::collections::{HashMap, HashSet};

use crate::ValidationError;

pub const PUBLIC_CORE_NAME: &str = "Publisher V2 public PDF/X object-rule audit";
pub const PUBLIC_CORE_VERSION: &str = "7";
pub const PUBLIC_CORE_SOURCE_IDENTITY: &str = "publisher-v2/pdf-x-4/public-object-rule-audit/v1";
pub const PUBLIC_CORE_RULES_IDENTITY: &str =
    "publisher-v2/pdf-x-4/object-interactive-metadata-safe-action-annotation-form-rules/v7";

const MAX_GRAPH_ITEMS: usize = 1_000_000;
const MAX_REFERENCED_OBJECTS: usize = 100_000;
const MAX_ANNOTATIONS: usize = 100_000;
const MAX_REFERENCE_DEPTH: usize = 32;
const MAX_FINDINGS: usize = 512;
const MAX_REPORTED_NAME_CHARS: usize = 64;
const PDF_17_ACTION_SUBTYPES: &[&str] = &[
    "GoTo",
    "GoToR",
    "GoToE",
    "Launch",
    "Thread",
    "URI",
    "Sound",
    "Movie",
    "Hide",
    "Named",
    "SubmitForm",
    "ResetForm",
    "ImportData",
    "JavaScript",
    "SetOCGState",
    "Rendition",
    "Trans",
    "GoTo3DView",
];

#[derive(Debug, Default)]
pub struct PublicPdfxObjectAudit {
    objects_checked: usize,
    findings: Vec<String>,
}

impl PublicPdfxObjectAudit {
    #[must_use]
    pub const fn objects_checked(&self) -> usize {
        self.objects_checked
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

/// Walks the exact candidate's reachable object graph and checks three public
/// PDF/X core categories: transfer curves, alternate images, and embedded or
/// externally referenced file content.
///
/// # Errors
///
/// Returns a validation error when the PDF cannot be parsed or resolved within
/// the graph, reference, and finding budgets. Prohibited objects are ordinary
/// blocking findings rather than execution errors.
pub fn audit_public_pdfx_objects(
    candidate: &[u8],
) -> Result<PublicPdfxObjectAudit, ValidationError> {
    let document = PdfDocument::from_bytes(candidate.to_vec())
        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
    let mut context = AuditContext::default();
    context.walk(&document)?;
    Ok(PublicPdfxObjectAudit {
        objects_checked: context.referenced_objects.len(),
        findings: context.findings,
    })
}

#[derive(Default)]
struct AuditContext {
    graph_items: usize,
    referenced_objects: HashSet<ObjectRef>,
    findings: Vec<String>,
}

impl AuditContext {
    fn walk(&mut self, document: &PdfDocument) -> Result<(), ValidationError> {
        self.inspect_annotation_placement(document)?;
        let mut pending = vec![(document.trailer().clone(), "document trailer".to_owned())];
        while let Some((object, owner)) = pending.pop() {
            self.graph_items = self
                .graph_items
                .checked_add(1)
                .filter(|count| *count <= MAX_GRAPH_ITEMS)
                .ok_or(ValidationError::OutputLimitExceeded)?;
            match object {
                Object::Reference(reference) => {
                    if !self.referenced_objects.insert(reference) {
                        continue;
                    }
                    if self.referenced_objects.len() > MAX_REFERENCED_OBJECTS {
                        return Err(ValidationError::OutputLimitExceeded);
                    }
                    let resolved = document
                        .load_object(reference)
                        .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
                    pending.push((resolved, format!("object {reference}")));
                }
                Object::Array(values) => {
                    pending.extend(values.into_iter().map(|value| (value, owner.clone())));
                }
                Object::Dictionary(dictionary) => {
                    self.inspect_dictionary(document, &dictionary, false, &owner)?;
                    pending.extend(dictionary.into_values().map(|value| (value, owner.clone())));
                }
                Object::Stream { dict, .. } => {
                    self.inspect_dictionary(document, &dict, true, &owner)?;
                    pending.extend(dict.into_values().map(|value| (value, owner.clone())));
                }
                Object::Null
                | Object::Boolean(_)
                | Object::Integer(_)
                | Object::Real(_)
                | Object::String(_)
                | Object::Name(_) => {}
            }
        }
        Ok(())
    }

    fn inspect_annotation_placement(
        &mut self,
        document: &PdfDocument,
    ) -> Result<(), ValidationError> {
        let page_count = document
            .page_count()
            .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
        let mut annotations_checked = 0_usize;
        for page_index in 0..page_count {
            let page_number = page_index + 1;
            let page = document
                .get_page(page_index)
                .map_err(|error| ValidationError::MalformedReport(error.to_string()))?;
            let page = page.as_dict().ok_or_else(|| {
                ValidationError::MalformedReport(format!("page {page_number} is not a dictionary"))
            })?;
            let Some(annotations) = page.get("Annots") else {
                continue;
            };
            let print_area = match direct_print_area(document, page) {
                Ok(Some(print_area)) => Some(print_area),
                Ok(None) => {
                    self.finding(format!(
                        "PUBLISHER-SAFE-ANNOT-002: page {page_number} has annotations but no direct TrimBox or ArtBox"
                    ))?;
                    None
                }
                Err(()) => {
                    self.finding(format!(
                        "PUBLISHER-SAFE-ANNOT-002: page {page_number} annotation print area is unreadable"
                    ))?;
                    None
                }
            };
            let annotations = resolve_object(document, annotations)?;
            let Some(annotations) = annotations.as_array() else {
                self.finding(format!(
                    "PUBLISHER-SAFE-ANNOT-002: page {page_number} annotation list is unreadable"
                ))?;
                continue;
            };
            for (annotation_index, annotation) in annotations.iter().enumerate() {
                annotations_checked = annotations_checked
                    .checked_add(1)
                    .filter(|count| *count <= MAX_ANNOTATIONS)
                    .ok_or(ValidationError::OutputLimitExceeded)?;
                let annotation = resolve_object(document, annotation)?;
                let Some(annotation) = object_dictionary(&annotation) else {
                    self.finding(format!(
                        "PUBLISHER-SAFE-ANNOT-002: page {page_number} annotation {} is unreadable",
                        annotation_index + 1
                    ))?;
                    continue;
                };
                if let Some(subtype) = annotation.get("Subtype") {
                    let subtype = resolve_object(document, subtype)?;
                    if matches!(subtype.as_name(), Some("Text")) {
                        self.finding(format!(
                            "PUBLISHER-SAFE-ANNOT-003: page {page_number} annotation {} is a Text annotation",
                            annotation_index + 1
                        ))?;
                    }
                }
                let Some(print_area) = print_area else {
                    continue;
                };
                let Some(rectangle) = annotation
                    .get("Rect")
                    .and_then(|value| resolved_rectangle(document, value).ok())
                else {
                    self.finding(format!(
                        "PUBLISHER-SAFE-ANNOT-002: page {page_number} annotation {} has no readable rectangle",
                        annotation_index + 1
                    ))?;
                    continue;
                };
                if rectangle.positively_intersects(print_area) {
                    self.finding(format!(
                        "PUBLISHER-SAFE-ANNOT-001: page {page_number} annotation {} overlaps the print area",
                        annotation_index + 1
                    ))?;
                }
            }
        }
        Ok(())
    }

    fn inspect_dictionary(
        &mut self,
        document: &PdfDocument,
        dictionary: &HashMap<String, Object>,
        is_stream: bool,
        owner: &str,
    ) -> Result<(), ValidationError> {
        if dictionary.contains_key("TR") {
            self.finding(format!(
                "PX4-TR-001: prohibited /TR transfer curve in {owner}"
            ))?;
        }
        if let Some(transfer) = dictionary.get("TR2") {
            let resolved = resolve_object(document, transfer)?;
            if resolved.as_name() != Some("Default") {
                self.finding(format!("PX4-TR-002: /TR2 must be /Default in {owner}"))?;
            }
        }
        if dictionary.contains_key("TransferFunction") {
            self.finding(format!(
                "PX4-TR-003: prohibited halftone /TransferFunction in {owner}"
            ))?;
        }
        if dictionary.contains_key("Alternates") {
            self.finding(format!(
                "PX4-ALT-001: prohibited alternate image list in {owner}"
            ))?;
        }
        if dictionary.contains_key("OPI") {
            self.finding(format!(
                "PX4-EXTERNAL-004: prohibited OPI external graphics in {owner}"
            ))?;
        }
        self.inspect_interactive_content(document, dictionary, owner)?;
        for (key, code, description) in [
            ("EmbeddedFiles", "PX4-EMBED-001", "embedded-file name tree"),
            ("EF", "PX4-EMBED-002", "embedded-file stream dictionary"),
            ("AF", "PX4-EMBED-003", "associated-file array"),
        ] {
            if dictionary.contains_key(key) {
                self.finding(format!("{code}: prohibited {description} in {owner}"))?;
            }
        }
        if is_stream {
            for (key, code) in [
                ("F", "PX4-EXTERNAL-001"),
                ("FFilter", "PX4-EXTERNAL-002"),
                ("FDecodeParms", "PX4-EXTERNAL-003"),
            ] {
                if dictionary.contains_key(key) {
                    self.finding(format!(
                        "{code}: prohibited external stream /{key} entry in {owner}"
                    ))?;
                }
            }
        }
        Ok(())
    }

    fn inspect_interactive_content(
        &mut self,
        document: &PdfDocument,
        dictionary: &HashMap<String, Object>,
        owner: &str,
    ) -> Result<(), ValidationError> {
        let is_action_dictionary = match dictionary.get("Type") {
            Some(action_type) => resolve_object(document, action_type)?.as_name() == Some("Action"),
            None => false,
        };
        let action_subtype = dictionary
            .get("S")
            .map(|action| resolve_object(document, action))
            .transpose()?;
        let action_subtype = action_subtype.as_ref().and_then(Object::as_name);
        if action_subtype == Some("JavaScript") {
            self.finding(format!(
                "PX4-ACTION-001: prohibited JavaScript action in {owner}"
            ))?;
        } else if is_action_dictionary
            || action_subtype.is_some_and(|name| PDF_17_ACTION_SUBTYPES.contains(&name))
        {
            let subtype = action_subtype.map_or("unspecified", bounded_name);
            self.finding(format!(
                "PUBLISHER-SAFE-ACTION-001: Publisher safe export rejects /{subtype} action in {owner}"
            ))?;
        }
        for (key, code, description) in [
            (
                "JavaScript",
                "PX4-ACTION-003",
                "named JavaScript action tree",
            ),
            ("JS", "PX4-ACTION-004", "JavaScript payload"),
            (
                "AcroForm",
                "PUBLISHER-SAFE-FORM-001",
                "interactive form catalog",
            ),
            ("XFA", "PX4-FORM-002", "XFA form content"),
            ("FT", "PUBLISHER-SAFE-FORM-002", "form field dictionary"),
        ] {
            if dictionary.contains_key(key) {
                self.finding(format!("{code}: prohibited {description} in {owner}"))?;
            }
        }
        if let Some(subtype) = dictionary.get("Subtype") {
            let resolved = resolve_object(document, subtype)?;
            if matches!(
                resolved.as_name(),
                Some("3D" | "Movie" | "Sound" | "Screen" | "RichMedia")
            ) {
                self.finding(format!(
                    "PX4-ANNOT-001: prohibited audio, video, or 3D annotation in {owner}"
                ))?;
            }
        }
        Ok(())
    }

    fn finding(&mut self, finding: String) -> Result<(), ValidationError> {
        if self.findings.len() >= MAX_FINDINGS {
            return Err(ValidationError::OutputLimitExceeded);
        }
        self.findings.push(finding);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct PdfRectangle {
    left: f64,
    bottom: f64,
    right: f64,
    top: f64,
}

impl PdfRectangle {
    fn positively_intersects(self, other: Self) -> bool {
        self.left.max(other.left) < self.right.min(other.right)
            && self.bottom.max(other.bottom) < self.top.min(other.top)
    }
}

fn direct_print_area(
    document: &PdfDocument,
    page: &HashMap<String, Object>,
) -> Result<Option<PdfRectangle>, ()> {
    if let Some(trim_box) = page.get("TrimBox") {
        return resolved_rectangle(document, trim_box).map(Some);
    }
    page.get("ArtBox")
        .map(|art_box| resolved_rectangle(document, art_box))
        .transpose()
}

fn resolved_rectangle(document: &PdfDocument, value: &Object) -> Result<PdfRectangle, ()> {
    let value = resolve_object(document, value).map_err(|_| ())?;
    let values = value.as_array().ok_or(())?;
    let [x1, y1, x2, y2] = values.as_slice() else {
        return Err(());
    };
    let x1 = resolved_number(document, x1)?;
    let y1 = resolved_number(document, y1)?;
    let x2 = resolved_number(document, x2)?;
    let y2 = resolved_number(document, y2)?;
    if ![x1, y1, x2, y2].into_iter().all(f64::is_finite) {
        return Err(());
    }
    Ok(PdfRectangle {
        left: x1.min(x2),
        bottom: y1.min(y2),
        right: x1.max(x2),
        top: y1.max(y2),
    })
}

fn resolved_number(document: &PdfDocument, value: &Object) -> Result<f64, ()> {
    const MAX_EXACT_F64_INTEGER: i64 = 9_007_199_254_740_992;
    match resolve_object(document, value).map_err(|_| ())? {
        Object::Integer(value)
            if (-MAX_EXACT_F64_INTEGER..=MAX_EXACT_F64_INTEGER).contains(&value) =>
        {
            #[allow(clippy::cast_precision_loss)]
            Ok(value as f64)
        }
        Object::Real(value) => Ok(value),
        _ => Err(()),
    }
}

fn object_dictionary(object: &Object) -> Option<&HashMap<String, Object>> {
    match object {
        Object::Dictionary(dictionary) => Some(dictionary),
        Object::Stream { dict, .. } => Some(dict),
        _ => None,
    }
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

fn bounded_name(name: &str) -> &str {
    if name.len() <= MAX_REPORTED_NAME_CHARS {
        name
    } else {
        "oversized"
    }
}

#[cfg(test)]
mod tests {
    use pdf_writer::{Name, Pdf, Rect, Ref, Str};

    use super::audit_public_pdfx_objects;

    #[derive(Clone, Copy)]
    enum Fault {
        None,
        TransferCurve,
        DefaultTransferCurve,
        NonDefaultTransferCurve,
        HalftoneTransferFunction,
        AlternateImage,
        EmbeddedFile,
        EmbeddedFileNameTree,
        ExternalStream,
        ExternalStreamFilterEntries,
        OpiExternalGraphic,
        JavaScriptAction,
        LaunchAction,
        NamedJavaScriptTree,
        AcroForm,
        XfaForm,
        ProhibitedAnnotation(&'static [u8]),
    }

    #[derive(Clone, Copy)]
    enum ActionLocation {
        CatalogOpen,
        PageAdditional,
        AnnotationPrimary,
    }

    #[derive(Clone, Copy)]
    enum PrintAreaSource {
        DirectTrim,
        DirectArt,
        InheritedTrim,
    }

    fn fixture(fault: Fault) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let fault_object = Ref::new(4);
        let embedded_file = Ref::new(5);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        {
            let mut root = pdf.catalog(catalog);
            root.pages(pages);
            if matches!(fault, Fault::EmbeddedFile) {
                root.insert(Name(b"AF")).array().item(fault_object);
            }
            if matches!(fault, Fault::EmbeddedFileNameTree) {
                root.insert(Name(b"Names"))
                    .dict()
                    .pair(Name(b"EmbeddedFiles"), fault_object);
            }
            if matches!(fault, Fault::JavaScriptAction | Fault::LaunchAction) {
                root.pair(Name(b"OpenAction"), fault_object);
            }
            if matches!(fault, Fault::NamedJavaScriptTree) {
                root.insert(Name(b"Names"))
                    .dict()
                    .pair(Name(b"JavaScript"), fault_object);
            }
            if matches!(fault, Fault::AcroForm | Fault::XfaForm) {
                root.pair(Name(b"AcroForm"), fault_object);
            }
        }
        pdf.pages(pages).kids([page]).count(1);
        {
            let mut writer = pdf.page(page);
            writer
                .parent(pages)
                .media_box(Rect::new(0.0, 0.0, 612.0, 792.0));
            match fault {
                Fault::TransferCurve
                | Fault::DefaultTransferCurve
                | Fault::NonDefaultTransferCurve
                | Fault::HalftoneTransferFunction => {
                    writer
                        .resources()
                        .ext_g_states()
                        .pair(Name(b"Fault"), fault_object);
                }
                Fault::AlternateImage
                | Fault::ExternalStream
                | Fault::ExternalStreamFilterEntries
                | Fault::OpiExternalGraphic => {
                    writer
                        .resources()
                        .x_objects()
                        .pair(Name(b"Fault"), fault_object);
                }
                Fault::None
                | Fault::EmbeddedFile
                | Fault::EmbeddedFileNameTree
                | Fault::JavaScriptAction
                | Fault::LaunchAction
                | Fault::NamedJavaScriptTree
                | Fault::AcroForm
                | Fault::XfaForm => {}
                Fault::ProhibitedAnnotation(_) => {
                    writer.annotations([fault_object]);
                }
            }
        }
        write_fault_objects(&mut pdf, fault, fault_object, embedded_file);
        write_interactive_fault(&mut pdf, fault, fault_object, embedded_file);
        pdf.finish()
    }

    fn action_fixture(
        subtypes: &[&[u8]],
        location: ActionLocation,
        include_type: bool,
        cycle: bool,
    ) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let annotation = Ref::new(4);
        let action_ids: Vec<_> = (0..subtypes.len())
            .map(|index| Ref::new(i32::try_from(index).expect("small action fixture") + 5))
            .collect();
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        {
            let mut root = pdf.catalog(catalog);
            root.pages(pages);
            if matches!(location, ActionLocation::CatalogOpen) {
                root.pair(Name(b"OpenAction"), action_ids[0]);
            }
        }
        pdf.pages(pages).kids([page]).count(1);
        {
            let mut writer = pdf.page(page);
            writer
                .parent(pages)
                .media_box(Rect::new(0.0, 0.0, 612.0, 792.0));
            if matches!(location, ActionLocation::PageAdditional) {
                writer
                    .insert(Name(b"AA"))
                    .dict()
                    .pair(Name(b"O"), action_ids[0]);
            }
            if matches!(location, ActionLocation::AnnotationPrimary) {
                writer.annotations([annotation]);
            }
        }
        if matches!(location, ActionLocation::AnnotationPrimary) {
            let mut writer = pdf.indirect(annotation).dict();
            writer.pair(Name(b"Type"), Name(b"Annot"));
            writer.pair(Name(b"Subtype"), Name(b"Link"));
            writer.pair(Name(b"A"), action_ids[0]);
        }
        for (index, subtype) in subtypes.iter().enumerate() {
            let mut action = pdf.indirect(action_ids[index]).dict();
            if include_type {
                action.pair(Name(b"Type"), Name(b"Action"));
            }
            action.pair(Name(b"S"), Name(subtype));
            if let Some(next) = action_ids.get(index + 1) {
                action.pair(Name(b"Next"), *next);
            } else if cycle {
                action.pair(Name(b"Next"), action_ids[0]);
            }
        }
        pdf.finish()
    }

    fn annotation_placement_fixture(
        rectangle: Option<Rect>,
        source: PrintAreaSource,
        subtype: &[u8],
    ) -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let annotation = Ref::new(4);
        let print_area = Rect::new(100.0, 100.0, 500.0, 700.0);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        {
            let mut writer = pdf.pages(pages);
            writer.kids([page]).count(1);
            if matches!(source, PrintAreaSource::InheritedTrim) {
                writer.pair(Name(b"TrimBox"), print_area);
            }
        }
        {
            let mut writer = pdf.page(page);
            writer
                .parent(pages)
                .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
                .annotations([annotation]);
            match source {
                PrintAreaSource::DirectTrim => {
                    writer.trim_box(print_area);
                }
                PrintAreaSource::DirectArt => {
                    writer.art_box(print_area);
                }
                PrintAreaSource::InheritedTrim => {}
            }
        }
        {
            let mut writer = pdf.indirect(annotation).dict();
            writer.pair(Name(b"Type"), Name(b"Annot"));
            writer.pair(Name(b"Subtype"), Name(subtype));
            if let Some(rectangle) = rectangle {
                writer.pair(Name(b"Rect"), rectangle);
            }
        }
        pdf.finish()
    }

    fn write_fault_objects(pdf: &mut Pdf, fault: Fault, fault_object: Ref, embedded_file: Ref) {
        match fault {
            Fault::TransferCurve => {
                pdf.ext_graphics(fault_object)
                    .pair(Name(b"TR"), Name(b"Identity"));
            }
            Fault::NonDefaultTransferCurve => {
                pdf.ext_graphics(fault_object)
                    .pair(Name(b"TR2"), Name(b"Identity"));
            }
            Fault::DefaultTransferCurve => {
                pdf.ext_graphics(fault_object)
                    .pair(Name(b"TR2"), Name(b"Default"));
            }
            Fault::HalftoneTransferFunction => {
                pdf.ext_graphics(fault_object)
                    .pair(Name(b"HT"), embedded_file);
                pdf.indirect(embedded_file)
                    .dict()
                    .pair(Name(b"TransferFunction"), Name(b"Identity"));
            }
            Fault::AlternateImage => {
                let mut image = pdf.indirect(fault_object).dict();
                image.pair(Name(b"Type"), Name(b"XObject"));
                image.pair(Name(b"Subtype"), Name(b"Image"));
                image
                    .insert(Name(b"Alternates"))
                    .array()
                    .item(embedded_file);
            }
            Fault::EmbeddedFile => {
                {
                    let mut file = pdf.indirect(fault_object).dict();
                    file.pair(Name(b"Type"), Name(b"Filespec"));
                    file.pair(Name(b"F"), Str(b"attachment.txt"));
                    file.insert(Name(b"EF"))
                        .dict()
                        .pair(Name(b"F"), embedded_file);
                }
                pdf.stream(embedded_file, b"attachment");
            }
            Fault::EmbeddedFileNameTree => {
                pdf.indirect(fault_object)
                    .dict()
                    .insert(Name(b"Names"))
                    .array();
            }
            Fault::ExternalStream => {
                let mut stream = pdf.stream(fault_object, b"");
                stream.pair(Name(b"Type"), Name(b"XObject"));
                stream.pair(Name(b"Subtype"), Name(b"Image"));
                stream.pair(Name(b"F"), Str(b"external-image.dat"));
            }
            Fault::ExternalStreamFilterEntries => {
                let mut stream = pdf.stream(fault_object, b"");
                stream.pair(Name(b"Type"), Name(b"XObject"));
                stream.pair(Name(b"Subtype"), Name(b"Image"));
                stream.pair(Name(b"FFilter"), Name(b"FlateDecode"));
                stream.insert(Name(b"FDecodeParms")).dict();
            }
            Fault::OpiExternalGraphic => {
                let mut image = pdf.indirect(fault_object).dict();
                image.pair(Name(b"Type"), Name(b"XObject"));
                image.pair(Name(b"Subtype"), Name(b"Image"));
                image.insert(Name(b"OPI")).dict();
            }
            Fault::None
            | Fault::JavaScriptAction
            | Fault::LaunchAction
            | Fault::NamedJavaScriptTree
            | Fault::AcroForm
            | Fault::XfaForm
            | Fault::ProhibitedAnnotation(_) => {}
        }
    }

    fn write_interactive_fault(pdf: &mut Pdf, fault: Fault, fault_object: Ref, child: Ref) {
        match fault {
            Fault::JavaScriptAction | Fault::LaunchAction => {
                let mut action = pdf.indirect(fault_object).dict();
                action.pair(Name(b"Type"), Name(b"Action"));
                let subtype: &[u8] = if matches!(fault, Fault::JavaScriptAction) {
                    b"JavaScript"
                } else {
                    b"Launch"
                };
                action.pair(Name(b"S"), Name(subtype));
                if matches!(fault, Fault::JavaScriptAction) {
                    action.pair(Name(b"JS"), Str(b"app.alert('fault')"));
                }
            }
            Fault::NamedJavaScriptTree => {
                pdf.indirect(fault_object)
                    .dict()
                    .insert(Name(b"Names"))
                    .array();
            }
            Fault::AcroForm => {
                pdf.indirect(fault_object)
                    .dict()
                    .insert(Name(b"Fields"))
                    .array()
                    .item(child);
                pdf.indirect(child).dict().pair(Name(b"FT"), Name(b"Tx"));
            }
            Fault::XfaForm => {
                pdf.indirect(fault_object).dict().pair(Name(b"XFA"), child);
                pdf.stream(child, b"xfa");
            }
            Fault::ProhibitedAnnotation(subtype) => {
                let mut annotation = pdf.indirect(fault_object).dict();
                annotation.pair(Name(b"Type"), Name(b"Annot"));
                annotation.pair(Name(b"Subtype"), Name(subtype));
            }
            Fault::None
            | Fault::TransferCurve
            | Fault::DefaultTransferCurve
            | Fault::NonDefaultTransferCurve
            | Fault::HalftoneTransferFunction
            | Fault::AlternateImage
            | Fault::EmbeddedFile
            | Fault::EmbeddedFileNameTree
            | Fault::ExternalStream
            | Fault::ExternalStreamFilterEntries
            | Fault::OpiExternalGraphic => {}
        }
    }

    #[test]
    fn clean_reachable_graph_has_no_public_core_findings() {
        let audit = audit_public_pdfx_objects(&fixture(Fault::None)).expect("parse clean fixture");
        assert!(audit.findings().is_empty());
        assert_eq!(audit.objects_checked(), 3);
    }

    #[test]
    fn transfer_curve_entries_are_rejected_except_tr2_default() {
        let tr = audit_public_pdfx_objects(&fixture(Fault::TransferCurve)).expect("parse TR fault");
        assert!(
            tr.findings()
                .iter()
                .any(|finding| finding.contains("PX4-TR-001"))
        );
        let tr2 = audit_public_pdfx_objects(&fixture(Fault::NonDefaultTransferCurve))
            .expect("parse TR2 fault");
        assert!(
            tr2.findings()
                .iter()
                .any(|finding| finding.contains("PX4-TR-002"))
        );
        let default = audit_public_pdfx_objects(&fixture(Fault::DefaultTransferCurve))
            .expect("parse allowed TR2 default");
        assert!(default.findings().is_empty());
        let halftone = audit_public_pdfx_objects(&fixture(Fault::HalftoneTransferFunction))
            .expect("parse halftone transfer-function fault");
        assert!(
            halftone
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-TR-003"))
        );
    }

    #[test]
    fn alternate_images_are_rejected() {
        let audit = audit_public_pdfx_objects(&fixture(Fault::AlternateImage))
            .expect("parse alternate-image fault");
        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-ALT-001"))
        );
    }

    #[test]
    fn embedded_and_external_files_are_rejected() {
        let embedded = audit_public_pdfx_objects(&fixture(Fault::EmbeddedFile))
            .expect("parse embedded-file fault");
        assert!(
            embedded
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-EMBED-002"))
        );
        assert!(
            embedded
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-EMBED-003"))
        );
        let name_tree = audit_public_pdfx_objects(&fixture(Fault::EmbeddedFileNameTree))
            .expect("parse embedded-file name-tree fault");
        assert!(
            name_tree
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-EMBED-001"))
        );
        let external = audit_public_pdfx_objects(&fixture(Fault::ExternalStream))
            .expect("parse external-stream fault");
        assert!(
            external
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-EXTERNAL-001"))
        );
        let filter_entries =
            audit_public_pdfx_objects(&fixture(Fault::ExternalStreamFilterEntries))
                .expect("parse external stream filter-entry fault");
        assert!(
            filter_entries
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-EXTERNAL-002"))
        );
        assert!(
            filter_entries
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-EXTERNAL-003"))
        );
        let opi = audit_public_pdfx_objects(&fixture(Fault::OpiExternalGraphic))
            .expect("parse OPI fault");
        assert!(
            opi.findings()
                .iter()
                .any(|finding| finding.contains("PX4-EXTERNAL-004"))
        );
    }

    #[test]
    fn javascript_safe_actions_and_safe_forms_are_rejected() {
        for (fault, code) in [
            (Fault::JavaScriptAction, "PX4-ACTION-001"),
            (Fault::LaunchAction, "PUBLISHER-SAFE-ACTION-001"),
            (Fault::NamedJavaScriptTree, "PX4-ACTION-003"),
        ] {
            let audit = audit_public_pdfx_objects(&fixture(fault)).expect("parse action fault");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(code)),
                "missing {code}: {:?}",
                audit.findings()
            );
        }
        let javascript = audit_public_pdfx_objects(&fixture(Fault::JavaScriptAction))
            .expect("parse JavaScript payload fault");
        assert!(
            javascript
                .findings()
                .iter()
                .any(|finding| finding.contains("PX4-ACTION-004"))
        );
        let form = audit_public_pdfx_objects(&fixture(Fault::AcroForm)).expect("parse form fault");
        for code in ["PUBLISHER-SAFE-FORM-001", "PUBLISHER-SAFE-FORM-002"] {
            assert!(
                form.findings().iter().any(|finding| finding.contains(code)),
                "missing {code}: {:?}",
                form.findings()
            );
        }
        let xfa = audit_public_pdfx_objects(&fixture(Fault::XfaForm)).expect("parse XFA fault");
        assert!(
            xfa.findings()
                .iter()
                .any(|finding| finding.contains("PX4-FORM-002"))
        );
    }

    #[test]
    fn every_pdf_17_action_subtype_is_rejected_under_publisher_safe_export_policy() {
        for subtype in super::PDF_17_ACTION_SUBTYPES {
            let audit = audit_public_pdfx_objects(&action_fixture(
                &[subtype.as_bytes()],
                ActionLocation::CatalogOpen,
                false,
                false,
            ))
            .expect("parse recognized action fixture");
            let expected = if *subtype == "JavaScript" {
                "PX4-ACTION-001"
            } else {
                "PUBLISHER-SAFE-ACTION-001"
            };
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains(expected)),
                "missing {expected} for {subtype}: {:?}",
                audit.findings()
            );
        }
    }

    #[test]
    fn safe_action_walk_reaches_page_annotation_chains_and_cycles() {
        for location in [
            ActionLocation::CatalogOpen,
            ActionLocation::PageAdditional,
            ActionLocation::AnnotationPrimary,
        ] {
            let audit = audit_public_pdfx_objects(&action_fixture(
                &[b"URI", b"GoToR"],
                location,
                false,
                true,
            ))
            .expect("parse chained action fixture");
            assert_eq!(
                audit
                    .findings()
                    .iter()
                    .filter(|finding| finding.contains("PUBLISHER-SAFE-ACTION-001"))
                    .count(),
                2
            );
        }

        let unknown_typed = audit_public_pdfx_objects(&action_fixture(
            &[b"VendorPrivate"],
            ActionLocation::CatalogOpen,
            true,
            false,
        ))
        .expect("parse typed private action fixture");
        assert!(unknown_typed.findings().iter().any(|finding| {
            finding.contains("PUBLISHER-SAFE-ACTION-001") && finding.contains("/VendorPrivate")
        }));
    }

    #[test]
    fn annotation_overlap_uses_direct_trim_and_art_print_areas() {
        for (rectangle, source) in [
            (
                Rect::new(120.0, 120.0, 180.0, 180.0),
                PrintAreaSource::DirectTrim,
            ),
            (
                Rect::new(80.0, 80.0, 120.0, 120.0),
                PrintAreaSource::DirectArt,
            ),
            (
                Rect::new(180.0, 180.0, 120.0, 120.0),
                PrintAreaSource::DirectTrim,
            ),
        ] {
            let audit = audit_public_pdfx_objects(&annotation_placement_fixture(
                Some(rectangle),
                source,
                b"Stamp",
            ))
            .expect("parse overlapping annotation fixture");
            assert!(audit.findings().iter().any(|finding| {
                finding.contains("PUBLISHER-SAFE-ANNOT-001")
                    && finding.contains("page 1 annotation 1")
            }));
        }
    }

    #[test]
    fn parent_trim_is_not_treated_as_a_page_print_area() {
        let audit = audit_public_pdfx_objects(&annotation_placement_fixture(
            Some(Rect::new(120.0, 120.0, 180.0, 180.0)),
            PrintAreaSource::InheritedTrim,
            b"Stamp",
        ))
        .expect("parse annotation fixture with parent-only TrimBox");
        assert!(audit.findings().iter().any(|finding| {
            finding.contains("PUBLISHER-SAFE-ANNOT-002")
                && finding.contains("no direct TrimBox or ArtBox")
        }));
        assert!(
            audit
                .findings()
                .iter()
                .all(|finding| !finding.contains("PUBLISHER-SAFE-ANNOT-001"))
        );
    }

    #[test]
    fn annotation_outside_or_touching_the_print_area_does_not_claim_overlap() {
        for rectangle in [
            Rect::new(20.0, 20.0, 80.0, 80.0),
            Rect::new(20.0, 20.0, 100.0, 80.0),
        ] {
            let audit = audit_public_pdfx_objects(&annotation_placement_fixture(
                Some(rectangle),
                PrintAreaSource::DirectTrim,
                b"Stamp",
            ))
            .expect("parse non-overlapping annotation fixture");
            assert!(
                audit
                    .findings()
                    .iter()
                    .all(|finding| !finding.contains("PUBLISHER-SAFE-ANNOT-001"))
            );
        }
    }

    #[test]
    fn annotation_without_a_readable_rectangle_fails_safe() {
        let audit = audit_public_pdfx_objects(&annotation_placement_fixture(
            None,
            PrintAreaSource::DirectTrim,
            b"Stamp",
        ))
        .expect("parse annotation fixture without rectangle");
        assert!(
            audit
                .findings()
                .iter()
                .any(|finding| finding.contains("PUBLISHER-SAFE-ANNOT-002"))
        );
    }

    #[test]
    fn text_annotations_are_rejected_separately_from_placement() {
        let outside = Rect::new(20.0, 20.0, 80.0, 80.0);
        let text = audit_public_pdfx_objects(&annotation_placement_fixture(
            Some(outside),
            PrintAreaSource::DirectTrim,
            b"Text",
        ))
        .expect("parse outside Text annotation fixture");
        assert!(
            text.findings()
                .iter()
                .any(|finding| finding.contains("PUBLISHER-SAFE-ANNOT-003"))
        );
        assert!(
            text.findings()
                .iter()
                .all(|finding| !finding.contains("PUBLISHER-SAFE-ANNOT-001"))
        );

        let stamp = audit_public_pdfx_objects(&annotation_placement_fixture(
            Some(outside),
            PrintAreaSource::DirectTrim,
            b"Stamp",
        ))
        .expect("parse outside Stamp annotation control");
        assert!(stamp.findings().is_empty());
    }

    #[test]
    fn audio_video_and_3d_annotations_are_rejected() {
        for subtype in [
            b"3D".as_slice(),
            b"Movie".as_slice(),
            b"Sound".as_slice(),
            b"Screen".as_slice(),
            b"RichMedia".as_slice(),
        ] {
            let audit = audit_public_pdfx_objects(&fixture(Fault::ProhibitedAnnotation(subtype)))
                .expect("parse prohibited annotation fault");
            assert!(
                audit
                    .findings()
                    .iter()
                    .any(|finding| finding.contains("PX4-ANNOT-001")),
                "missing annotation finding for {:?}",
                String::from_utf8_lossy(subtype)
            );
        }
    }
}
