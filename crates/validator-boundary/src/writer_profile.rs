//! Closed Publisher emission profile, separate from PDF/X conformance.

use flate2::{Decompress, FlushDecompress, Status};
use pdf_oxide::{
    document::PdfDocument,
    object::{Object, ObjectRef},
    parser::parse_object,
};
use raster_capacity::{MAX_RETAINED_RASTER_BYTES, production_raster_pixels};
use std::collections::{HashMap, HashSet};

const MAX_ITEMS: usize = 1_000_000;
const MAX_OBJECTS: usize = 100_000;
const MAX_DEPTH: usize = 24;
const MAX_OPERATORS: usize = 1_000_000;
const PREFIX: &str = "blocking Publisher writer profile v5";

/// A closed emission census, not a PDF/X conformance verdict. Payload semantics
/// remain owned by the font, ICC, raster and general PDF/X auditors. Image sample
/// sizes and lossless inflation are checked here before those broader auditors.
#[derive(Debug)]
pub struct WriterProfileAudit {
    pub artifact_sha256: [u8; 32],
    pub findings: Vec<String>,
}

/// Inspect exact candidate bytes inside the confined validator worker only.
///
/// # Errors
/// Returns an error on parse or traversal-budget failure. Unexpected writer
/// constructs are blocking findings with paths, bound to `artifact_sha256`.
pub fn audit_publisher_writer_output(bytes: &[u8]) -> Result<WriterProfileAudit, String> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > super::MAX_CANDIDATE_BYTES {
        return Err("Publisher profile candidate limit".into());
    }
    let document = PdfDocument::from_bytes(bytes.to_vec()).map_err(|e| e.to_string())?;
    let mut audit = Census {
        document: &document,
        findings: vec![],
        seen: HashSet::new(),
        objects: HashSet::new(),
        items: 0,
        operators: 0,
        decoded_samples: 0,
    };
    if document.version() != (1, 6) {
        audit.reject("header", "only PDF 1.6 is emitted")?;
    }
    // The writer emits one classic cross-reference section, never a stream or
    // incremental trailer. This reads the final pointer, not payload substrings.
    let final_pointer = bytes
        .windows(9)
        .rposition(|w| w == b"startxref")
        .and_then(|i| std::str::from_utf8(&bytes[i + 9..]).ok())
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<usize>().ok());
    if final_pointer
        .is_none_or(|offset| bytes.get(offset..offset.saturating_add(4)) != Some(b"xref"))
    {
        audit.reject(
            "trailer/startxref",
            "expected classic cross-reference table",
        )?;
    }
    audit.walk(document.trailer().clone(), "Trailer", "trailer", 0)?;
    let ids = document.all_object_ids();
    if ids.len() > MAX_OBJECTS {
        return Err("Publisher profile object limit".into());
    }
    for id in ids {
        if id != 0 && !audit.objects.contains(&id) {
            audit.reject(
                &format!("object {id}"),
                "object is outside the writer resource graph",
            )?;
        }
    }
    Ok(WriterProfileAudit {
        artifact_sha256: super::Hash32::of(bytes).0,
        findings: audit.findings,
    })
}

struct Census<'a> {
    document: &'a PdfDocument,
    findings: Vec<String>,
    seen: HashSet<(ObjectRef, &'static str)>,
    objects: HashSet<u32>,
    items: usize,
    operators: usize,
    decoded_samples: usize,
}

