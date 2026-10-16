#[path = "fixtures/common_rounding.rs"]
mod fixture;
use document_core::shape_geometry::{
    ShapePathCommand, normalized_corner_radii, rounded_cell_text_bounds, rounded_interior_insets,
    rounded_text_margins,
};
use layout_core::{ResolvedPublication, ResolvedSceneObjectKind};

#[test]
fn rounded_preview_and_complete_output_share_safe_interiors_without_repartitioning_cells() {
    let (project, fonts, _) = fixture::fixture();
    let preview = ResolvedPublication::resolve_with_font_resources(&project, &fonts).unwrap();
    let output = ResolvedPublication::resolve_output_with_font_resources(&project, &fonts).unwrap();
    assert_eq!(preview.pages(), output.pages());
    let objects = &output.pages()[0].objects;
    for (index, object) in objects.iter().enumerate().take(2) {
        let ResolvedSceneObjectKind::Text { layout, .. } = &object.kind else {
            panic!("Text Box")
        };
        let document_core::model::ObjectKind::Text { frame, .. } =
            &project.document.pages[0].objects[index].kind
        else {
            panic!("Text Box")
        };
        let margins =
            rounded_text_margins(object.size, object.corner_rounding, frame.margins).unwrap();
        assert_eq!(layout.content_bounds.x, margins.left);
        assert_eq!(layout.content_bounds.y, margins.top);
        assert!(!layout.overset);
        if index == 1 {
            assert_ink_inside(layout);
        }
        assert_eq!(
            layout
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            if index == 0 {
                vec!["Full text"]
            } else {
                vec!["jf", "A"]
            }
        );
    }
    let table_object = &objects[2];
    let ResolvedSceneObjectKind::Table { table } = &table_object.kind else {
        panic!("Table")
    };
    assert_eq!(table.column_boundaries_um, vec![0, 12_700, 25_400]);
    assert_eq!(table.row_boundaries_um, vec![0, 6_350, 12_700]);
    let inset = rounded_interior_insets(table_object.size, table_object.corner_rounding).unwrap();
    for cell in &table.populated_cells {
        let col = usize::from(cell.column);
        let row = usize::from(cell.row);
        let [left, top, right, bottom] = rounded_cell_text_bounds(
            table_object.size,
            inset,
            [
                table.column_boundaries_um[col],
                table.row_boundaries_um[row],
                table.column_boundaries_um[col + 1],
                table.row_boundaries_um[row + 1],
            ],
        )
        .unwrap();
        assert_eq!(cell.layout.content_bounds.x.value(), left);
        assert_eq!(cell.layout.content_bounds.y.value(), top);
        assert_eq!(cell.layout.content_bounds.width.value(), right - left);
        assert_eq!(cell.layout.content_bounds.height.value(), bottom - top);
        assert!(!cell.clipped);
    }
    assert_eq!(
        table
            .border_segments
            .iter()
            .flat_map(|edge| &edge.outline)
            .filter(|command| matches!(command, ShapePathCommand::CubicTo(..)))
            .count(),
        4
    );
    assert_eq!(
        normalized_corner_radii(table_object.size, table_object.corner_rounding).unwrap(),
        [6_350; 4]
    );
}

fn assert_ink_inside(layout: &layout_core::ResolvedTextBoxPreview) {
    use layout_core::ResolvedOutlineCommand;
    let bounds = layout.content_bounds;
    let convert = |value: i64| value * 72_000 / 25_400;
    let left = convert(bounds.x.value());
    let top = convert(bounds.y.value());
    let right = convert(bounds.x.value() + bounds.width.value());
    let bottom = convert(bounds.y.value() + bounds.height.value());
    for glyph in layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .flat_map(|run| &run.glyphs)
    {
        let check = |x: layout_core::text::Millipoints, y: layout_core::text::Millipoints| {
            let x = glyph.x.value() + x.value();
            let y = glyph.y.value() - y.value();
            assert!(
                x >= left - 1 && x <= right + 1 && y >= top - 1 && y <= bottom + 1,
                "painted ink ({x},{y}) is outside safe interior ({left},{top},{right},{bottom})"
            );
        };
        if let Some(outline) = &glyph.outline {
            for command in &outline.commands {
                match *command {
                    ResolvedOutlineCommand::MoveTo { x, y }
                    | ResolvedOutlineCommand::LineTo { x, y } => check(x, y),
                    ResolvedOutlineCommand::QuadTo {
                        control_x,
                        control_y,
                        x,
                        y,
                    } => {
                        check(control_x, control_y);
                        check(x, y);
                    }
                    ResolvedOutlineCommand::CurveTo {
                        control_1_x,
                        control_1_y,
                        control_2_x,
                        control_2_y,
                        x,
                        y,
                    } => {
                        check(control_1_x, control_1_y);
                        check(control_2_x, control_2_y);
                        check(x, y);
                    }
                    ResolvedOutlineCommand::Close => {}
                }
            }
        }
    }
}

