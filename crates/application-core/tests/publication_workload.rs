//! Opt-in Windows workload characterization; no production scaling changes.
#![cfg(windows)]

use application_core::{
    ApplyTransactionInput, CreateDocumentInput, EditorCommandInput, EditorModule,
};
use document_core::model::{
    FontFaceBinding, ImageContentKind, ImageCrop, ObjectEffects, ObjectKind, PhysicalSize,
    PublicationObject, TableData, TextFrame, TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::{AssetStore, ObjectId, ProjectSnapshot};
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, PreviewBudgetState, ResolvedPagePreview,
    ResolvedPublication, ResolvedSceneObjectKind, TextBoxPreviewState, preview_font_face_bindings,
};
use serde_json::json;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        // Keep the ordinary Windows path spelling accepted by the publication
        // boundary; canonicalize() adds a verbatim device prefix on Windows.
        let root = std::env::temp_dir();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!(
            "publisher-publication-workload-{}-{nonce}",
            std::process::id()
        ));
        assert!(path.is_absolute() && path.parent() == Some(root.as_path()));
        // Exclusive ownership: never adopt or remove a pre-existing destination.
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn object(page: usize, label: &str, kind: ObjectKind, y: i64, height: i64) -> PublicationObject {
    PublicationObject {
        id: ObjectId::parse(format!("workload-p{page:02}-{label}")).unwrap(),
        group_id: None,
        locked: false,
        name: format!("Page {page}: {label}"),
        size: PhysicalSize::new(180_000, height).unwrap(),
        transform: Transform::identity_at(15_000, y),
        opacity_millionths: 1_000_000,
        effects: ObjectEffects::default(),
        wrap_settings: document_core::model::WrapSettings::default(),
        corner_rounding: document_core::model::CornerRounding::default(),
        text_field: None,
        kind,
    }
}