impl Census<'_> {
    fn reject(&mut self, path: &str, reason: &str) -> Result<(), String> {
        if self.findings.len() >= super::MAX_FINDINGS {
            return Err("Publisher profile finding limit".into());
        }
        self.findings.push(format!("{PREFIX}: {path}: {reason}"));
        Ok(())
    }

    fn walk(
        &mut self,
        object: Object,
        role: &'static str,
        path: &str,
        depth: usize,
    ) -> Result<(), String> {
        self.items += 1;
        if depth > MAX_DEPTH || self.items > MAX_ITEMS {
            return Err("Publisher profile graph limit".into());
        }
        if let Object::Reference(reference) = object {
            if reference.r#gen != 0 {
                self.reject(path, "writer references use generation zero")?;
            }
            self.objects.insert(reference.id);
            if self.objects.len() > MAX_OBJECTS {
                return Err("Publisher profile object limit".into());
            }
            if !self.seen.insert((reference, role)) {
                return Ok(());
            }
            let value = self
                .document
                .load_object(reference)
                .map_err(|e| format!("{path}: {e}"))?;
            return self.walk(
                value,
                role,
                &format!("{path} [object {reference}]"),
                depth + 1,
            );
        }
        let (dict, data) = match object {
            Object::Dictionary(dict) => (dict, None),
            Object::Stream { dict, data } => (dict, Some(data)),
            _ => return self.reject(path, "expected writer dictionary or stream"),
        };
        let role = if role == "XObject" || role == "FormPaint" {
            match dict.get("Subtype").and_then(Object::as_name) {
                Some("Image") => "Image",
                Some("Form") if role == "XObject" => "Form",
                _ => return self.reject(path, "unexpected XObject subtype or nested Form"),
            }
        } else {
            role
        };
        let role = if role == "Alpha" && dict.contains_key("SMask") {
            "ShadowState"
        } else {
            role
        };
        let (allowed, required, stream) = schema(role);
        if role == "Descriptor" && dict.contains_key("FontFile2") == dict.contains_key("FontFile3")
        {
            self.reject(path, "writer embeds exactly one font program")?;
        }
        if role == "CIDFont" {
            let truetype = dict.get("Subtype").and_then(Object::as_name) == Some("CIDFontType2");
            if truetype != dict.contains_key("CIDToGIDMap") {
                self.reject(path, "CIDToGIDMap presence differs from writer font kind")?;
            }
        }
        if data.is_some() != stream {
            self.reject(path, "dictionary/stream shape differs from writer")?;
        }
        for key in required.split_whitespace() {
            if !dict.contains_key(key) {
                self.reject(&format!("{path}/{key}"), "required writer entry missing")?;
            }
        }
        let mut keys = dict.keys().collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            let child_path = format!("{path}/{}", safe_name(key));
            if !allowed.split_whitespace().any(|k| k == key) {
                self.reject(&child_path, "entry is not emitted by Publisher")?;
                continue;
            }
            self.entry(role, key, &dict[key], &child_path, depth + 1)?;
        }
        if let Some(data) = data {
            self.stream(role, &dict, &data, path)?;
        }
        Ok(())
    }

    fn stream(
        &mut self,
        role: &str,
        dict: &HashMap<String, Object>,
        data: &[u8],
        path: &str,
    ) -> Result<(), String> {
        if u64::try_from(data.len()).unwrap_or(u64::MAX) > super::MAX_CANDIDATE_BYTES {
            return Err(format!("{path}: Publisher profile stream limit"));
        }
        if !matches!(dict.get("Length"), Some(Object::Integer(length)) if usize::try_from(*length).ok() == Some(data.len()))
        {
            self.reject(
                &format!("{path}/Length"),
                "expected direct exact raw stream length",
            )?;
        }
        if matches!(role, "Content" | "Form") && !dict.contains_key("Filter") {
            self.content(data, path)?;
        }
        if matches!(role, "Image" | "Mask") {
            // Unsupported filter/predictor shapes already have findings;
            // never hand their bytes to another decoder in this pass.
            let supported_filter = !dict.contains_key("Filter")
                || matches!(dict.get("Filter"), Some(Object::Name(name)) if name == "FlateDecode");
            if supported_filter && !dict.contains_key("DecodeParms") {
                let dimensions = ["Width", "Height"].map(|key| match dict.get(key) {
                    Some(Object::Integer(value)) => u32::try_from(*value).ok(),
                    _ => None,
                });
                let sample_count = dimensions[0]
                    .zip(dimensions[1])
                    .and_then(|(width, height)| production_raster_pixels(width, height).ok())
                    .and_then(|pixels| pixels.checked_mul(if role == "Image" { 4 } else { 1 }));
                if let Some(expected) = sample_count {
                    claim_decoded_samples(&mut self.decoded_samples, expected)?;
                    let result = if dict.contains_key("Filter") {
                        exact_flate_samples(data, expected)
                    } else if data.len() == expected {
                        Ok(())
                    } else {
                        Err("raw sample length differs from exact dimensions".into())
                    };
                    if let Err(reason) = result {
                        self.reject(path, &reason)?;
                    }
                } else {
                    self.reject(path, "image dimensions exceed shared raster admission")?;
                }
            }
        }
        if role == "ShadowForm" && data != b"/AlphaPixels Do" {
            self.reject(path, "shadow mask Form must paint only its alpha image")?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keep the closed child-role and leaf schema together for census updates"
    )]
    fn entry(
        &mut self,
        role: &'static str,
        key: &str,
        value: &Object,
        path: &str,
        depth: usize,
    ) -> Result<(), String> {
        let child_role = match (role, key) {
            ("Trailer", "Root") => Some("Catalog"),
            ("Trailer", "Info") => Some("Info"),
            ("Catalog", "Pages") => Some("Pages"),
            ("Catalog", "Metadata") => Some("Metadata"),
            ("Page", "Contents") => Some("Content"),
            ("Page", "Resources") => Some("Resources"),
            ("Form", "Resources") => Some("FormResources"),
            ("Form", "Group") => Some("Group"),
            ("Image", "SMask") => Some("Mask"),
            ("ShadowState", "SMask") => Some("ShadowMask"),
            ("ShadowMask", "G") => Some("ShadowForm"),
            ("ShadowForm", "Resources") => Some("ShadowResources"),
            ("ShadowForm", "Group") => Some("ShadowGroup"),
            ("Type0", "ToUnicode") => Some("CMap"),
            ("CIDFont", "FontDescriptor") => Some("Descriptor"),
            ("CIDFont", "CIDSystemInfo") => Some("SystemInfo"),
            ("Descriptor", "FontFile2") => Some("FontFile2"),
            ("Descriptor", "FontFile3") => Some("FontFile3"),
            ("Descriptor", "CIDSet") => Some("Raw"),
            ("OutputIntent", "DestOutputProfile") => Some("ICC"),
            _ => None,
        };
        if let Some(child_role) = child_role {
            let direct = matches!(
                child_role,
                "Resources"
                    | "FormResources"
                    | "Group"
                    | "SystemInfo"
                    | "ShadowMask"
                    | "ShadowResources"
                    | "ShadowGroup"
            );
            if direct != matches!(value, Object::Dictionary(_))
                || (!direct && !matches!(value, Object::Reference(_)))
            {
                self.reject(path, "direct/indirect representation differs from writer")?;
            }
            return self.walk(value.clone(), child_role, path, depth);
        }
        if matches!(role, "Resources" | "FormResources" | "ShadowResources") {
            let child = match key {
                "ExtGState" => "Alpha",
                "Font" => "Type0",
                "XObject" if role == "FormResources" => "FormPaint",
                "XObject" if role == "ShadowResources" => "Mask",
                "XObject" => "XObject",
                _ => unreachable!(),
            };
            let Object::Dictionary(map) = value else {
                return self.reject(path, "resource map must be direct");
            };
            if role == "ShadowResources" && (map.len() != 1 || !map.contains_key("AlphaPixels")) {
                self.reject(
                    path,
                    "shadow mask resources contain exactly one alpha image",
                )?;
            }
            let mut names = map.keys().collect::<Vec<_>>();
            names.sort();
            for name in names {
                if !matches!(map[name], Object::Reference(_)) {
                    self.reject(path, "resource value must be indirect")?;
                }
                self.walk(
                    map[name].clone(),
                    child,
                    &format!("{path}/{}", safe_name(name)),
                    depth,
                )?;
            }
            return Ok(());
        }
        if let Some(child) = match (role, key) {
            ("Pages", "Kids") => Some("Page"),
            ("Catalog", "OutputIntents") => Some("OutputIntent"),
            ("Type0", "DescendantFonts") => Some("CIDFont"),
            _ => None,
        } {
            let Object::Array(values) = value else {
                return self.reject(path, "expected direct writer array");
            };
            if child != "Page" && values.len() != 1 {
                self.reject(path, "writer emits exactly one entry")?;
            }
            for (index, value) in values.iter().enumerate() {
                if (child == "OutputIntent") != matches!(value, Object::Dictionary(_))
                    || (child != "OutputIntent" && !matches!(value, Object::Reference(_)))
                {
                    self.reject(path, "array item representation differs from writer")?;
                }
                self.walk(value.clone(), child, &format!("{path}[{index}]"), depth)?;
            }
            return Ok(());
        }
        if role == "Page" && key == "Parent" {
            return if matches!(value, Object::Reference(r) if r.r#gen == 0) {
                let catalog = self.document.catalog().map_err(|error| error.to_string())?;
                if catalog
                    .as_dict()
                    .and_then(|dictionary| dictionary.get("Pages"))
                    != Some(value)
                {
                    self.reject(
                        path,
                        "writer pages share the catalog's flat page-tree parent",
                    )?;
                }
                self.walk(value.clone(), "Pages", path, depth)
            } else {
                self.reject(path, "page parent must be an indirect reference")
            };
        }
        if let Some(expected) = expected_names(role, key) {
            if !value
                .as_name()
                .is_some_and(|n| expected.split_whitespace().any(|e| e == n))
            {
                self.reject(path, "name is outside the writer profile")?;
            }
            return Ok(());
        }
        let valid = match key {
            "BitsPerComponent" => matches!(value, Object::Integer(8)),
            "N" => matches!(value, Object::Integer(4)),
            "Width" | "Height" => matches!(value, Object::Integer(n) if *n > 0),
            "I" => matches!(value, Object::Boolean(true)),
            "K" if matches!(role, "Group" | "ShadowGroup") => {
                matches!(value, Object::Boolean(false))
            }
            "CA" | "ca" => number(value).is_some_and(|n| (0.0..=1.0).contains(&n)),
            "Supplement" => matches!(value, Object::Integer(0)),
            "Registry" => matches!(value, Object::String(bytes) if bytes == b"Adobe"),
            "Ordering" => matches!(value, Object::String(bytes) if bytes == b"Identity"),
            "StemV" => number(value) == Some(80.0),
            "DW" => number(value) == Some(1000.0),
            "Length" | "Length1" | "Count" | "Size" | "Flags" => {
                matches!(value, Object::Integer(n) if *n >= 0)
            }
            "BBox" if role == "ShadowForm" => {
                matches!(value, Object::Array(a) if a.iter().map(number).collect::<Vec<_>>() == vec![Some(0.0), Some(0.0), Some(1.0), Some(1.0)])
            }
            "MediaBox" | "BleedBox" | "TrimBox" | "BBox" | "FontBBox" => {
                matches!(value, Object::Array(a) if a.len() == 4 && a.iter().all(|o| number(o).is_some()))
            }
            "W" => {
                matches!(value, Object::Array(a) if a.len() == 2 && matches!(a[0], Object::Integer(0)) && matches!(&a[1], Object::Array(widths) if widths.iter().all(|o| number(o).is_some())))
            }
            "ID" => {
                matches!(value, Object::Array(a) if a.len() == 2 && a.iter().all(|v| matches!(v, Object::String(_))))
            }
            "BaseFont" | "FontName" => matches!(value, Object::Name(_)),
            "ItalicAngle" | "Ascent" | "Descent" | "CapHeight" => number(value).is_some(),
            _ => matches!(value, Object::String(_)),
        };
        if !valid {
            self.reject(
                path,
                "value shape or fixed value is outside the writer profile",
            )?;
        }
        Ok(())
    }

    fn content(&mut self, bytes: &[u8], path: &str) -> Result<(), String> {
        let mut input = bytes;
        let mut operands = Vec::new();
        while !input.is_empty() {
            input = trim_pdf(input);
            if input.is_empty() {
                break;
            }
            // Bound delimiters before handing a composite operand to the parser,
            // which otherwise recurses without a caller-controlled depth limit.
            let first = input[0];
            if matches!(
                first,
                b'/' | b'(' | b'<' | b'[' | b'+' | b'-' | b'.' | b'0'..=b'9'
            ) {
                let end = operand_end(input).map_err(|error| format!("{path}: {error}"))?;
                let (rest, value) = parse_object(&input[..end])
                    .map_err(|_| format!("{path}: malformed content operand"))?;
                if !trim_pdf(rest).is_empty() {
                    return self.reject(path, "unconsumed content operand");
                }
                operands.push(value);
                if operands.len() > 8 {
                    return self.reject(path, "writer operator operand limit");
                }
                input = &input[end..];
            } else {
                let end = input
                    .iter()
                    .position(|b| delimiter(*b))
                    .unwrap_or(input.len());
                if end == 0 {
                    return self.reject(path, "unexpected content delimiter");
                }
                let name = std::str::from_utf8(&input[..end]).unwrap_or("non-ASCII");
                self.operators += 1;
                if self.operators > MAX_OPERATORS {
                    return Err("Publisher profile operator limit".into());
                }
                if !writer_operator(name, &operands) {
                    self.reject(
                        &format!("{path}/operator[{}]", self.operators),
                        &format!("unsupported operator or operands: {}", safe_name(name)),
                    )?;
                }
                operands.clear();
                input = &input[end..];
            }
        }
        if !operands.is_empty() {
            self.reject(path, "trailing content operands")?;
        }
        Ok(())
    }
}

