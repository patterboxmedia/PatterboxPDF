//! Reuse a publication's existing exact font choice for newly created text.

use document_core::AssetStore;
use document_core::commands::{Command, Transaction};
use document_core::model::{Document, FontFaceBinding, ObjectKind, TextStyle};
use document_core::package::is_supported_font_media_type;

pub(super) fn bind_new_text(
    document: &Document,
    assets: &AssetStore,
    transaction: &mut Transaction,
) {
    if !transaction.commands.iter().any(|command| {
        matches!(command, Command::AddObject { object, .. }
            if matches!(object.kind, ObjectKind::Text { .. } | ObjectKind::Table { .. }))
    }) {
        return;
    }
    let styles: Vec<_> = document
        .authored_objects()
        .filter_map(|object| match &object.kind {
            ObjectKind::Text { style, .. } => Some(style),
            ObjectKind::Table { table } => Some(&table.style),
            _ => None,
        })
        .collect();
    for command in &mut transaction.commands {
        let Command::AddObject { object, .. } = command else {
            continue;
        };
        let style = match &mut object.kind {
            ObjectKind::Text { style, .. } => style,
            ObjectKind::Table { table } => &mut table.style,
            _ => continue,
        };
        // An explicit exact face always wins. Duplication and paste have their
        // own commands and retain their existing bindings without this lookup.
        if style.font_face.is_none() {
            style.font_face = unique_face(&styles, assets, style);
        }
    }
}

fn unique_face(
    styles: &[&TextStyle],
    assets: &AssetStore,
    requested: &TextStyle,
) -> Option<FontFaceBinding> {
    let mut selected: Option<&FontFaceBinding> = None;
    for style in styles {
        if !style
            .font_family
            .trim()
            .eq_ignore_ascii_case(requested.font_family.trim())
        {
            continue;
        }
        let Some(face) = style.exact_font_face_for(requested.font_weight, requested.italic) else {
            continue;
        };
        if !assets
            .get(&face.source)
            .is_some_and(|asset| is_supported_font_media_type(&asset.media_type))
        {
            continue;
        }
        // A family label can refer to different versions of a font. Do not
        // silently pick one; leave the existing explicit-selection path intact.
        if selected.is_some_and(|previous| previous != face) {
            return None;
        }
        selected = Some(face);
    }
    selected.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApplyTransactionInput, CreateDocumentInput, EditorModule};
    use serde_json::json;

    fn add(
        editor: &mut EditorModule,
        table: bool,
        family: &str,
        weight: u16,
        italic: bool,
        face: Option<&FontFaceBinding>,
    ) {
        let snapshot = editor.snapshot().unwrap();
        let mut command = json!({
            "type": if table { "addTable" } else { "addText" },
            "pageId": snapshot.project.document.pages[0].id.as_str(),
            "name": "New object", "xUm": 10_000, "yUm": 10_000, "widthUm": 100_000, "heightUm": 40_000,
            "style": { "fontFamily": family, "fontWeight": weight, "italic": italic,
                "fontSizeMillipoints": 12_000, "alignment": "left", "lineHeightPerThousand": 1_200,
                "color": { "red": 0, "green": 0, "blue": 0, "alpha": 255 } }
        });
        if let Some(face) = face {
            command["style"]["fontFace"] =
                json!({ "source": face.source.as_str(), "faceIndex": face.face_index });
        }
        if table {
            command["rows"] = json!(2);
            command["columns"] = json!(2);
        } else {
            command["text"] = json!("New text");
            command["sizingMode"] = json!("fixed");
        }
        let input: ApplyTransactionInput = serde_json::from_value(json!({
            "expectedRevision": snapshot.project.revision.value(), "label": "Add object", "commands": [command]
        })).unwrap();
        editor.apply(input).unwrap();
    }

    fn setup() -> (EditorModule, FontFaceBinding) {
        let mut editor = EditorModule::new();
        editor
            .create(CreateDocumentInput {
                document_id: "font-reuse".into(),
                title: "Font reuse".into(),
                trim_width_um: 215_900,
                trim_height_um: 279_400,
                bleed_um: 0,
                page_count: 1,
            })
            .unwrap();
        let source = editor.assets.insert(
            "font/ttf",
            include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
        );
        let face = FontFaceBinding {
            source,
            face_index: 0,
        };
        add(&mut editor, false, "DejaVu Sans", 400, false, Some(&face));
        (editor, face)
    }

    fn last_face(editor: &EditorModule) -> Option<FontFaceBinding> {
        let snapshot = editor.snapshot().unwrap();
        match &snapshot.project.document.pages[0]
            .objects
            .last()
            .unwrap()
            .kind
        {
            ObjectKind::Text { style, .. } => style.font_face.clone(),
            ObjectKind::Table { table } => table.style.font_face.clone(),
            _ => panic!("expected text or table"),
        }
    }

    #[test]
    fn new_text_and_tables_reuse_the_saved_face_with_atomic_history() {
        let (mut editor, face) = setup();
        let before = editor.snapshot().unwrap();
        add(&mut editor, false, "dejavu sans", 400, false, None);
        assert_eq!(last_face(&editor), Some(face.clone()));
        let added = editor.snapshot().unwrap();
        editor.undo(added.project.revision.value()).unwrap();
        assert_eq!(
            editor.snapshot().unwrap().project.document,
            before.project.document
        );
        let revision = editor.snapshot().unwrap().project.revision.value();
        editor.redo(revision).unwrap();
        assert_eq!(
            editor.snapshot().unwrap().project.document,
            added.project.document
        );
        add(&mut editor, true, "DejaVu Sans", 400, false, None);
        assert_eq!(last_face(&editor), Some(face));
        assert_eq!(editor.assets.len(), 1);
    }

    #[test]
    fn unmatched_and_ambiguous_faces_still_require_a_choice() {
        let (mut editor, face) = setup();
        for (family, weight, italic) in [
            ("Arial", 400, false),
            ("DejaVu Sans", 700, false),
            ("DejaVu Sans", 400, true),
        ] {
            add(&mut editor, false, family, weight, italic, None);
            assert_eq!(last_face(&editor), None);
        }
        // Two distinct bound faces with the same authoring label are ambiguous.
        let other = FontFaceBinding {
            source: face.source.clone(),
            face_index: 1,
        };
        add(&mut editor, false, "DejaVu Sans", 400, false, Some(&other));
        assert_eq!(last_face(&editor), Some(other));
        add(&mut editor, false, "DejaVu Sans", 400, false, None);
        assert_eq!(last_face(&editor), None);
        add(&mut editor, true, "DejaVu Sans", 400, false, Some(&face));
        assert_eq!(last_face(&editor), Some(face));
    }
}