fn picture(variant: u8) -> Vec<u8> {
    // Original deterministic geometric artwork: actual decodable RGB PNGs.
    let pixels = image::RgbImage::from_fn(32, 24, |x, y| {
        let stripe = if (x / 4 + y / 4) % 2 == 0 { 40 } else { 200 };
        image::Rgb([stripe, variant, 255 - stripe])
    });
    let mut bytes = Cursor::new(Vec::new());
    pixels
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn seed(path: &Path) -> (ProjectSnapshot, AssetStore) {
    let mut editor = EditorModule::new();
    let mut project = editor
        .create(CreateDocumentInput {
            document_id: "publication-workload-v1".into(),
            title: "Original workshop notes: 24-page workload v1".into(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 24,
        })
        .unwrap()
        .project;
    let mut assets = AssetStore::default();
    let font = assets.insert(
        "font/ttf",
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
    );
    let images = [64, 160].map(|variant| {
        let bytes = picture(variant);
        let inspection = document_core::inspect_raster(&bytes).unwrap();
        assets.insert_raster(bytes, inspection)
    });
    let style = TextStyle {
        font_face: Some(FontFaceBinding {
            source: font,
            face_index: 0,
        }),
        font_family: "DejaVu Sans".into(),
        font_size_millipoints: 10_000,
        ..Default::default()
    };
    for (index, page) in project.document.pages.iter_mut().enumerate() {
        let number = index + 1;
        if [12, 24].contains(&number) {
            continue;
        }
        for section in 0..3 {
            let text = if section == 0 {
                format!("WORKSHOP {number:02}: Observe, arrange, and explain")
            } else {
                (0..8).map(|paragraph| format!(
                    "Page {number:02}, section {section}, paragraph {paragraph}: Place the paper on a clear table. Compare the long edges, fold carefully, and describe the result in your own words. Leave room for the next attempt. {}",
                    if paragraph % 2 == 0 { "Record what changed." } else { "Check the position before continuing." }
                )).collect::<Vec<_>>().join("\n\n")
            };
            page.objects.push(object(
                number,
                &format!("text-{section}"),
                ObjectKind::Text {
                    text,
                    style: style.clone(),
                    frame: TextFrame::default(),
                    story: TextStoryFormatting::default(),
                },
                [12_000, 28_000, 120_000][section],
                [12_000, 86_000, 86_000][section],
            ));
        }
        let mut table = TableData::empty(4, 4).unwrap();
        table.style = style.clone();
        table.cells = (0..16)
            .map(|cell| format!("P{number:02} check {:02}\nReady", cell + 1))
            .collect();
        page.objects.push(object(
            number,
            "table",
            ObjectKind::Table { table },
            212_000,
            48_000,
        ));
        let mut image = object(
            number,
            "picture",
            ObjectKind::Image {
                asset: images[index % 2].clone(),
                wrap_mode: WrapMode::default(),
                content_kind: ImageContentKind::default(),
                crop: ImageCrop::default(),
                flip: document_core::model::ImageFlip::default(),
                lock_aspect_ratio: true,
            },
            12_000,
            9_000,
        );
        image.size = PhysicalSize::new(12_000, 9_000).unwrap();
        image.transform = Transform::identity_at(195_000, 12_000);
        page.objects.push(image);
    }
    project.document.validate().unwrap();
    document_core::save_new_package(path, &project, &assets).unwrap();
    (project, assets)
}

/// Retain the same rights-safe workload for native interaction acceptance.
/// Explicit opt-in creates a new bundle; it never adopts or replaces a directory.
#[test]
#[ignore = "requires PUBLISHER_WORKLOAD_BUNDLE naming a new absolute directory"]
fn write_native_acceptance_fixture() {
    let destination = PathBuf::from(
        std::env::var_os("PUBLISHER_WORKLOAD_BUNDLE")
            .expect("set PUBLISHER_WORKLOAD_BUNDLE to a new absolute directory"),
    );
    assert!(
        destination.is_absolute(),
        "fixture destination must be absolute"
    );
    std::fs::create_dir(&destination).expect("fixture bundle must not already exist");
    let path = destination.join("workshop-24-pages.publisher-v2");
    let (project, assets) = seed(&path);
    std::fs::write(
        destination.join("LICENSE-DejaVu"),
        include_bytes!("../../layout-core/tests/fixtures/LICENSE-DejaVu"),
    )
    .unwrap();
    let reopened = document_core::open_package(&path).unwrap();
    assert_eq!(reopened.snapshot, project);
    assert_eq!(reopened.assets, assets);
    println!("Native acceptance fixture: {}", path.display());
}

fn text_counts(project: &ProjectSnapshot) -> (usize, usize) {
    let mut items = 0;
    let mut bytes = 0;
    for object in project.document.pages.iter().flat_map(|page| &page.objects) {
        match &object.kind {
            ObjectKind::Text { text, .. } => {
                items += 1;
                bytes += text.len();
            }
            ObjectKind::Table { table } => {
                for cell in table.cells.iter().filter(|cell| !cell.is_empty()) {
                    items += 1;
                    bytes += cell.len();
                }
            }
            _ => {}
        }
    }
    (items, bytes)
}

fn compose(project: &ProjectSnapshot, resources: &[ExactFontResource], label: &str) {
    let started = Instant::now();
    let scene = ResolvedPublication::resolve_with_font_resources(project, resources).unwrap();
    let micros = started.elapsed().as_micros();
    let mut complete = 0;
    let mut limited = 0;
    let mut overset = 0;
    for object in scene.pages().iter().flat_map(|page| &page.objects) {
        let layouts = match &object.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => vec![layout],
            ResolvedSceneObjectKind::Table { table } => table
                .populated_cells
                .iter()
                .map(|cell| &cell.layout)
                .collect(),
            _ => Vec::new(),
        };
        for layout in layouts {
            match layout.preview_state {
                TextBoxPreviewState::Complete => {
                    complete += 1;
                    assert_eq!(
                        layout.font_binding.substitution_status,
                        FontSubstitutionStatus::Exact
                    );
                }
                TextBoxPreviewState::BudgetExceeded => {
                    limited += 1;
                }
                TextBoxPreviewState::MissingFont => {
                    panic!("checked-in exact font must be available")
                }
            }
            overset += usize::from(layout.overset);
        }
    }
    let (items, bytes) = text_counts(project);
    assert_eq!(
        complete + limited,
        items,
        "omitted work must remain explicit"
    );
    assert_eq!(scene.pages().len(), project.document.pages.len());
    for (source, resolved) in project.document.pages.iter().zip(scene.pages()) {
        assert_eq!(source.objects.len(), resolved.objects.len());
    }
    if label == "single_page_control" {
        assert_eq!(scene.budget_state(), PreviewBudgetState::Complete);
        assert_eq!(limited, 0);
    } else {
        assert_eq!(scene.budget_state(), PreviewBudgetState::Truncated);
        assert!(complete > 0 && limited > 0);
        if label == "item_ceiling_probe" {
            assert_eq!(complete, 256);
            assert_eq!(limited, 1);
        }
    }
    println!(
        "WORKLOAD {}",
        json!({"phase":label,"compose_us":micros,"pages":scene.pages().len(),"text_items":items,"text_bytes":bytes,"complete_items":complete,"limited_items":limited,"overset_items":overset,"preview_complete":limited == 0})
    );
}

fn compose_output(project: &ProjectSnapshot, resources: &[ExactFontResource], label: &str) {
    let started = Instant::now();
    let scene =
        ResolvedPublication::resolve_output_with_font_resources(project, resources).unwrap();
    let micros = started.elapsed().as_micros();
    assert_eq!(scene.budget_state(), PreviewBudgetState::Complete);
    assert_eq!(scene.revision(), project.revision);
    assert_eq!(
        scene.source_project_digest(),
        project.semantic_digest().unwrap()
    );
    assert_eq!(scene.pages().len(), project.document.pages.len());
    let mut complete = 0;
    for (source, page) in project.document.pages.iter().zip(scene.pages()) {
        assert_eq!(page.page_id, source.id);
        assert_eq!(page.objects.len(), source.objects.len());
        for object in &page.objects {
            let layouts = match &object.kind {
                ResolvedSceneObjectKind::Text { layout, .. } => vec![layout],
                ResolvedSceneObjectKind::Table { table } => table
                    .populated_cells
                    .iter()
                    .map(|cell| &cell.layout)
                    .collect(),
                _ => Vec::new(),
            };
            for layout in layouts {
                assert_eq!(layout.preview_state, TextBoxPreviewState::Complete);
                assert_eq!(
                    layout.font_binding.substitution_status,
                    FontSubstitutionStatus::Exact
                );
                complete += 1;
            }
        }
    }
    assert_eq!(complete, text_counts(project).0);
    println!(
        "OUTPUT {}",
        json!({"phase":label,"compose_us":micros,"pages":scene.pages().len(),"complete_items":complete,"output_complete":true})
    );
}

fn files_under(path: &Path) -> (u64, u64) {
    if !path.exists() {
        return (0, 0);
    }
    let mut total = (0, 0);
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.metadata().unwrap();
        assert!(!metadata.file_type().is_symlink());
        let (files, bytes) = if metadata.is_dir() {
            files_under(&entry.path())
        } else {
            (1, metadata.len())
        };
        total.0 += files;
        total.1 += bytes;
    }
    total
}