fn schema(role: &str) -> (&'static str, &'static str, bool) {
    match role {
        "Trailer" => ("Size Root Info ID", "Size Root", false),
        "Catalog" => ("Type Pages Metadata OutputIntents", "Type Pages", false),
        "Pages" => ("Type Kids Count", "Type Kids Count", false),
        "Page" => (
            "Type Parent MediaBox BleedBox TrimBox Contents Resources PublisherV2BleedIntent",
            "Type Parent MediaBox Contents",
            false,
        ),
        "Resources" | "FormResources" => ("ExtGState XObject Font", "", false),
        "Form" | "ShadowForm" => (
            "Type Subtype BBox Resources Group Length",
            "Type Subtype BBox Resources Group Length",
            true,
        ),
        "Group" | "ShadowGroup" => ("Type S I K CS", "Type S I K CS", false),
        "ShadowState" => ("Type SMask", "Type SMask", false),
        "ShadowMask" => ("Type S G", "Type S G", false),
        "ShadowResources" => ("XObject", "XObject", false),
        "Image" => (
            "Type Subtype Width Height ColorSpace BitsPerComponent SMask Filter Length",
            "Type Subtype Width Height ColorSpace BitsPerComponent Length",
            true,
        ),
        "Mask" => (
            "Type Subtype Width Height ColorSpace BitsPerComponent Filter Length",
            "Type Subtype Width Height ColorSpace BitsPerComponent Length",
            true,
        ),
        "Alpha" => ("Type CA ca", "Type ca", false),
        "Type0" => (
            "Type Subtype BaseFont Encoding DescendantFonts ToUnicode",
            "Type Subtype BaseFont Encoding DescendantFonts ToUnicode",
            false,
        ),
        "CIDFont" => (
            "Type Subtype BaseFont CIDSystemInfo FontDescriptor DW W CIDToGIDMap",
            "Type Subtype BaseFont CIDSystemInfo FontDescriptor DW W",
            false,
        ),
        "Descriptor" => (
            "Type FontName Flags FontBBox ItalicAngle Ascent Descent CapHeight StemV CIDSet FontFile2 FontFile3",
            "Type FontName Flags FontBBox ItalicAngle Ascent Descent CapHeight StemV CIDSet",
            false,
        ),
        "SystemInfo" => (
            "Registry Ordering Supplement",
            "Registry Ordering Supplement",
            false,
        ),
        "FontFile2" => ("Length Length1", "Length Length1", true),
        "FontFile3" => ("Length Subtype", "Length Subtype", true),
        "CMap" => ("Type Length", "Type Length", true),
        "Content" | "Raw" => ("Length", "Length", true),
        "ICC" => ("N Length", "N Length", true),
        "Metadata" => ("Type Subtype Length", "Type Subtype Length", true),
        "OutputIntent" => (
            "Type S OutputConditionIdentifier Info DestOutputProfile",
            "Type S OutputConditionIdentifier Info DestOutputProfile",
            false,
        ),
        "Info" => (
            "Title CreationDate ModDate Creator Producer Trapped GTS_PDFXVersion",
            "Title CreationDate ModDate Creator Producer Trapped GTS_PDFXVersion",
            false,
        ),
        _ => unreachable!("closed role set"),
    }
}