#[test]
fn independent_requested_radii_change_scene_digest_and_preserve_table_source() {
    let (mut project, fonts, _) = fixture::fixture();
    let source = project.document.pages[0].objects[2].kind.clone();
    let first = ResolvedPublication::resolve_output_with_font_resources(&project, &fonts).unwrap();
    let corners = &mut project.document.pages[0].objects[2].corner_rounding;
    corners.linked = false;
    corners.top_left_um = 3_000;
    let second = ResolvedPublication::resolve_output_with_font_resources(&project, &fonts).unwrap();
    assert_ne!(first.semantic_digest(), second.semantic_digest());
    assert_eq!(project.document.pages[0].objects[2].kind, source);
    let ResolvedSceneObjectKind::Table { table } = &second.pages()[0].objects[2].kind else {
        panic!("Table")
    };
    assert_eq!(
        table
            .populated_cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"]
    );
}

#[test]
fn every_rounded_table_corner_uses_one_clockwise_outgoing_edge_appearance() {
    use document_core::model::{Color, TableCellBorders, TableCellFormatting, TableGridBorder};
    let (mut project, fonts, _) = fixture::fixture();
    let colors = [
        Color {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        },
        Color {
            red: 0,
            green: 255,
            blue: 0,
            alpha: 255,
        },
        Color {
            red: 0,
            green: 0,
            blue: 255,
            alpha: 255,
        },
        Color {
            red: 100,
            green: 0,
            blue: 100,
            alpha: 255,
        },
    ];
    let cells = [(0, 0), (0, 1), (1, 1), (1, 0)];
    let document_core::model::ObjectKind::Table { table } =
        &mut project.document.pages[0].objects[2].kind
    else {
        panic!("Table")
    };
    for (index, ((row, column), color)) in cells.into_iter().zip(colors).enumerate() {
        let border = Some(TableGridBorder {
            color,
            width_millipoints: 2_000,
        });
        let borders = match index {
            0 => TableCellBorders {
                top: border,
                ..TableCellBorders::default()
            },
            1 => TableCellBorders {
                right: border,
                ..TableCellBorders::default()
            },
            2 => TableCellBorders {
                bottom: border,
                ..TableCellBorders::default()
            },
            _ => TableCellBorders {
                left: border,
                ..TableCellBorders::default()
            },
        };
        table.cell_formatting.push(TableCellFormatting {
            row,
            column,
            borders,
            ..TableCellFormatting::default()
        });
    }
    table
        .cell_formatting
        .sort_by_key(|format| (format.row, format.column));
    let object = &project.document.pages[0].objects[2];
    let path =
        document_core::shape_geometry::rectangle_outline(object.size, object.corner_rounding)
            .unwrap();
    let scene = ResolvedPublication::resolve_output_with_font_resources(&project, &fonts).unwrap();
    let ResolvedSceneObjectKind::Table { table } = &scene.pages()[0].objects[2].kind else {
        panic!("Table")
    };
    for (corner, color) in [path[8], path[2], path[4], path[6]].into_iter().zip(colors) {
        let owners = table
            .border_segments
            .iter()
            .filter(|edge| edge.outline.contains(&corner))
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].color, color);
        assert_eq!(owners[0].width_millipoints, 2_000);
    }
}