fn compose_pages(
    project: &ProjectSnapshot,
    resources: &[ExactFontResource],
    indices: &[usize],
    label: &str,
) {
    let mut complete_items = 0;
    let mut elapsed_us = 0;
    for &index in indices {
        let started = Instant::now();
        let preview = ResolvedPagePreview::resolve_with_font_resources(
            project,
            &project.document.pages[index].id,
            true,
            resources,
        )
        .unwrap();
        let micros = started.elapsed().as_micros();
        elapsed_us += micros;
        assert_eq!(preview.revision(), project.revision);
        assert_eq!(
            preview.source_project_digest(),
            project.semantic_digest().unwrap()
        );
        assert_eq!(preview.budget_state(), PreviewBudgetState::Complete);
        let mut page_items = 0;
        for object in &preview.page().objects {
            let layouts = match &object.kind {
                ResolvedSceneObjectKind::Text { layout, .. } => vec![layout],
                ResolvedSceneObjectKind::Table { table } => table
                    .populated_cells
                    .iter()
                    .map(|cell| &cell.layout)
                    .collect(),
                _ => Vec::new(),
            };
            for layout in layouts {
                assert_eq!(layout.preview_state, TextBoxPreviewState::Complete);
                assert_eq!(
                    layout.font_binding.substitution_status,
                    FontSubstitutionStatus::Exact
                );
                page_items += 1;
            }
        }
        assert_eq!(page_items, if [11, 23].contains(&index) { 0 } else { 19 });
        complete_items += page_items;
        println!(
            "PAGE_WORKLOAD {}",
            json!({"phase":label,"page":index+1,"compose_us":micros,"complete_items":page_items})
        );
    }
    println!(
        "PAGE_WORKLOAD {}",
        json!({"phase":label,"pages":indices.len(),"sum_compose_us":elapsed_us,"complete_items":complete_items})
    );
}