fn expected_names(role: &str, key: &str) -> Option<&'static str> {
    match (role, key) {
        ("Catalog", "Type") => Some("Catalog"),
        ("Pages", "Type") => Some("Pages"),
        ("Page", "Type") => Some("Page"),
        ("Form" | "ShadowForm" | "Image" | "Mask", "Type") => Some("XObject"),
        ("Form" | "ShadowForm", "Subtype") => Some("Form"),
        ("Image" | "Mask", "Subtype") => Some("Image"),
        ("Image" | "Mask", "Filter") => Some("FlateDecode"),
        ("Image", "ColorSpace") => Some("DeviceCMYK"),
        ("Mask", "ColorSpace") | ("ShadowGroup", "CS") => Some("DeviceGray"),
        ("Group" | "ShadowGroup", "S") => Some("Transparency"),
        ("Group" | "ShadowGroup", "Type") => Some("Group"),
        ("ShadowMask", "S") => Some("Luminosity"),
        ("ShadowMask", "Type") => Some("Mask"),
        ("Group", "CS") => Some("DeviceCMYK DeviceRGB"),
        ("Alpha" | "ShadowState", "Type") => Some("ExtGState"),
        ("Type0" | "CIDFont", "Type") => Some("Font"),
        ("Type0", "Subtype") => Some("Type0"),
        ("Type0", "Encoding") => Some("Identity-H"),
        ("CIDFont", "Subtype") => Some("CIDFontType2 CIDFontType0"),
        ("CIDFont", "CIDToGIDMap") => Some("Identity"),
        ("Descriptor", "Type") => Some("FontDescriptor"),
        ("FontFile3", "Subtype") => Some("CIDFontType0C"),
        ("CMap", "Type") => Some("CMap"),
        ("Metadata", "Type") => Some("Metadata"),
        ("Metadata", "Subtype") => Some("XML"),
        ("OutputIntent", "Type") => Some("OutputIntent"),
        ("OutputIntent", "S") => Some("GTS_PDFX"),
        ("Info", "Trapped") => Some("False"),
        _ => None,
    }
}

fn claim_decoded_samples(used: &mut usize, expected: usize) -> Result<(), String> {
    *used = used
        .checked_add(expected)
        .filter(|total| *total <= MAX_RETAINED_RASTER_BYTES)
        .ok_or("Publisher profile aggregate decoded sample limit")?;
    Ok(())
}

