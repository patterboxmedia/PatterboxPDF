#[path = "fixtures/table_authoring.rs"]
mod fixture;
use fixture::instruction_page;
use layout_core::{
    PreviewBudgetState, ResolvedPagePreview, ResolvedPublication, ResolvedSceneObjectKind,
    TextBoxPreviewState,
};

#[test]
fn filled_thirty_by_thirty_plus_heading_and_list_are_complete_in_preview_and_output() {
    let (project, font) = instruction_page();
    let output = ResolvedPublication::resolve_output_with_font_resources(
        &project,
        std::slice::from_ref(&font),
    )
    .expect("filled 30 by 30 instruction page must compose completely");
    let preview = ResolvedPagePreview::resolve_with_font_resources(
        &project,
        &project.document.pages[0].id,
        false,
        &[font],
    )
    .unwrap();
    assert_eq!(output.budget_state(), PreviewBudgetState::Complete);
    assert_eq!(preview.budget_state(), PreviewBudgetState::Complete);
    assert_eq!(&output.pages()[0], preview.page());
    assert_eq!(output.pages()[0].objects.len(), 3);
    let ResolvedSceneObjectKind::Table { table } = &output.pages()[0].objects[2].kind else {
        panic!("Table")
    };
    assert_eq!(table.populated_cells.len(), 900);
    for (index, cell) in table.populated_cells.iter().enumerate() {
        assert_eq!(
            (cell.row, cell.column),
            (
                u16::try_from(index / 30).unwrap(),
                u16::try_from(index % 30).unwrap()
            )
        );
        assert_eq!(
            cell.text,
            char::from(b'A' + u8::try_from(index % 26).unwrap()).to_string()
        );
        assert_eq!(cell.layout.preview_state, TextBoxPreviewState::Complete);
        assert!(!cell.clipped);
        assert_eq!(cell.layout.lines.len(), 1);
    }
}