fn process_private_bytes() -> u64 {
    use std::os::windows::process::CommandExt;
    // Read only this test process. The helper is windowless and its allocations
    // belong to a separate process, outside the measured private-byte count.
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "[System.Diagnostics.Process]::GetProcessById({}).PrivateMemorySize64",
                std::process::id()
            ),
        ])
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(output.status.success(), "memory helper failed");
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "Windows history memory baseline; run alone in release mode"]
#[allow(
    clippy::assertions_on_constants,
    reason = "ignored benchmark must compile in debug but refuse debug measurements at runtime"
)]
fn representative_history_memory_baseline() {
    assert!(!cfg!(debug_assertions), "run with --release");
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.join("LICENSE-DejaVu"),
        include_bytes!("../../layout-core/tests/fixtures/LICENSE-DejaVu"),
    )
    .unwrap();
    let path = fixture.0.join("history.publisher-v2");
    let (expected, _) = seed(&path);
    let mut editor = EditorModule::new();
    editor.open(&path).unwrap();
    let content_hash = |document: &document_core::Document| {
        document_core::AssetHash::for_bytes(&serde_json::to_vec(document).unwrap())
    };
    let mut expected_states = vec![content_hash(&expected.document)];
    let baseline = process_private_bytes();
    println!("HISTORY {}", json!({"edits":0,"private_bytes":baseline}));
    for edit in 0..1_000 {
        let command = match edit % 3 {
            0 => EditorCommandInput::UpdateText {
                object_id: "workload-p01-text-1".into(),
                text: format!("Original workshop instruction, revision {edit:04}."),
            },
            1 => EditorCommandInput::UpdateTableCell {
                object_id: "workload-p23-table".into(),
                row: 0,
                column: 0,
                text: format!("Check {edit:04}"),
            },
            _ => EditorCommandInput::SetOpacity {
                object_id: "workload-p23-picture".into(),
                opacity_millionths: 500_000 + (edit % 400) * 1_000,
            },
        };
        editor
            .apply(ApplyTransactionInput {
                expected_revision: editor.snapshot().unwrap().project.revision.value(),
                label: "History workload edit".into(),
                commands: vec![command],
            })
            .unwrap();
        expected_states.push(content_hash(&editor.snapshot().unwrap().project.document));
        if (edit + 1) % 100 == 0 {
            let bytes = process_private_bytes();
            let history = editor.history_retention_status().unwrap();
            println!(
                "HISTORY {}",
                json!({"edits":edit+1,"private_bytes":bytes,
                "growth_bytes":bytes.saturating_sub(baseline),
                "retained_undo":history.undo_transactions,"snapshot_charge":history.snapshot_bytes,
                "history_asset_charge":history.history_asset_bytes,"older_history_removed":history.older_history_removed})
            );
        }
    }
    let edited = editor.snapshot().unwrap().project.document;
    let history = editor.history_retention_status().unwrap();
    assert!(history.older_history_removed);
    assert!(history.undo_transactions <= history.limits.max_transactions);
    assert!(history.snapshot_bytes <= history.limits.max_snapshot_bytes);
    assert!(history.history_asset_bytes <= history.limits.max_history_asset_bytes);
    let mut undone = 0;
    while editor.snapshot().unwrap().can_undo {
        editor
            .undo(editor.snapshot().unwrap().project.revision.value())
            .unwrap();
        undone += 1;
        assert_eq!(
            content_hash(&editor.snapshot().unwrap().project.document),
            expected_states[1_000 - undone]
        );
    }
    assert!(undone > 0 && undone <= history.limits.max_transactions);
    println!(
        "HISTORY {}",
        json!({"phase":"retained_history_undone","private_bytes":process_private_bytes(),"undo_steps":undone,"undo_exact":true})
    );
    let mut redone = 0;
    while editor.snapshot().unwrap().can_redo {
        editor
            .redo(editor.snapshot().unwrap().project.revision.value())
            .unwrap();
        redone += 1;
        assert_eq!(
            content_hash(&editor.snapshot().unwrap().project.document),
            expected_states[1_000 - undone + redone]
        );
    }
    assert_eq!(redone, undone);
    assert_eq!(editor.snapshot().unwrap().project.document, edited);
    println!(
        "HISTORY {}",
        json!({"phase":"retained_history_redone","private_bytes":process_private_bytes(),"redo_steps":redone,"redo_exact":true})
    );
}

