//! Renderer-neutral layout boundary.
//!
//! Layout may depend on the document domain. It must not depend on a UI, Tauri,
//! a browser renderer, or a PDF implementation.

pub mod fonts;
pub mod geometry;
mod publication;
pub mod text;

pub use publication::{
    FontSubstitutionStatus, PreviewBudgetState, PublicationLayoutError,
    RequestedFontBindingPreview, RequestedFontRangeBindingPreview, ResolvedFitTextBox,
    ResolvedFontFacePreview, ResolvedGlyphPreview, ResolvedGlyphRunPreview, ResolvedPagePreview,
    ResolvedPageScene, ResolvedPasteboardScene, ResolvedPublication, ResolvedSceneObject,
    ResolvedSceneObjectKind, ResolvedTableCellPreview, ResolvedTablePreview,
    ResolvedTableCellFill, ResolvedTableBorderSegment,
    ResolvedTextBoxPreview, ResolvedTextLinePreview, ResolvedTextUnderlinePreview,
    TextBoxPreviewState, output_font_face_bindings, page_preview_font_face_bindings,
    preview_font_face_bindings, master_preview_font_face_bindings,
};
pub use text::{
    ExactFontRegistrationError, ExactFontResource, ResolvedFitTextLayout, ResolvedGlyphFallback,
    ResolvedGlyphOutline, ResolvedOutlineCommand,
};

#[cfg(test)]
mod tests {
    use document_core::Revision;

    use super::ResolvedPublication;

    #[test]
    fn resolved_publication_keeps_source_revision() {
        let revision = Revision::initial().next();
        assert_eq!(
            ResolvedPublication::for_revision(revision).revision(),
            revision
        );
    }
}