// The writer uses one complete zlib stream, with no predictors, dictionaries,
// concatenated members or padding. Count samples in a fixed scratch buffer;
// even a tiny malicious stream cannot allocate its advertised expanded size.
fn exact_flate_samples(bytes: &[u8], expected: usize) -> Result<(), String> {
    let mut decoder = Decompress::new(true);
    let mut scratch = [0_u8; 16 * 1024];
    let mut consumed = 0_usize;
    let mut produced = 0_usize;
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(&bytes[consumed..], &mut scratch, FlushDecompress::None)
            .map_err(|_| "malformed lossless image stream")?;
        let read = usize::try_from(decoder.total_in() - before_in)
            .map_err(|_| "lossless input count overflow")?;
        let written = usize::try_from(decoder.total_out() - before_out)
            .map_err(|_| "lossless output count overflow")?;
        consumed = consumed
            .checked_add(read)
            .ok_or("lossless input count overflow")?;
        produced = produced
            .checked_add(written)
            .ok_or("lossless output count overflow")?;
        if produced > expected {
            return Err("inflated sample length exceeds exact dimensions".into());
        }
        if status == Status::StreamEnd {
            return if consumed == bytes.len() && produced == expected {
                Ok(())
            } else {
                Err("lossless stream has trailing bytes or inexact sample length".into())
            };
        }
        if read == 0 && written == 0 {
            return Err("truncated lossless image stream".into());
        }
    }
}

fn safe_name(name: &str) -> String {
    name.chars()
        .take(64)
        .map(|c| if c.is_ascii_graphic() { c } else { '?' })
        .collect()
}
#[expect(
    clippy::cast_precision_loss,
    reason = "only finite numeric shape and the zero-to-one alpha range use this conversion, not exact integer identity"
)]
fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(n) if n.is_finite() => Some(*n),
        _ => None,
    }
}
fn delimiter(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            0 | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}
fn trim_pdf(mut bytes: &[u8]) -> &[u8] {
    loop {
        while bytes
            .first()
            .is_some_and(|b| b.is_ascii_whitespace() || *b == 0)
        {
            bytes = &bytes[1..];
        }
        if bytes.first() != Some(&b'%') {
            return bytes;
        }
        bytes = &bytes[bytes
            .iter()
            .position(|b| matches!(b, b'\r' | b'\n'))
            .unwrap_or(bytes.len())..];
    }
}

// Locate one operand without interpreting payload text as operators. Composite
// nesting and literal-string nesting are bounded before parse_object is called.
fn operand_end(bytes: &[u8]) -> Result<usize, String> {
    let mut index = 0;
    let mut stack = Vec::new();
    loop {
        let Some(&byte) = bytes.get(index) else {
            return if stack.is_empty() {
                Ok(index)
            } else {
                Err("unclosed content operand".into())
            };
        };
        match byte {
            b'(' => {
                let mut nesting = 1;
                index += 1;
                while nesting > 0 {
                    match bytes.get(index) {
                        Some(b'\\') => {
                            index += 2;
                            continue;
                        }
                        Some(b'(') => {
                            nesting += 1;
                            if nesting > MAX_DEPTH {
                                return Err("content string nesting limit".into());
                            }
                        }
                        Some(b')') => nesting -= 1,
                        None => return Err("unclosed content string".into()),
                        _ => {}
                    }
                    index += 1;
                }
            }
            b'<' if bytes.get(index + 1) == Some(&b'<') => {
                stack.push(b'>');
                index += 2;
            }
            b'<' => {
                index += 1;
                while bytes.get(index).is_some_and(|b| *b != b'>') {
                    index += 1;
                }
                if index == bytes.len() {
                    return Err("unclosed hex string".into());
                }
                index += 1;
            }
            b'[' => {
                stack.push(b']');
                index += 1;
            }
            b']' | b'>' => {
                if stack.pop() != Some(byte) {
                    return Err("unbalanced content operand".into());
                }
                index += 1;
                if byte == b'>' {
                    if bytes.get(index) != Some(&b'>') {
                        return Err("unbalanced content dictionary".into());
                    }
                    index += 1;
                }
            }
            b'%' => {
                while bytes
                    .get(index)
                    .is_some_and(|b| !matches!(b, b'\r' | b'\n'))
                {
                    index += 1;
                }
            }
            _ => {
                if index == 0 && byte == b'/' {
                    index += 1;
                }
                if stack.is_empty() {
                    while bytes.get(index).is_some_and(|b| !delimiter(*b)) {
                        index += 1;
                    }
                    return Ok(index);
                }
                index += 1;
            }
        }
        if stack.len() > MAX_DEPTH {
            return Err("content operand nesting limit".into());
        }
        if stack.is_empty() {
            return Ok(index);
        }
    }
}

