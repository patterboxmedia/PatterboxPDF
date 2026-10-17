//! Complete metadata for bounded validation candidates. Time is captured by
//! the caller, so serialization remains deterministic for the same inputs.

use chrono::{DateTime, Datelike, Timelike, Utc};
use pdf_writer::{Date, TextStr, writers::DocumentInfo};
use std::fmt::Write;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PdfCandidateMetadata {
    title: String,
    created_at: DateTime<Utc>,
}

impl PdfCandidateMetadata {
    /// Captures the creation time of a newly generated PDF, not the source
    /// publication's original creation time. Reuse this value for reproducibility.
    #[must_use]
    pub fn for_export(title: &str) -> Self {
        Self {
            title: clean_title(title),
            created_at: Utc::now(),
        }
    }

    /// Supplies an explicit UTC timestamp for reproducible generation.
    /// Returns `None` for invalid dates or years outside PDF's four-digit range.
    /// PDF dates have whole seconds through 59. Fractional seconds are dropped;
    /// an RFC3339 leap second uses its preceding 59th second in both Info and XMP.
    #[must_use]
    pub fn at_utc(title: &str, timestamp: &str) -> Option<Self> {
        let date = DateTime::parse_from_rfc3339(timestamp)
            .ok()?
            .with_timezone(&Utc);
        (1..=9999).contains(&date.year()).then(|| Self {
            title: clean_title(title),
            created_at: date,
        })
    }

    pub(crate) fn title_byte_len(&self) -> usize {
        self.title.len()
    }

    /// PDF/X requires a meaningful title; ordinary metadata retains the caller's
    /// normalized value. `DocumentInfo` strings are outside the content-string limit.
    pub(crate) fn title_is_blank(&self) -> bool {
        self.title.trim().is_empty()
    }

    /// Existing deterministic fixture APIs describe synthetic validation probes.
    /// Their reference epoch must not be presented as a real export timestamp.
    #[cfg(feature = "publisher-adapter")]
    pub(crate) fn validation_probe() -> Self {
        Self::at_utc("Publisher V2 validation candidate", "2000-01-01T00:00:00Z")
            .expect("fixed valid reference epoch")
    }

    #[cfg(feature = "publisher-adapter")]
    pub(crate) fn xmp(&self, file_id: &[u8], producer: &str) -> Vec<u8> {
        self.xmp_for(file_id, producer, "Publisher V2", true)
    }

    pub(crate) fn xmp_for(
        &self,
        file_id: &[u8],
        producer: &str,
        creator: &str,
        pdfx: bool,
    ) -> Vec<u8> {
        let creator = xml_text(creator);
        let pdfx_namespace = if pdfx {
            "   xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\"\n"
        } else {
            ""
        };
        let pdfx_version = if pdfx {
            "   <pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>\n"
        } else {
            ""
        };
        let title = xml_text(&self.title);
        let producer = xml_text(producer);
        // Use the same whole second as PDF Info. Chrono's %S includes its
        // leap-second nanoseconds and can print 60 while second() returns 59.
        let date = format!(
            "{}{:02}Z",
            self.created_at.format("%Y-%m-%dT%H:%M:"),
            self.created_at.second()
        );
        let mut hex = String::with_capacity(32);
        for byte in file_id {
            write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
        }
        let id = format!(
            "uuid:{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        );
        format!(
            r#"<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
   xmlns:dc="http://purl.org/dc/elements/1.1/"
   xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
{pdfx_namespace}   xmlns:xmp="http://ns.adobe.com/xap/1.0/"
   xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">{title}</rdf:li></rdf:Alt></dc:title>
   <pdf:Producer>{producer}</pdf:Producer><pdf:Trapped>False</pdf:Trapped>
{pdfx_version}   <xmp:CreatorTool>{creator}</xmp:CreatorTool>
   <xmp:CreateDate>{date}</xmp:CreateDate><xmp:ModifyDate>{date}</xmp:ModifyDate>
   <xmp:MetadataDate>{date}</xmp:MetadataDate>
   <xmpMM:DocumentID>{id}</xmpMM:DocumentID><xmpMM:InstanceID>{id}</xmpMM:InstanceID>
   <xmpMM:VersionID>1</xmpMM:VersionID><xmpMM:RenditionClass>default</xmpMM:RenditionClass>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
        )
        .into_bytes()
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the constructor checks the PDF year range; chrono guarantees calendar component ranges"
    )]
    pub(crate) fn write_info(&self, info: &mut DocumentInfo<'_>) {
        let date = Date::new(self.created_at.year() as u16)
            .month(self.created_at.month() as u8)
            .day(self.created_at.day() as u8)
            .hour(self.created_at.hour() as u8)
            .minute(self.created_at.minute() as u8)
            .second(self.created_at.second() as u8)
            .utc_offset_hour(0)
            .utc_offset_minute(0);
        info.title(TextStr(&self.title))
            .creation_date(date)
            .modified_date(date);
    }
}

