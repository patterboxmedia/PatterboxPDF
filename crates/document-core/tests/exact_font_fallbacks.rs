mod common;

use common::{TempDirectory, letter_document};
use document_core::model::{
    ExactFontFallbackChain, FontFaceBinding, FontFallbackScript,
    MAX_EXACT_FONT_FALLBACK_FACES_PER_SCRIPT, ModelError,
};
use document_core::package::{PackageError, ProjectSnapshot, open_package, save_new_package};
use document_core::{
    AssetHash, AssetStore, Command, CommandError, DocumentSession, Revision, Transaction,
};

const FALLBACK_FONT: &[u8] =
    include_bytes!("../../layout-core/tests/fixtures/NotoEmoji-Regular.ttf");

fn face(source: AssetHash) -> FontFaceBinding {
    FontFaceBinding {
        source,
        face_index: 0,
    }
}

#[test]
fn publication_fallback_chain_is_atomic_undoable_and_package_round_trippable() {
    let mut assets = AssetStore::default();
    let source = assets.insert("font/ttf", FALLBACK_FONT.to_vec());
    let fallback = ExactFontFallbackChain {
        script: FontFallbackScript::Latin,
        faces: vec![face(source.clone())],
    };
    let mut session = DocumentSession::new(letter_document()).unwrap();

    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Set publication font fallback",
                Command::SetExactFontFallbacks {
                    fallbacks: vec![fallback.clone()],
                },
            ),
        )
        .unwrap();
    assert_eq!(
        session.document().exact_font_fallbacks,
        vec![fallback.clone()]
    );

    session.undo(Revision::new(1)).unwrap();
    assert!(session.document().exact_font_fallbacks.is_empty());
    session.redo(Revision::new(2)).unwrap();
    assert_eq!(session.document().exact_font_fallbacks, vec![fallback]);

    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("exact-font-fallback-roundtrip");
    let destination = temp.path().join("fallback.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).unwrap();
    assert_eq!(open_package(&destination).unwrap().snapshot, snapshot);

    let error = save_new_package(
        &temp.path().join("fallback-missing.publisher-v2"),
        &snapshot,
        &AssetStore::default(),
    )
    .unwrap_err();
    assert!(matches!(error, PackageError::MissingAsset(found) if found == source));
}

#[test]
fn malformed_fallback_chains_fail_without_mutating_the_document() {
    let source = AssetHash::for_bytes(FALLBACK_FONT);
    let latin = || ExactFontFallbackChain {
        script: FontFallbackScript::Latin,
        faces: vec![face(source.clone())],
    };
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let error = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Reject duplicate fallback scripts",
                Command::SetExactFontFallbacks {
                    fallbacks: vec![latin(), latin()],
                },
            ),
        )
        .unwrap_err();

    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::ExactFontFallbacksNotNormalized)
    );
    assert!(session.document().exact_font_fallbacks.is_empty());
    assert_eq!(session.revision(), Revision::initial());

    let error = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Reject an empty fallback chain",
                Command::SetExactFontFallbacks {
                    fallbacks: vec![ExactFontFallbackChain {
                        script: FontFallbackScript::Latin,
                        faces: Vec::new(),
                    }],
                },
            ),
        )
        .unwrap_err();
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::InvalidExactFontFallbackChain)
    );
    assert!(session.document().exact_font_fallbacks.is_empty());

    let scripts = [
        FontFallbackScript::Arabic,
        FontFallbackScript::Bengali,
        FontFallbackScript::Cyrillic,
        FontFallbackScript::Devanagari,
        FontFallbackScript::Greek,
    ];
    let oversized = scripts
        .into_iter()
        .enumerate()
        .map(|(script_index, script)| ExactFontFallbackChain {
            script,
            faces: (0..MAX_EXACT_FONT_FALLBACK_FACES_PER_SCRIPT)
                .map(|face_index| {
                    face(AssetHash::for_bytes(&[
                        u8::try_from(script_index).unwrap(),
                        u8::try_from(face_index).unwrap(),
                    ]))
                })
                .collect(),
        })
        .collect();
    let error = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Reject an excessive total fallback resource set",
                Command::SetExactFontFallbacks {
                    fallbacks: oversized,
                },
            ),
        )
        .unwrap_err();
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::TooManyExactFontFallbackFaces)
    );
    assert!(session.document().exact_font_fallbacks.is_empty());
}