fn writer_operator(name: &str, operands: &[Object]) -> bool {
    let numeric = |count| operands.len() == count && operands.iter().all(|o| number(o).is_some());
    match name {
        "q" | "Q" | "h" | "f" | "S" | "W" | "n" | "BT" | "ET" | "EMC" => operands.is_empty(),
        "cm" | "c" | "Tm" => numeric(6),
        "k" | "K" | "re" => numeric(4),
        "rg" | "RG" => numeric(3),
        "m" | "l" | "Td" => numeric(2),
        "w" => numeric(1),
        "Tr" => matches!(operands, [Object::Integer(0)]),
        "gs" | "Do" => matches!(operands, [Object::Name(_)]),
        "Tf" => {
            operands.len() == 2
                && matches!(&operands[0], Object::Name(_))
                && number(&operands[1]).is_some()
        }
        "Tj" => matches!(operands, [Object::String(_)]),
        "BMC" => matches!(operands, [Object::Name(n)] if n == "Artifact"),
        "BDC" => {
            matches!(operands, [Object::Name(n), Object::Dictionary(d)] if n == "Span" && d.len() == 1 && matches!(d.get("ActualText"), Some(Object::String(_))))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_RETAINED_RASTER_BYTES, audit_publisher_writer_output, claim_decoded_samples};
    use pdf_writer::{Finish, Name, Pdf, Rect, Ref};

    fn candidate(fault: &str) -> Vec<u8> {
        candidate_content(fault, b"0 0 1 1 re f")
    }

    fn candidate_content(fault: &str, content: &[u8]) -> Vec<u8> {
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        {
            let mut catalog = pdf.catalog(Ref::new(1));
            catalog.pages(Ref::new(2));
            if fault == "catalog" {
                catalog.pair(Name(b"UnknownCatalog"), true);
            }
            if fault == "optional-content" {
                let mut properties = catalog.insert(Name(b"OCProperties")).dict();
                properties.insert(Name(b"OCGs")).array();
                properties.insert(Name(b"D")).dict();
            }
        }
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        {
            let mut page = pdf.page(Ref::new(3));
            page.parent(Ref::new(2))
                .media_box(Rect::new(0., 0., 100., 100.))
                .contents(Ref::new(4));
            if fault == "page" {
                page.pair(Name(b"UnknownPage"), true);
            }
            if fault == "payload" {
                page.pair(Name(b"PublisherV2BleedIntent"), Ref::new(5));
            }
            let mut resources = page.resources();
            if fault == "resource" {
                resources.pair(Name(b"UnknownResource"), true);
            }
            if matches!(
                fault,
                "depth"
                    | "filter"
                    | "jpx-filter"
                    | "xobject"
                    | "nested"
                    | "form"
                    | "form-operator"
                    | "form-cycle"
            ) {
                resources.x_objects().pair(Name(b"Unused"), Ref::new(5));
            }
            if fault == "blend-mode" {
                resources
                    .ext_g_states()
                    .pair(Name(b"UnusedAlpha"), Ref::new(5));
            }
            if fault == "font" {
                resources.fonts().pair(Name(b"UnusedFont"), Ref::new(5));
            }
        }
        pdf.stream(
            Ref::new(4),
            if fault == "operator" {
                b"0 0 1 1 re f Mystery"
            } else {
                content
            },
        );
        if fault == "payload" {
            pdf.stream(Ref::new(5), b"unexpected payload reference");
        } else if matches!(fault, "nested" | "form" | "form-operator" | "form-cycle") {
            let mut form = pdf.form_xobject(
                Ref::new(5),
                if fault == "form-operator" {
                    b"Mystery"
                } else {
                    b"q Q"
                },
            );
            form.bbox(Rect::new(0., 0., 10., 10.));
            form.group()
                .transparency()
                .isolated(true)
                .knockout(false)
                .color_space()
                .device_cmyk();
            if fault == "nested" {
                form.resources().pair(Name(b"UnknownNested"), true);
            } else if fault == "form-cycle" {
                form.resources()
                    .x_objects()
                    .pair(Name(b"Cycle"), Ref::new(5));
            } else {
                form.resources();
            }
            form.finish();
        } else if fault == "font" {
            pdf.type1_font(Ref::new(5)).base_font(Name(b"Helvetica"));
        } else if fault == "blend-mode" {
            pdf.ext_graphics(Ref::new(5))
                .non_stroking_alpha(0.5)
                .blend_mode(pdf_writer::types::BlendMode::Multiply);
        } else if matches!(fault, "depth" | "filter" | "jpx-filter" | "xobject") {
            write_candidate_image(&mut pdf, fault);
        }
        pdf.finish()
    }

    fn write_candidate_image(pdf: &mut Pdf, fault: &str) {
        let mut stream = pdf.stream(Ref::new(5), &[0; 4]);
        stream.pair(Name(b"Type"), Name(b"XObject"));
        stream.pair(
            Name(b"Subtype"),
            Name(if fault == "xobject" {
                b"Unexpected"
            } else {
                b"Image"
            }),
        );
        stream
            .pair(Name(b"Width"), 1)
            .pair(Name(b"Height"), 1)
            .pair(Name(b"ColorSpace"), Name(b"DeviceCMYK"));
        stream.pair(
            Name(b"BitsPerComponent"),
            if fault == "depth" { 16 } else { 8 },
        );
        if fault == "filter" {
            stream.pair(Name(b"Filter"), Name(b"FlateDecode"));
        }
        if fault == "jpx-filter" {
            // This is a dictionary rejection probe, not valid JPEG 2000.
            // The writer profile must refuse this Filter value without
            // interpreting payload bytes or claiming PDF/X invalidity.
            stream.pair(Name(b"Filter"), Name(b"JPXDecode"));
        }
    }

    #[test]
    fn admits_minimal_writer_shape() {
        assert!(
            audit_publisher_writer_output(&candidate(""))
                .unwrap()
                .findings
                .is_empty()
        );
    }

    #[test]
    fn rejects_non_emitted_blend_optional_content_and_jpx_keys() {
        for (fault, advertised_entry, resource_path, key) in [
            (
                "blend-mode",
                "/BM /Multiply",
                "/ExtGState/UnusedAlpha",
                "/BM:",
            ),
            (
                "optional-content",
                "/OCProperties",
                "trailer/Root",
                "/OCProperties:",
            ),
            (
                "jpx-filter",
                "/Filter /JPXDecode",
                "/XObject/Unused",
                "/Filter:",
            ),
        ] {
            let bytes = candidate(fault);
            assert!(
                bytes
                    .windows(advertised_entry.len())
                    .any(|part| part == advertised_entry.as_bytes()),
                "missing advertised fixture entry for {fault}: {advertised_entry}"
            );
            let audit = audit_publisher_writer_output(&bytes).unwrap();
            assert!(
                audit.findings.iter().any(|finding| {
                    finding.contains(resource_path)
                        && finding.contains(key)
                        && finding.contains(if fault == "jpx-filter" {
                            "name is outside the writer profile"
                        } else {
                            "entry is not emitted by Publisher"
                        })
                }),
                "missing writer-scope rejection for {fault}: {:?}",
                audit.findings
            );
            assert_eq!(audit.artifact_sha256, super::super::Hash32::of(&bytes).0);
        }
    }

    fn shadow_candidate(fault: &str) -> Vec<u8> {
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        {
            let mut page = pdf.page(Ref::new(3));
            page.parent(Ref::new(2))
                .media_box(Rect::new(0., 0., 100., 100.))
                .contents(Ref::new(4));
            // Deliberately unused: nested resources still require inspection.
            page.resources()
                .ext_g_states()
                .pair(Name(b"Shadow"), Ref::new(5));
        }
        pdf.stream(Ref::new(4), b"q Q");
        {
            let mut state = pdf.ext_graphics(Ref::new(5));
            let mut mask = state.soft_mask();
            mask.subtype(if fault == "alpha-subtype" {
                pdf_writer::types::MaskType::Alpha
            } else {
                pdf_writer::types::MaskType::Luminosity
            })
            .group(Ref::new(6));
            if fault == "transfer" {
                mask.pair(Name(b"TR"), Name(b"Identity"));
            }
        }
        {
            let mut form = pdf.form_xobject(
                Ref::new(6),
                if fault == "operator" {
                    b"/AlphaPixels Do q Q\n"
                } else {
                    b"/AlphaPixels Do"
                },
            );
            form.bbox(Rect::new(
                0.,
                0.,
                if fault == "bounds" { 2. } else { 1. },
                1.,
            ));
            {
                let mut group = form.group();
                group.transparency().isolated(true).knockout(false);
                if fault == "color-space" {
                    group.color_space().device_rgb();
                } else {
                    group.color_space().device_gray();
                }
            }
            let mut resources = form.resources();
            if fault == "resource" {
                resources.fonts().pair(Name(b"Unexpected"), Ref::new(7));
            }
            let mut images = resources.x_objects();
            images.pair(
                Name(b"AlphaPixels"),
                if fault == "cycle" {
                    Ref::new(6)
                } else {
                    Ref::new(7)
                },
            );
            if fault == "extra-image" {
                images.pair(Name(b"Extra"), Ref::new(7));
            }
        }
        let mut image = pdf.image_xobject(Ref::new(7), &[128]);
        image
            .width(1)
            .height(1)
            .bits_per_component(if fault == "depth" { 16 } else { 8 });
        image.color_space().device_gray();
        image.finish();
        pdf.finish()
    }

    #[test]
    fn admits_only_the_emitted_picture_shadow_mask_graph() {
        let positive = audit_publisher_writer_output(&shadow_candidate("")).unwrap();
        assert!(positive.findings.is_empty(), "{:?}", positive.findings);
        for fault in [
            "alpha-subtype",
            "transfer",
            "operator",
            "bounds",
            "color-space",
            "resource",
            "cycle",
            "extra-image",
            "depth",
        ] {
            let audit = audit_publisher_writer_output(&shadow_candidate(fault)).unwrap();
            assert!(!audit.findings.is_empty(), "unrejected shadow {fault}");
            assert!(
                audit
                    .findings
                    .iter()
                    .any(|finding| finding.contains("/SMask")),
                "missing shadow path: {:?}",
                audit.findings
            );
        }
    }

    fn zlib(samples: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(samples).unwrap();
        encoder.finish().unwrap()
    }

    fn raster_candidate(
        samples: &[u8],
        mask: Option<&[u8]>,
        filter: &str,
        width: i32,
        height: i32,
    ) -> Vec<u8> {
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        {
            let mut page = pdf.page(Ref::new(3));
            page.parent(Ref::new(2))
                .media_box(Rect::new(0., 0., 100., 100.))
                .contents(Ref::new(4));
            page.resources()
                .x_objects()
                .pair(Name(b"Picture"), Ref::new(5));
        }
        {
            let mut content = pdf.stream(Ref::new(4), b"q /Picture Do Q");
            if filter == "content-filter" {
                content.pair(Name(b"Filter"), Name(b"FlateDecode"));
            }
        }
        {
            let mut image = pdf.image_xobject(Ref::new(5), samples);
            image.width(width).height(height).bits_per_component(8);
            image.color_space().device_cmyk();
            if mask.is_some() {
                image.s_mask(Ref::new(6));
            }
            match filter {
                "raw" | "content-filter" => {}
                "array" => {
                    image
                        .insert(Name(b"Filter"))
                        .array()
                        .item(Name(b"FlateDecode"));
                }
                "lzw" => {
                    image.pair(Name(b"Filter"), Name(b"LZWDecode"));
                }
                _ => {
                    image.pair(Name(b"Filter"), Name(b"FlateDecode"));
                }
            }
            if filter == "predictor" {
                image
                    .insert(Name(b"DecodeParms"))
                    .dict()
                    .pair(Name(b"Predictor"), 12);
            }
        }
        if let Some(mask) = mask {
            let mut image = pdf.image_xobject(Ref::new(6), mask);
            image.width(width).height(height).bits_per_component(8);
            image.color_space().device_gray();
            if filter != "raw" {
                image.pair(Name(b"Filter"), Name(b"FlateDecode"));
            }
        }
        pdf.finish()
    }

    #[test]
    fn admits_exact_raw_and_single_flate_cmyk_and_gray_samples() {
        let cmyk = [0, 20, 40, 60, 80, 100, 120, 140];
        let alpha = [255, 100];
        for bytes in [
            raster_candidate(&cmyk, Some(&alpha), "raw", 2, 1),
            raster_candidate(&zlib(&cmyk), Some(&zlib(&alpha)), "flate", 2, 1),
        ] {
            let audit = audit_publisher_writer_output(&bytes).unwrap();
            assert!(audit.findings.is_empty(), "{:?}", audit.findings);
            assert_eq!(audit.artifact_sha256, super::super::Hash32::of(&bytes).0);
        }
    }

    #[test]
    fn rejects_wrong_lossless_length_corruption_truncation_and_trailing_members() {
        let exact = zlib(&[0; 4]);
        let mut corrupt = exact.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        let truncated = exact[..exact.len() - 1].to_vec();
        let trailing = [exact.clone(), vec![0]].concat();
        let concatenated = [exact.clone(), exact].concat();
        for samples in [
            corrupt,
            truncated,
            trailing,
            concatenated,
            zlib(&[0; 3]),
            zlib(&vec![0; 40_000]),
        ] {
            let audit =
                audit_publisher_writer_output(&raster_candidate(&samples, None, "flate", 1, 1))
                    .unwrap();
            assert!(!audit.findings.is_empty(), "unrejected lossless payload");
        }
        let mask_failure = audit_publisher_writer_output(&raster_candidate(
            &zlib(&[0; 4]),
            Some(&zlib(&[0; 2])),
            "flate",
            1,
            1,
        ))
        .unwrap();
        assert!(
            mask_failure
                .findings
                .iter()
                .any(|finding| finding.contains("/SMask"))
        );
        assert!(
            !audit_publisher_writer_output(&raster_candidate(&[0; 3], None, "raw", 1, 1))
                .unwrap()
                .findings
                .is_empty()
        );
    }

    #[test]
    fn rejects_filter_arrays_other_codecs_predictors_and_nonimage_filters() {
        for filter in ["array", "lzw", "predictor", "content-filter"] {
            let bytes = raster_candidate(&zlib(&[0; 4]), None, filter, 1, 1);
            let audit = audit_publisher_writer_output(&bytes).unwrap();
            assert!(!audit.findings.is_empty(), "unrejected {filter}");
            assert!(
                audit
                    .findings
                    .iter()
                    .any(|finding| finding.contains("/Filter") || finding.contains("/DecodeParms"))
            );
        }
    }

    #[test]
    fn bounds_declared_dimensions_and_aggregate_samples_without_allocating_them() {
        for (width, height) in [(13_501, 1), (10_501, 13_500), (0, 1), (i32::MAX, i32::MAX)] {
            let audit =
                audit_publisher_writer_output(&raster_candidate(&[], None, "raw", width, height))
                    .unwrap();
            assert!(
                audit
                    .findings
                    .iter()
                    .any(|finding| finding.contains("shared raster admission"))
            );
        }
        let mut used = MAX_RETAINED_RASTER_BYTES - 4;
        claim_decoded_samples(&mut used, 4).unwrap();
        assert!(claim_decoded_samples(&mut used, 1).is_err());
        assert_eq!(used, MAX_RETAINED_RASTER_BYTES);
        used = usize::MAX;
        assert!(claim_decoded_samples(&mut used, 1).is_err());
    }

    #[test]
    fn rejects_unknown_constructs_including_unused_nested_resources() {
        for fault in [
            "catalog",
            "page",
            "resource",
            "operator",
            "depth",
            "filter",
            "font",
            "nested",
            "xobject",
            "form-operator",
            "form-cycle",
            "payload",
        ] {
            let bytes = candidate(fault);
            let audit = audit_publisher_writer_output(&bytes).unwrap();
            assert!(!audit.findings.is_empty(), "unrejected {fault}");
            assert!(
                audit.findings.iter().any(|f| f.contains("trailer/Root")),
                "path missing {fault}: {:?}",
                audit.findings
            );
            assert_eq!(audit.artifact_sha256, super::super::Hash32::of(&bytes).0);
            if fault == "nested" {
                assert!(audit.findings.iter().any(|f| f.contains("UnknownNested")));
            }
        }
    }

    #[test]
    fn admits_unused_transparency_form_and_does_not_parse_text_as_operators() {
        for bytes in [candidate("form"), candidate_content("", b"% Mystery\n/Artifact BMC EMC /Span << /ActualText (Mystery \\(quoted\\) <BI> [sh]) >> BDC BT /Font 12 Tf 0 Tr 1 0 0 1 0 0 Tm <0041> Tj ET EMC")] {
            let audit = audit_publisher_writer_output(&bytes).unwrap();
            assert!(audit.findings.is_empty(), "{:?}", audit.findings);
        }
    }

    #[test]
    fn rejects_unlisted_operators_and_extra_marked_content_properties() {
        for content in [
            b"0.5 g".as_slice(),
            b"BI /W 1 ID x EI",
            b"[] TJ",
            b"BX Mystery EX",
            b"/Span << /ActualText (OK) /Extra true >> BDC",
            b"0 0 1 1 re f Mystery q Q",
            b"1 0 0 1 0 0 cm trailing",
            b"1 2",
        ] {
            let audit = audit_publisher_writer_output(&candidate_content("", content)).unwrap();
            assert!(!audit.findings.is_empty(), "{content:?}");
        }
    }

    #[test]
    fn bounds_nested_content_operands_and_rejects_orphan_payload_objects() {
        let nested = format!("{}0{} Tj", "[".repeat(25), "]".repeat(25));
        let error =
            audit_publisher_writer_output(&candidate_content("", nested.as_bytes())).unwrap_err();
        assert!(error.contains("trailer/Root") && error.contains("nesting limit"));
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        pdf.pages(Ref::new(2)).kids([]).count(0);
        pdf.stream(Ref::new(3), b"unreachable payload");
        let audit = audit_publisher_writer_output(&pdf.finish()).unwrap();
        assert!(
            audit
                .findings
                .iter()
                .any(|f| f.contains("object 3") && f.contains("outside"))
        );
    }

    #[test]
    #[ignore = "requires explicit local writer artifacts via PUBLISHER_WRITER_PROFILE_CORPUS"]
    fn local_writer_artifact_census() {
        let corpus =
            std::env::var_os("PUBLISHER_WRITER_PROFILE_CORPUS").expect("explicit corpus paths");
        let paths = std::env::split_paths(&corpus).collect::<Vec<_>>();
        assert!(!paths.is_empty());
        for path in paths {
            let bytes = std::fs::read(&path).unwrap();
            let audit = audit_publisher_writer_output(&bytes).unwrap();
            assert!(
                audit.findings.is_empty(),
                "{}: {:?}",
                path.display(),
                audit.findings
            );
            println!(
                "{} SHA-256 {}",
                path.display(),
                super::super::Hash32(audit.artifact_sha256).hex()
            );
        }
    }
}