fn clean_title(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c == '\t'
                || c == '\n'
                || c == '\r'
                || (c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}')
            {
                c
            } else {
                '\u{fffd}'
            }
        })
        .collect()
}

fn xml_text(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            // XML 1.0 normalizes literal CR/CRLF. A character reference keeps
            // the decoded XMP value identical to the PDF Info text string.
            '\r' => "&#13;".to_owned(),
            c if c == '\t'
                || c == '\n'
                || c == '\r'
                || (c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}') =>
            {
                c.to_string()
            }
            _ => "\u{fffd}".to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_decoding_preserves_title_carriage_returns() {
        for title in ["a\rb\r\nc", "試\r\n験\r & <title>"] {
            let metadata = PdfCandidateMetadata::at_utc(title, "2026-10-06T00:00:00Z").unwrap();
            let xmp = String::from_utf8(metadata.xmp_for(
                &[0x12; 16],
                "PDF Library",
                "PDF Library",
                true,
            ))
            .unwrap();
            let decoded = roxmltree::Document::parse(&xmp).unwrap();
            let dc_title = decoded
                .descendants()
                .find(|n| n.has_tag_name(("http://purl.org/dc/elements/1.1/", "title")))
                .unwrap();
            let value = dc_title
                .descendants()
                .find(|n| n.has_tag_name(("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "li")))
                .unwrap()
                .text()
                .unwrap();
            assert_eq!(value, title);
        }
    }

    #[test]
    fn complete_metadata_escapes_titles_and_binds_dates_and_identity() {
        let metadata =
            PdfCandidateMetadata::at_utc("Notes & <試験>\u{1}", "2026-10-03T01:02:03Z").unwrap();
        let xmp =
            String::from_utf8(metadata.xmp_for(&[0x12; 16], "Publisher V2", "Publisher V2", true))
                .unwrap();
        assert!(xmp.contains("Notes &amp; &lt;試験&gt;\u{fffd}"));
        for field in ["CreateDate", "ModifyDate", "MetadataDate"] {
            assert!(xmp.contains(&format!("<xmp:{field}>2026-10-03T01:02:03Z</xmp:{field}>")));
        }
        assert!(xmp.contains("uuid:12121212-1212-1212-1212-121212121212"));
        assert!(xmp.contains("<pdf:Trapped>False</pdf:Trapped>"));
        assert!(xmp.contains("<xmpMM:VersionID>1</xmpMM:VersionID>"));
        assert!(xmp.contains("<xmpMM:RenditionClass>default</xmpMM:RenditionClass>"));
        let mut pdf = pdf_writer::Pdf::new();
        metadata.write_info(&mut pdf.document_info(pdf_writer::Ref::new(1)));
        let info = String::from_utf8_lossy(&pdf.finish()).into_owned();
        assert!(info.contains("/CreationDate (D:20261003010203Z)"));
        assert!(info.contains("/ModDate (D:20261003010203Z)"));
        assert!(PdfCandidateMetadata::at_utc("title", "2026-02-30T00:00:00Z").is_none());
    }

    #[test]
    fn leap_and_fractional_seconds_have_one_consistent_pdf_date() {
        for (input, xmp_date, info_date) in [
            (
                "2016-12-31T23:59:60Z",
                "2016-12-31T23:59:59Z",
                "D:20161231235959Z",
            ),
            (
                "2016-12-31T23:59:59.75Z",
                "2016-12-31T23:59:59Z",
                "D:20161231235959Z",
            ),
            (
                "2017-01-01T00:59:60+01:00",
                "2016-12-31T23:59:59Z",
                "D:20161231235959Z",
            ),
            (
                "9999-12-31T23:59:60Z",
                "9999-12-31T23:59:59Z",
                "D:99991231235959Z",
            ),
        ] {
            let metadata = PdfCandidateMetadata::at_utc("Whole seconds", input).unwrap();
            let xmp = String::from_utf8(metadata.xmp_for(
                &[0x12; 16],
                "PDF Library",
                "PDF Library",
                false,
            ))
            .unwrap();
            for field in ["CreateDate", "ModifyDate", "MetadataDate"] {
                assert!(
                    xmp.contains(&format!("<xmp:{field}>{xmp_date}</xmp:{field}>")),
                    "{input}: {field}"
                );
            }
            let mut pdf = pdf_writer::Pdf::new();
            metadata.write_info(&mut pdf.document_info(pdf_writer::Ref::new(1)));
            let info = String::from_utf8_lossy(&pdf.finish()).into_owned();
            for field in ["CreationDate", "ModDate"] {
                assert!(
                    info.contains(&format!("/{field} ({info_date})")),
                    "{input}: {field}"
                );
            }
        }
    }
}
