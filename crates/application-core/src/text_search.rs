//! Searches authored text and commits replacements through canonical transactions.

use super::{
    EditorErrorCode, EditorModule, EditorResult, EditorSnapshot, invalid_input, no_document,
    parse_object_id,
};
use document_core::commands::{Command, MAX_TEXT_EDIT_COUNT, TextEdit, Transaction};
use document_core::model::{
    Document, MAX_TEXT_BYTES, ObjectKind, PublicationObject, Revision, TextSourceRange,
};
use serde::{Deserialize, Serialize};
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

const MAX_QUERY_BYTES: usize = 1_024;
const MAX_RESULT_PAGE: usize = 500;
// Whole-publication replacement can exceed the ordinary command bridge limit.
// Saveability admission still bounds the resulting document and retained history.
const MAX_REPLACEMENT_SOURCES: usize = 50_000;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TextSearchScope {
    #[default]
    Publication,
    Page {
        page_id: String,
    },
    Master {
        master_id: String,
    },
    Object {
        object_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TextSourceLocation {
    TextBox {
        object_id: String,
        page_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        master_id: Option<String>,
    },
    TableCell {
        object_id: String,
        page_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        master_id: Option<String>,
        row: u16,
        column: u16,
    },
}

impl TextSourceLocation {
    #[must_use]
    pub fn object_id(&self) -> &str {
        match self {
            Self::TextBox { object_id, .. } | Self::TableCell { object_id, .. } => object_id,
        }
    }

    #[must_use]
    pub fn page_id(&self) -> Option<&str> {
        match self {
            Self::TextBox { page_id, .. } | Self::TableCell { page_id, .. } => page_id.as_deref(),
        }
    }
    #[must_use]
    pub fn master_id(&self) -> Option<&str> {
        match self {
            Self::TextBox { master_id, .. } | Self::TableCell { master_id, .. } => {
                master_id.as_deref()
            }
        }
    }
}

pub struct AuthoredTextSource<'a> {
    pub location: TextSourceLocation,
    pub text: &'a str,
    pub locked: bool,
}

/// Visits local text, pasteboard and each Master definition once. Generated
/// page-number fields are layout content rather than authored Find targets.
pub fn authored_text_sources(document: &Document) -> impl Iterator<Item = AuthoredTextSource<'_>> {
    document
        .pages
        .iter()
        .map(|page| (Some(page.id.as_str()), None, &page.objects))
        .chain(std::iter::once((None, None, &document.pasteboard.objects)))
        .chain(
            document
                .masters
                .iter()
                .map(|master| (None, Some(master.id.as_str()), &master.objects)),
        )
        .flat_map(|(page_id, master_id, objects)| {
            objects
                .iter()
                .filter(|object| object.text_field.is_none())
                .flat_map(move |object| object_sources(object, page_id, master_id))
        })
}

