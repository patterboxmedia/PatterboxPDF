use application_core::{
    ApplyTransactionInput, CreateDocumentInput, EditorCommandInput, EditorErrorCode, EditorModule,
    TextBoxSizingModeInput,
};
use document_core::model::{ObjectKind, TextBoxSizingMode, TextFrame};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "publisher-v2-text-box-sizing-{}-{sequence}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("remove stale test directory");
        }
        std::fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

fn create_editor() -> EditorModule {
    let mut editor = EditorModule::new();
    editor
        .create(CreateDocumentInput {
            document_id: "text-box-sizing".to_owned(),
            title: "Text Box Sizing".to_owned(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 1,
        })
        .expect("create publication");
    editor
}

fn transaction(
    expected_revision: u64,
    label: &str,
    commands: Vec<EditorCommandInput>,
) -> ApplyTransactionInput {
    ApplyTransactionInput {
        expected_revision,
        label: label.to_owned(),
        commands,
    }
}

#[test]
fn click_and_drag_intent_and_authoritative_conversion_round_trip() {
    let temporary = TempDirectory::new();
    let package = temporary.path().join("text-box-sizing.publisher-v2");
    let mut editor = create_editor();
    let page_id = editor.snapshot().expect("snapshot").project.document.pages[0]
        .id
        .as_str()
        .to_owned();
    let created = editor
        .apply(transaction(
            0,
            "Create click and drag text boxes",
            vec![
                EditorCommandInput::AddText {
                    page_id: Some(page_id.clone()),
                    name: "Clicked empty text box".to_owned(),
                    text: "  \n".to_owned(),
                    style: None,
                    sizing_mode: TextBoxSizingModeInput::Fit,
                    x_um: 10_000,
                    y_um: 10_000,
                    width_um: TextFrame::MIN_FIT_WIDTH_MICROMETRES,
                    height_um: TextFrame::MIN_FIT_HEIGHT_MICROMETRES,
                },
                EditorCommandInput::AddText {
                    page_id: Some(page_id),
                    name: "Dragged body".to_owned(),
                    text: "Line one\nLine two".to_owned(),
                    style: None,
                    sizing_mode: TextBoxSizingModeInput::Fixed,
                    x_um: 10_000,
                    y_um: 30_000,
                    width_um: 100_000,
                    height_um: 80_000,
                },
            ],
        ))
        .expect("represent click and drag intent canonically");
    let page = &created.project.document.pages[0];
    let fit_id = page.objects[0].id.as_str().to_owned();
    let fixed_id = page.objects[1].id.as_str().to_owned();
    assert!(matches!(
        &page.objects[0].kind,
        ObjectKind::Text { frame, .. } if frame.sizing_mode == TextBoxSizingMode::Fit
    ));
    assert!(matches!(
        &page.objects[1].kind,
        ObjectKind::Text { frame, .. } if frame.sizing_mode == TextBoxSizingMode::Fixed
    ));

    let before_conversion = page.objects[1].clone();
    let converted = editor
        .update_text_box_sizing(1, &fixed_id, TextBoxSizingModeInput::Fit)
        .expect("application resolves and commits authoritative Fit geometry");
    let fitted = converted.project.document.pages[0].objects[1].clone();
    assert_ne!(fitted.size, before_conversion.size);
    assert!(matches!(
        &fitted.kind,
        ObjectKind::Text { text, frame, .. }
            if text == "Line one\nLine two" && frame.sizing_mode == TextBoxSizingMode::Fit
    ));

    let frozen = editor
        .update_text_box_sizing(2, &fixed_id, TextBoxSizingModeInput::Fixed)
        .expect("application freezes the same authoritative bounds as a Fixed box");
    let fixed = frozen.project.document.pages[0].objects[1].clone();
    assert_eq!(fixed.size, fitted.size);
    assert!(matches!(
        &fixed.kind,
        ObjectKind::Text { frame, .. } if frame.sizing_mode == TextBoxSizingMode::Fixed
    ));

    let undone = editor.undo(3).expect("undo sizing conversion");
    assert_eq!(undone.project.document.pages[0].objects[1], fitted);
    let redone = editor.redo(4).expect("redo sizing conversion");
    assert_eq!(redone.project.document.pages[0].objects[1], fixed);
    assert_eq!(
        redone.project.document.pages[0].objects[0].id.as_str(),
        fit_id
    );

    editor.save_new(&package).expect("save sizing modes");
    let mut reopened = EditorModule::new();
    let reopened = reopened.open(&package).expect("reopen sizing modes");
    assert_eq!(reopened.project.document, redone.project.document);
}

#[test]
fn public_transactions_reject_caller_supplied_sizing_geometry_atomically() {
    let mut editor = create_editor();
    let page_id = editor.snapshot().expect("snapshot").project.document.pages[0]
        .id
        .as_str()
        .to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add fixed text",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Body".to_owned(),
                text: "Body".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 10_000,
                y_um: 10_000,
                width_um: 80_000,
                height_um: 50_000,
            }],
        ))
        .expect("add fixed text");
    let before = editor.snapshot().expect("snapshot before rejection");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();

    for sizing_mode in [TextBoxSizingModeInput::Fit, TextBoxSizingModeInput::Fixed] {
        let error = editor
            .apply(transaction(
                1,
                "Reject fabricated sizing geometry",
                vec![
                    EditorCommandInput::UpdateText {
                        object_id: object_id.clone(),
                        text: "This edit must not leak through".to_owned(),
                    },
                    EditorCommandInput::UpdateTextBoxSizing {
                        object_id: object_id.clone(),
                        sizing_mode,
                        width_um: 999_999,
                        height_um: 888_888,
                    },
                ],
            ))
            .expect_err("public geometry-bearing sizing commands are rejected");
        assert_eq!(error.code, EditorErrorCode::AuthoritativeLayoutRequired);
        assert_eq!(editor.snapshot().expect("snapshot after rejection"), before);
    }
}

#[test]
fn omitted_json_sizing_mode_remains_backward_compatible_fixed() {
    let input: EditorCommandInput = serde_json::from_value(serde_json::json!({
        "type": "addText",
        "pageId": null,
        "name": "Legacy adapter text",
        "text": "Legacy",
        "xUm": 0,
        "yUm": 0,
        "widthUm": 50_000,
        "heightUm": 20_000
    }))
    .expect("older adapter payload remains readable");
    assert!(matches!(
        input,
        EditorCommandInput::AddText {
            sizing_mode: TextBoxSizingModeInput::Fixed,
            ..
        }
    ));
}