#[test]
#[ignore = "24-page Windows workload baseline; run explicitly in release mode with --ignored --nocapture"]
#[allow(
    clippy::too_many_lines,
    reason = "one bounded workload records composition and persistence phases"
)]
#[allow(
    clippy::assertions_on_constants,
    reason = "ignored benchmark must compile in debug but refuse debug measurements at runtime"
)]
fn representative_publication_baseline() {
    assert!(
        !cfg!(debug_assertions),
        "run this opt-in measurement with --release"
    );
    let fixture = Fixture::new();
    let seed_path = fixture.0.join("seed.publisher-v2");
    let saved_path = fixture.0.join("saved.publisher-v2");
    let recovery = fixture.0.join("recovery");
    // Include the font's required notice with every generated fixture bundle.
    std::fs::write(
        fixture.0.join("LICENSE-DejaVu"),
        include_bytes!("../../layout-core/tests/fixtures/LICENSE-DejaVu"),
    )
    .unwrap();
    let (expected, assets) = seed(&seed_path);
    let mut editor = EditorModule::new();
    let initial = editor.open(&seed_path).unwrap();
    assert_eq!(initial.project, expected);
    let embedded = editor
        .referenced_font_faces(&preview_font_face_bindings(&expected), 1, 1_000_000)
        .unwrap();
    assert!(!embedded.budget_exceeded);
    assert_eq!(embedded.faces.len(), 1);
    let resources = embedded
        .faces
        .into_iter()
        .map(|face| ExactFontResource {
            source: face.source,
            face_index: face.face_index,
            bytes: face.bytes,
        })
        .collect::<Vec<_>>();
    let (items, bytes) = text_counts(&expected);
    assert_eq!(items, 418);
    assert_eq!(bytes, 82_324);
    assert_eq!(assets.len(), 3);
    let objects: usize = expected
        .document
        .pages
        .iter()
        .map(|page| page.objects.len())
        .sum();
    assert_eq!(objects, 110);
    assert_eq!(
        expected
            .document
            .pages
            .iter()
            .filter(|page| page.objects.is_empty())
            .count(),
        2
    );
    let asset_bytes: usize = document_core::assets::referenced_asset_hashes(&expected.document)
        .iter()
        .map(|hash| assets.get(hash).unwrap().bytes.len())
        .sum();
    println!(
        "WORKLOAD {}",
        json!({"phase":"fixture","schema":1,"pages":24,"blank_pages":[12,24],"objects":objects,"text_boxes":66,"tables":22,"populated_cells":352,"pictures":22,"assets":assets.len(),"asset_bytes":asset_bytes,"text_items":items,"text_bytes":bytes})
    );
    for run in 0..3 {
        compose(&expected, &resources, &format!("whole_publication_{run}"));
        compose_output(&expected, &resources, &format!("whole_publication_{run}"));
    }
    compose_pages(
        &expected,
        &resources,
        &(0..24).collect::<Vec<_>>(),
        "scoped_all_pages",
    );
    let mut control = expected.clone();
    control.document.pages.truncate(1);
    compose(&control, &resources, "single_page_control");
    let mut item_probe = expected.clone();
    item_probe.document.pages.truncate(1);
    let mut short_text = expected.document.pages[0].objects[1].clone();
    let ObjectKind::Text { text, .. } = &mut short_text.kind else {
        panic!("body Text Box")
    };
    text.truncate(32);
    // A Table consumes one aggregate preview slot, not one per populated cell.
    // Use actual Text Boxes to independently exceed the 256-object budget.
    item_probe.document.pages[0].objects = (0..257)
        .map(|index| {
            let mut object = short_text.clone();
            object.id = ObjectId::parse(format!("workload-item-probe-{index}")).unwrap();
            object
        })
        .collect();
    assert_eq!(text_counts(&item_probe).0, 257);
    assert!(text_counts(&item_probe).1 < 64 * 1024);
    compose(&item_probe, &resources, "item_ceiling_probe");
    let mut byte_probe = expected.clone();
    for page in &mut byte_probe.document.pages {
        page.objects
            .retain(|object| !matches!(object.kind, ObjectKind::Table { .. }));
    }
    assert!(text_counts(&byte_probe).0 < 256 && text_counts(&byte_probe).1 > 64 * 1024);
    compose(&byte_probe, &resources, "byte_ceiling_probe");
    let mut alternate_recoveries = Vec::new();
    for (label, page_index) in [("early", 0), ("late", 22)] {
        let object = &expected.document.pages[page_index].objects[1];
        let ObjectKind::Text { text, .. } = &object.kind else {
            panic!("body Text Box")
        };
        let started = Instant::now();
        let edited = editor
            .apply(ApplyTransactionInput {
                expected_revision: editor.snapshot().unwrap().project.revision.value(),
                label: format!("{label} workload edit"),
                commands: vec![EditorCommandInput::UpdateText {
                    object_id: object.id.as_str().into(),
                    text: format!("{text}\n\nCheck the final fold."),
                }],
            })
            .unwrap();
        let apply_us = started.elapsed().as_micros();
        assert_ne!(edited.project.document, expected.document);
        compose(&edited.project, &resources, &format!("{label}_edit"));
        compose_output(&edited.project, &resources, &format!("{label}_edit"));
        compose_pages(
            &edited.project,
            &resources,
            &[page_index],
            &format!("{label}_edit"),
        );
        let candidate = editor.autosave(&recovery).unwrap();
        let package_bytes = std::fs::read(&candidate.package_path).unwrap();
        alternate_recoveries.push((candidate.package_path, package_bytes));
        let started = Instant::now();
        let undone = editor.undo(edited.project.revision.value()).unwrap();
        let undo_us = started.elapsed().as_micros();
        assert_eq!(undone.project.document, expected.document);
        compose_output(&undone.project, &resources, &format!("{label}_undo"));
        compose_pages(
            &undone.project,
            &resources,
            &[page_index],
            &format!("{label}_undo"),
        );
        let (files, bytes) = files_under(&recovery);
        println!(
            "WORKLOAD {}",
            json!({"phase":label,"apply_us":apply_us,"undo_us":undo_us,"recovery_files":files,"recovery_bytes":bytes})
        );
    }
    let final_project = editor.snapshot().unwrap().project;
    // Undo relinquishes the edited branch's recovery tokens. Preserve those
    // alternatives, then save one new candidate on the current document lineage.
    let alternate_storage = files_under(&recovery);
    let current_candidate = editor.autosave(&recovery).unwrap();
    let current_storage = files_under(current_candidate.package_path.parent().unwrap());
    let before_retirement = files_under(&recovery);
    assert_eq!(before_retirement.0, alternate_storage.0 + current_storage.0);
    assert_eq!(before_retirement.1, alternate_storage.1 + current_storage.1);
    let started = Instant::now();
    editor.save_new(&saved_path).unwrap();
    let save_us = started.elapsed().as_micros();
    assert!(!editor.snapshot().unwrap().is_dirty);
    let report = editor.retire_saved_recoveries(&recovery);
    assert!(report.warning.is_none(), "{report:?}");
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(
        report.outcomes[0].package_path,
        current_candidate.package_path
    );
    assert!(
        report
            .outcomes
            .iter()
            .all(|outcome| outcome.retired && outcome.warning.is_none())
    );
    assert!(!current_candidate.package_path.exists());
    for (path, bytes) in &alternate_recoveries {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    let final_storage = files_under(&recovery);
    assert_eq!(final_storage, alternate_storage);
    let started = Instant::now();
    let mut reopened = EditorModule::new();
    let snapshot = reopened.open(&saved_path).unwrap();
    let reopen_us = started.elapsed().as_micros();
    assert_eq!(snapshot.project, final_project);
    assert!(!snapshot.is_dirty);
    let package = document_core::open_package(&saved_path).unwrap();
    assert_eq!(package.assets, assets);
    compose(&snapshot.project, &resources, "reopened");
    compose_output(&snapshot.project, &resources, "reopened");
    compose_pages(&snapshot.project, &resources, &[0, 22], "reopened");
    println!(
        "WORKLOAD {}",
        json!({"phase":"persistence","save_us":save_us,"reopen_us":reopen_us,"package_bytes":std::fs::metadata(&saved_path).unwrap().len(),"recovery_peak_files":before_retirement.0,"recovery_peak_bytes":before_retirement.1,"recovery_final_files":final_storage.0,"recovery_final_bytes":final_storage.1,"alternate_recoveries_preserved":alternate_recoveries.len(),"current_recoveries_retired":report.outcomes.len(),"snapshot_roundtrip_exact":true,"assets_roundtrip_exact":true})
    );
}