fn object_sources<'a>(
    object: &'a PublicationObject,
    page_id: Option<&'a str>,
    master_id: Option<&'a str>,
) -> Box<dyn Iterator<Item = AuthoredTextSource<'a>> + 'a> {
    match &object.kind {
        ObjectKind::Text { text, .. } => Box::new(std::iter::once(AuthoredTextSource {
            location: TextSourceLocation::TextBox {
                object_id: object.id.as_str().into(),
                page_id: page_id.map(str::to_owned),
                master_id: master_id.map(str::to_owned),
            },
            text,
            locked: object.locked,
        })),
        ObjectKind::Table { table } => {
            Box::new(table.cells.iter().enumerate().map(move |(index, text)| {
                AuthoredTextSource {
                    location: TextSourceLocation::TableCell {
                        object_id: object.id.as_str().into(),
                        page_id: page_id.map(str::to_owned),
                        master_id: master_id.map(str::to_owned),
                        row: u16::try_from(index / usize::from(table.columns))
                            .expect("validated Table row"),
                        column: u16::try_from(index % usize::from(table.columns))
                            .expect("validated Table column"),
                    },
                    text,
                    locked: object.locked,
                }
            }))
        }
        _ => Box::new(std::iter::empty()),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextSearchCursor {
    pub source_index: usize,
    pub byte_offset: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindTextInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub query: String,
    pub match_case: bool,
    #[serde(default)]
    pub whole_word: bool,
    pub scope: TextSearchScope,
    pub cursor: Option<TextSearchCursor>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSearchMatch {
    pub source: TextSourceLocation,
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
    pub locked: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindTextResult {
    pub document_id: String,
    pub revision: u64,
    pub matches: Vec<TextSearchMatch>,
    pub complete: bool,
    pub next_cursor: Option<TextSearchCursor>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceTextMatchInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub source: TextSourceLocation,
    pub start_byte: usize,
    pub end_byte: usize,
    pub expected_text: String,
    pub replacement: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceAllTextInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub query: String,
    pub match_case: bool,
    #[serde(default)]
    pub whole_word: bool,
    pub scope: TextSearchScope,
    pub replacement: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceTextResult {
    pub snapshot: EditorSnapshot,
    pub replaced: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextSourceSnapshotInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub scope: TextSearchScope,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedTextSource {
    pub source_index: usize,
    pub location: TextSourceLocation,
    pub text: String,
    pub locked: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSourceSnapshot {
    pub document_id: String,
    pub revision: u64,
    pub sources: Vec<OwnedTextSource>,
}

impl EditorModule {
    /// Freezes authored text before dispatching an offline language worker.
    /// # Errors
    /// Refuses stale document identities, revisions and invalid scopes.
    pub fn text_source_snapshot(
        &self,
        input: TextSourceSnapshotInput,
    ) -> EditorResult<TextSourceSnapshot> {
        let document = self.search_document(&input.document_id, input.expected_revision)?;
        require_scope(document, &input.scope)?;
        let sources = authored_text_sources(document)
            .enumerate()
            .filter(|(_, source)| scope_contains(&input.scope, &source.location))
            .map(|(source_index, source)| OwnedTextSource {
                source_index,
                location: source.location,
                text: source.text.to_owned(),
                locked: source.locked,
            })
            .collect();
        Ok(TextSourceSnapshot {
            document_id: input.document_id,
            revision: input.expected_revision,
            sources,
        })
    }
    /// Searches the requested canonical revision with an explicit continuation cursor.
    /// # Errors
    /// Refuses stale revisions, empty/excessive queries and invalid scopes/cursors.
    pub fn find_text(&self, input: FindTextInput) -> EditorResult<FindTextResult> {
        let document = self.search_document(&input.document_id, input.expected_revision)?;
        require_query(&input.query)?;
        require_scope(document, &input.scope)?;
        if input.limit == 0 || input.limit > MAX_RESULT_PAGE {
            return Err(invalid_input(
                "Find shows between one and 500 matches at a time.",
            ));
        }
        let mut matches = Vec::new();
        let mut cursor = None;
        let mut saw_cursor_source = input.cursor.is_none();
        for (index, source) in authored_text_sources(document).enumerate() {
            if let Some(after) = input.cursor {
                if index < after.source_index {
                    continue;
                }
                if index == after.source_index {
                    if !source.text.is_char_boundary(after.byte_offset) {
                        return Err(invalid_input(
                            "The Find position is invalid. Start Find again.",
                        ));
                    }
                    saw_cursor_source = true;
                }
            }
            if !scope_contains(&input.scope, &source.location) {
                continue;
            }
            for range in literal_matches(
                source.text,
                &input.query,
                input.match_case,
                input.whole_word,
            ) {
                if input.cursor.is_some_and(|after| {
                    index == after.source_index && range.start < after.byte_offset
                }) {
                    continue;
                }
                if matches.len() == input.limit {
                    return Ok(FindTextResult {
                        document_id: input.document_id,
                        revision: input.expected_revision,
                        matches,
                        complete: false,
                        next_cursor: cursor,
                    });
                }
                cursor = Some(TextSearchCursor {
                    source_index: index,
                    byte_offset: range.end,
                });
                matches.push(TextSearchMatch {
                    source: source.location.clone(),
                    start_byte: range.start,
                    end_byte: range.end,
                    text: source.text[range.start..range.end].into(),
                    locked: source.locked,
                });
            }
        }
        if !saw_cursor_source {
            return Err(invalid_input(
                "The Find position is invalid. Start Find again.",
            ));
        }
        Ok(FindTextResult {
            document_id: input.document_id,
            revision: input.expected_revision,
            matches,
            complete: true,
            next_cursor: None,
        })
    }

    /// Replaces one exact, still-current match in one Undo step.
    /// # Errors
    /// Refuses stale/changed matches, locked sources and saveability limits before mutation.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Owned decoded input matches the canonical native command boundary"
    )]
    pub fn replace_text_match(
        &mut self,
        input: ReplaceTextMatchInput,
    ) -> EditorResult<ReplaceTextResult> {
        let document = self.search_document(&input.document_id, input.expected_revision)?;
        require_replacement(&input.replacement)?;
        let source = authored_text_sources(document)
            .find(|source| source.location == input.source)
            .ok_or_else(|| invalid_input("That text no longer exists. Find it again."))?;
        let range = TextSourceRange {
            start: input.start_byte,
            end: input.end_byte,
        };
        if range.start >= range.end
            || source.text.get(range.start..range.end) != Some(input.expected_text.as_str())
        {
            return Err(invalid_input(
                "That match changed. Find it again before replacing.",
            ));
        }
        let command = replacement_command(&source, &[range], &input.replacement)?;
        self.execute_saveable(
            Revision::new(input.expected_revision),
            Transaction::single("Replace text", command),
            std::iter::empty(),
        )?;
        Ok(ReplaceTextResult {
            snapshot: self.snapshot()?,
            replaced: 1,
        })
    }

    /// Replaces every match in the chosen scope as one bounded, atomic Undo step.
    /// # Errors
    /// Refuses locked matches or excessive source/history budgets, without partial replacement.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Owned decoded input matches the canonical native command boundary"
    )]
    pub fn replace_all_text(
        &mut self,
        input: ReplaceAllTextInput,
    ) -> EditorResult<ReplaceTextResult> {
        let document = self.search_document(&input.document_id, input.expected_revision)?;
        require_query(&input.query)?;
        require_replacement(&input.replacement)?;
        require_scope(document, &input.scope)?;
        let mut commands = Vec::new();
        let mut replaced = 0_usize;
        for source in authored_text_sources(document) {
            if !scope_contains(&input.scope, &source.location) {
                continue;
            }
            let ranges = literal_matches(
                source.text,
                &input.query,
                input.match_case,
                input.whole_word,
            );
            if ranges.is_empty() {
                continue;
            }
            if commands.len() == MAX_REPLACEMENT_SOURCES {
                return Err(invalid_input(
                    "Replace All affects more than 50,000 text boxes or cells. Choose a smaller scope.",
                ));
            }
            replaced += ranges.len();
            commands.push(replacement_command(&source, &ranges, &input.replacement)?);
        }
        if commands.is_empty() {
            return Ok(ReplaceTextResult {
                snapshot: self.snapshot()?,
                replaced: 0,
            });
        }
        self.execute_saveable(
            Revision::new(input.expected_revision),
            Transaction {
                label: "Replace All".into(),
                commands,
            },
            std::iter::empty(),
        )?;
        Ok(ReplaceTextResult {
            snapshot: self.snapshot()?,
            replaced,
        })
    }

    fn search_document(&self, document_id: &str, revision: u64) -> EditorResult<&Document> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        if session.revision().value() != revision || session.document().id.as_str() != document_id {
            return Err(super::EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed. Find the text again.",
            ));
        }
        Ok(session.document())
    }
}

fn require_query(query: &str) -> EditorResult<()> {
    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err(invalid_input(
            "Enter Find text of between one and 1,024 bytes.",
        ));
    }
    Ok(())
}

