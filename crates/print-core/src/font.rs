use font_boundary::PreparedFontSubset;

/// Stable evidence at the font-embedding boundary; it never contains font data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FontBoundary {
    NotExercised,
    PermissionBlocked,
    SubsetPrepared(Box<SubsetFontBoundary>),
    FullEmbeddingRequired {
        source_sha256: [u8; 32],
        face_index: u32,
    },
}

/// Compactly boxed identity facts for one worker-prepared font subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubsetFontBoundary {
    pub source_sha256: [u8; 32],
    pub face_index: u32,
    pub face_identity_sha256: [u8; 32],
    pub glyph_set_sha256: [u8; 32],
    pub glyph_count: u16,
    pub subset_sha256: [u8; 32],
    pub rights_identity_sha256: [u8; 32],
    pub worker_executable_sha256: [u8; 32],
    pub engine_identity_sha256: [u8; 32],
    pub rules_identity_sha256: [u8; 32],
}

impl FontBoundary {
    #[must_use]
    pub fn from_prepared(subset: &PreparedFontSubset) -> Self {
        Self::SubsetPrepared(Box::new(SubsetFontBoundary {
            source_sha256: subset.source_sha256(),
            face_index: subset.face_index(),
            face_identity_sha256: subset.face_identity_sha256(),
            glyph_set_sha256: subset.glyph_set_sha256(),
            glyph_count: subset.glyph_count(),
            subset_sha256: subset.subset_sha256(),
            rights_identity_sha256: subset.rights_identity_sha256(),
            worker_executable_sha256: subset.worker_executable_sha256(),
            engine_identity_sha256: subset.engine_identity_sha256(),
            rules_identity_sha256: subset.rules_identity_sha256(),
        }))
    }

    pub(crate) fn stable_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(265);
        match self {
            Self::NotExercised => bytes.push(0),
            Self::PermissionBlocked => bytes.push(1),
            Self::SubsetPrepared(evidence) => {
                bytes.push(2);
                bytes.extend_from_slice(&evidence.source_sha256);
                bytes.extend_from_slice(&evidence.face_index.to_be_bytes());
                bytes.extend_from_slice(&evidence.face_identity_sha256);
                bytes.extend_from_slice(&evidence.glyph_set_sha256);
                bytes.extend_from_slice(&evidence.glyph_count.to_be_bytes());
                bytes.extend_from_slice(&evidence.subset_sha256);
                bytes.extend_from_slice(&evidence.rights_identity_sha256);
                bytes.extend_from_slice(&evidence.worker_executable_sha256);
                bytes.extend_from_slice(&evidence.engine_identity_sha256);
                bytes.extend_from_slice(&evidence.rules_identity_sha256);
            }
            Self::FullEmbeddingRequired {
                source_sha256,
                face_index,
            } => {
                bytes.push(3);
                bytes.extend_from_slice(source_sha256);
                bytes.extend_from_slice(&face_index.to_be_bytes());
            }
        }
        bytes
    }
}