fn require_replacement(text: &str) -> EditorResult<()> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(invalid_input(
            "The replacement exceeds the Text Box or cell text limit.",
        ));
    }
    Ok(())
}

fn require_scope(document: &Document, scope: &TextSearchScope) -> EditorResult<()> {
    let exists = match scope {
        TextSearchScope::Publication => true,
        TextSearchScope::Page { page_id } => document
            .pages
            .iter()
            .any(|page| page.id.as_str() == page_id),
        TextSearchScope::Master { master_id } => document
            .masters
            .iter()
            .any(|master| master.id.as_str() == master_id),
        TextSearchScope::Object { object_id } => {
            authored_text_sources(document).any(|source| source.location.object_id() == object_id)
        }
    };
    if !exists {
        return Err(invalid_input(
            "Choose a current Page, Text Box or Table to search.",
        ));
    }
    Ok(())
}

fn scope_contains(scope: &TextSearchScope, source: &TextSourceLocation) -> bool {
    match scope {
        TextSearchScope::Publication => true,
        TextSearchScope::Page { page_id } => source.page_id() == Some(page_id),
        TextSearchScope::Master { master_id } => source.master_id() == Some(master_id),
        TextSearchScope::Object { object_id } => source.object_id() == object_id,
    }
}

fn literal_matches(
    text: &str,
    query: &str,
    match_case: bool,
    whole_word: bool,
) -> Vec<TextSourceRange> {
    let ranges: Vec<_> = if match_case {
        text.match_indices(query)
            .map(|(start, found)| TextSourceRange {
                start,
                end: start + found.len(),
            })
            .collect()
    } else {
        let mut folded = String::new();
        let mut boundaries = vec![(0, 0)];
        for (index, ch) in text.char_indices() {
            folded.extend(ch.to_lowercase());
            boundaries.push((folded.len(), index + ch.len_utf8()));
        }
        let needle = query.to_lowercase();
        folded
            .match_indices(&needle)
            .filter_map(|(start, found)| {
                let first = boundaries
                    .binary_search_by_key(&start, |(folded, _)| *folded)
                    .ok()?;
                let last = boundaries
                    .binary_search_by_key(&(start + found.len()), |(folded, _)| *folded)
                    .ok()?;
                Some(TextSourceRange {
                    start: boundaries[first].1,
                    end: boundaries[last].1,
                })
            })
            .collect()
    };
    ranges
        .into_iter()
        .filter(|range| !whole_word || word_boundary(text, *range))
        .collect()
}

fn word_boundary(text: &str, range: TextSourceRange) -> bool {
    let is_word = |ch: char| {
        ch.is_alphanumeric()
            || ch.general_category_group() == GeneralCategoryGroup::Mark
            || ch.general_category() == GeneralCategory::ConnectorPunctuation
    };
    let apostrophe = |ch| ch == '\'' || ch == '’';
    let mut left = text[..range.start].chars().rev();
    let mut right = text[range.end..].chars();
    let before = left.next();
    let after = right.next();
    !before.is_some_and(is_word)
        && !after.is_some_and(is_word)
        && !before.is_some_and(|ch| apostrophe(ch) && left.next().is_some_and(is_word))
        && !after.is_some_and(|ch| apostrophe(ch) && right.next().is_some_and(is_word))
}

fn replacement_command(
    source: &AuthoredTextSource<'_>,
    ranges: &[TextSourceRange],
    replacement: &str,
) -> EditorResult<Command> {
    if source.locked {
        return Err(invalid_input(
            "A matching Text Box or Table is locked. Unlock it or choose a smaller scope.",
        ));
    }
    if ranges.len() > MAX_TEXT_EDIT_COUNT {
        return Err(invalid_input(
            "Too many matches in one Text Box or cell. Choose a longer Find phrase.",
        ));
    }
    let removed: usize = ranges.iter().map(|range| range.end - range.start).sum();
    let next_bytes = replacement
        .len()
        .checked_mul(ranges.len())
        .and_then(|added| source.text.len().checked_sub(removed)?.checked_add(added));
    if next_bytes.is_none_or(|bytes| bytes > MAX_TEXT_BYTES) {
        return Err(invalid_input(
            "The replaced text would exceed a Text Box or cell limit. Use a shorter replacement.",
        ));
    }
    let object_id = parse_object_id(source.location.object_id().into())?;
    match source.location {
        TextSourceLocation::TextBox { .. } => Ok(Command::ApplyTextEdits {
            object_id,
            edits: ranges
                .iter()
                .rev()
                .map(|range| TextEdit {
                    range: *range,
                    text: replacement.into(),
                    bold: None,
                    italic: None,
                    style: None,
                })
                .collect(),
        }),
        TextSourceLocation::TableCell { row, column, .. } => {
            let mut text = source.text.to_owned();
            for range in ranges.iter().rev() {
                text.replace_range(range.start..range.end, replacement);
            }
            Ok(Command::UpdateTableCell {
                object_id,
                row,
                column,
                text,
            })
        }
    }
}
