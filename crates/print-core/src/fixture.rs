use color_boundary::{COLOR_ENGINE_IDENTITY, COLOR_RULES_IDENTITY, TrustedColorConversion};
use document_core::Revision;
use layout_core::ResolvedPublication;
use sha2::{Digest, Sha256};
use std::fmt::Write;

use crate::{ArtifactHash, FontBoundary, OutputIntentFacts, ProfileHash};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum ColorRenderingIntent {
    RelativeColorimetric,
}

impl ColorRenderingIntent {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::RelativeColorimetric => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaggedRgbBoundary {
    NotExercised,
    WorkerAttested {
        source_profile_hash: ProfileHash,
        raw_pixel_hash: ArtifactHash,
        width: u32,
        height: u32,
        rendering_intent: ColorRenderingIntent,
        engine_identity_hash: ArtifactHash,
        rules_identity_hash: ArtifactHash,
        worker_executable_hash: ArtifactHash,
    },
}

impl TaggedRgbBoundary {
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn expected(
        source_profile_hash: ProfileHash,
        raw_pixel_hash: ArtifactHash,
        width: u32,
        height: u32,
        worker_executable_hash: ArtifactHash,
    ) -> Self {
        Self::WorkerAttested {
            source_profile_hash,
            raw_pixel_hash,
            width,
            height,
            rendering_intent: ColorRenderingIntent::RelativeColorimetric,
            engine_identity_hash: ArtifactHash::of(COLOR_ENGINE_IDENTITY.as_bytes()),
            rules_identity_hash: ArtifactHash::of(COLOR_RULES_IDENTITY.as_bytes()),
            worker_executable_hash,
        }
    }

    fn hash_into(self, digest: &mut Sha256) {
        match self {
            Self::NotExercised => digest.update([0]),
            Self::WorkerAttested {
                source_profile_hash,
                raw_pixel_hash,
                width,
                height,
                rendering_intent,
                engine_identity_hash,
                rules_identity_hash,
                worker_executable_hash,
            } => {
                digest.update([1]);
                digest.update(source_profile_hash.bytes());
                digest.update(raw_pixel_hash.bytes());
                digest.update(width.to_be_bytes());
                digest.update(height.to_be_bytes());
                digest.update([rendering_intent.tag()]);
                digest.update(engine_identity_hash.bytes());
                digest.update(rules_identity_hash.bytes());
                digest.update(worker_executable_hash.bytes());
            }
        }
    }
}

/// Identifies the resolved publication revision inspected by print preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreflightBasis {
    revision: Revision,
    scene_digest: [u8; 32],
}

impl From<ResolvedPublication> for PreflightBasis {
    fn from(value: ResolvedPublication) -> Self {
        Self {
            revision: value.revision(),
            scene_digest: value.semantic_digest(),
        }
    }
}

impl PreflightBasis {
    #[must_use]
    pub const fn revision(self) -> Revision {
        self.revision
    }

    /// Exact immutable scene evaluated by this print fixture.
    #[must_use]
    pub const fn scene_digest(self) -> [u8; 32] {
        self.scene_digest
    }
}

/// An immutable fingerprint of publication, page, color, font, and profile facts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EvaluationKey([u8; 32]);

impl EvaluationKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    #[must_use]
    pub fn hex(self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.0 {
            write!(&mut hex, "{byte:02x}").expect("writing to a string cannot fail");
        }
        hex
    }
}

/// A page rectangle in integer micrometres, using PDF bottom-left coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageBox {
    pub left_um: u32,
    pub bottom_um: u32,
    pub right_um: u32,
    pub top_um: u32,
}

impl PageBox {
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.left_um < self.right_um && self.bottom_um < self.top_um
    }

    #[must_use]
    pub const fn width_um(self) -> u32 {
        self.right_um - self.left_um
    }

    #[must_use]
    pub const fn height_um(self) -> u32 {
        self.top_um - self.bottom_um
    }

    #[must_use]
    pub const fn contains(self, inner: Self) -> bool {
        self.left_um <= inner.left_um
            && self.bottom_um <= inner.bottom_um
            && self.right_um >= inner.right_um
            && self.top_um >= inner.top_um
    }
}

/// Explicit physical page boxes. Nothing is inferred from a viewer crop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageBoxes {
    pub media: PageBox,
    pub trim: PageBox,
    pub bleed: PageBox,
}

impl PageBoxes {
    /// A4 trim with 3 mm bleed on every edge.
    pub const A4_WITH_3MM_BLEED: Self = Self {
        media: PageBox {
            left_um: 0,
            bottom_um: 0,
            right_um: 216_000,
            top_um: 303_000,
        },
        trim: PageBox {
            left_um: 3_000,
            bottom_um: 3_000,
            right_um: 213_000,
            top_um: 300_000,
        },
        bleed: PageBox {
            left_um: 0,
            bottom_um: 0,
            right_um: 216_000,
            top_um: 303_000,
        },
    };

    #[must_use]
    pub const fn is_nested(self) -> bool {
        self.media.is_valid()
            && self.bleed.is_valid()
            && self.trim.is_valid()
            && self.media.contains(self.bleed)
            && self.bleed.contains(self.trim)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorFact {
    DeviceCmykSwatch,
    IccTaggedRgbRaster,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransparencyFact {
    LiveVectorOpacity50Percent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterResolutionRule {
    target_ppi: u16,
    minimum_ppi: u16,
}

impl RasterResolutionRule {
    #[must_use]
    pub const fn target_ppi(self) -> u16 {
        self.target_ppi
    }

    #[must_use]
    pub const fn minimum_ppi(self) -> u16 {
        self.minimum_ppi
    }
}

/// Provider-replaceable effective-resolution policy bound into print evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterResolutionPreset {
    id: &'static str,
    continuous_tone: RasterResolutionRule,
    line_art: RasterResolutionRule,
}

impl RasterResolutionPreset {
    /// General commercial-print targets backed by the project's primary-source
    /// research. These are workflow policy, not PDF/X conformance rules.
    pub const COMMERCIAL_PRINT_V1: Self = Self {
        id: "commercial-print-v1",
        continuous_tone: RasterResolutionRule {
            target_ppi: 300,
            minimum_ppi: 200,
        },
        line_art: RasterResolutionRule {
            target_ppi: 1_200,
            minimum_ppi: 600,
        },
    };

    /// Defines a named built-in or provider profile whose thresholds are safe
    /// to use as preflight policy. Runtime-loaded profiles will use a separate
    /// owned representation when that boundary is introduced.
    #[must_use]
    pub const fn from_static_thresholds(
        id: &'static str,
        continuous_tone_target_ppi: u16,
        continuous_tone_minimum_ppi: u16,
        line_art_target_ppi: u16,
        line_art_minimum_ppi: u16,
    ) -> Option<Self> {
        if id.is_empty()
            || continuous_tone_minimum_ppi == 0
            || continuous_tone_target_ppi < continuous_tone_minimum_ppi
            || line_art_minimum_ppi == 0
            || line_art_target_ppi < line_art_minimum_ppi
        {
            return None;
        }
        Some(Self {
            id,
            continuous_tone: RasterResolutionRule {
                target_ppi: continuous_tone_target_ppi,
                minimum_ppi: continuous_tone_minimum_ppi,
            },
            line_art: RasterResolutionRule {
                target_ppi: line_art_target_ppi,
                minimum_ppi: line_art_minimum_ppi,
            },
        })
    }

    #[must_use]
    pub const fn id(self) -> &'static str {
        self.id
    }

    #[must_use]
    pub const fn continuous_tone(self) -> RasterResolutionRule {
        self.continuous_tone
    }

    #[must_use]
    pub const fn line_art(self) -> RasterResolutionRule {
        self.line_art
    }
}

/// The smallest one-page experiment shared by every candidate print route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureContract {
    pub basis: PreflightBasis,
    pub boxes: PageBoxes,
    pub output_intent: OutputIntentFacts,
    pub font: FontBoundary,
    pub raster_resolution: RasterResolutionPreset,
    pub(crate) tagged_rgb: TaggedRgbBoundary,
    pub colors: [ColorFact; 2],
    pub transparency: TransparencyFact,
}

impl FixtureContract {
    #[must_use]
    pub fn new(
        basis: PreflightBasis,
        output_intent: OutputIntentFacts,
        font: FontBoundary,
    ) -> Self {
        Self {
            basis,
            boxes: PageBoxes::A4_WITH_3MM_BLEED,
            output_intent,
            font,
            raster_resolution: RasterResolutionPreset::COMMERCIAL_PRINT_V1,
            tagged_rgb: TaggedRgbBoundary::NotExercised,
            colors: [ColorFact::DeviceCmykSwatch, ColorFact::IccTaggedRgbRaster],
            transparency: TransparencyFact::LiveVectorOpacity50Percent,
        }
    }

    #[must_use]
    pub fn with_trusted_color(mut self, conversion: &TrustedColorConversion) -> Self {
        self.tagged_rgb = TaggedRgbBoundary::WorkerAttested {
            source_profile_hash: ProfileHash::from_bytes(conversion.source_profile_hash()),
            raw_pixel_hash: ArtifactHash::from_bytes(conversion.raw_pixel_hash()),
            width: conversion.width(),
            height: conversion.height(),
            rendering_intent: ColorRenderingIntent::RelativeColorimetric,
            engine_identity_hash: ArtifactHash::from_bytes(conversion.engine_identity_hash()),
            rules_identity_hash: ArtifactHash::from_bytes(conversion.rules_identity_hash()),
            worker_executable_hash: ArtifactHash::from_bytes(conversion.worker_executable_hash()),
        };
        self
    }

    #[must_use]
    pub const fn with_raster_resolution_preset(mut self, preset: RasterResolutionPreset) -> Self {
        self.raster_resolution = preset;
        self
    }

    #[cfg(test)]
    pub(crate) const fn with_tagged_rgb(mut self, tagged_rgb: TaggedRgbBoundary) -> Self {
        self.tagged_rgb = tagged_rgb;
        self
    }

    #[must_use]
    pub fn evaluation_key(&self) -> EvaluationKey {
        let mut digest = Sha256::new();
        digest.update(b"publisher-v2/print-evaluation/v1\0");
        digest.update(self.basis.revision().value().to_be_bytes());
        digest.update(self.basis.scene_digest());
        for page_box in [self.boxes.media, self.boxes.trim, self.boxes.bleed] {
            for value in [
                page_box.left_um,
                page_box.bottom_um,
                page_box.right_um,
                page_box.top_um,
            ] {
                digest.update(value.to_be_bytes());
            }
        }
        digest.update(self.output_intent.sha256.bytes());
        digest.update((self.output_intent.byte_len as u64).to_be_bytes());
        digest.update(self.font.stable_bytes());
        digest.update(
            u64::try_from(self.raster_resolution.id().len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(self.raster_resolution.id().as_bytes());
        for rule in [
            self.raster_resolution.continuous_tone(),
            self.raster_resolution.line_art(),
        ] {
            digest.update(rule.target_ppi().to_be_bytes());
            digest.update(rule.minimum_ppi().to_be_bytes());
        }
        self.tagged_rgb.hash_into(&mut digest);
        digest.update(b"device-cmyk+icc-rgb+live-alpha-50");
        EvaluationKey(digest.finalize().into())
    }

    #[must_use]
    pub const fn profile_hash(&self) -> ProfileHash {
        self.output_intent.sha256
    }
}

#[cfg(test)]
mod tests {
    use document_core::Revision;
    use layout_core::ResolvedPublication;

    use super::{
        ColorRenderingIntent, FixtureContract, PageBoxes, PreflightBasis, RasterResolutionPreset,
        RasterResolutionRule, TaggedRgbBoundary,
    };
    use crate::{
        ArtifactHash, FontBoundary, IccColorSpace, IccDeviceClass, OutputIntentFacts, ProfileHash,
    };

    fn profile_facts() -> OutputIntentFacts {
        OutputIntentFacts::from_test_parts(
            ProfileHash::from_bytes([7; 32]),
            1_024,
            1_024,
            IccDeviceClass::Printer,
            IccColorSpace::Cmyk,
            *b"Lab ",
        )
    }

    #[test]
    fn page_boxes_are_explicit_and_correctly_nested() {
        let boxes = PageBoxes::A4_WITH_3MM_BLEED;
        assert!(boxes.is_nested());
        assert_eq!(
            (boxes.trim.width_um(), boxes.trim.height_um()),
            (210_000, 297_000)
        );
    }

    #[test]
    fn zero_area_and_inverted_boxes_are_not_valid() {
        let zero_width = super::PageBox {
            left_um: 10,
            bottom_um: 0,
            right_um: 10,
            top_um: 20,
        };
        let inverted = super::PageBox {
            left_um: 20,
            bottom_um: 0,
            right_um: 10,
            top_um: 20,
        };
        assert!(!zero_width.is_valid());
        assert!(!inverted.is_valid());

        let mut boxes = PageBoxes::A4_WITH_3MM_BLEED;
        boxes.trim = inverted;
        assert!(!boxes.is_nested());
    }

    #[test]
    fn evaluation_key_changes_with_resolved_revision() {
        let first = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            profile_facts(),
            FontBoundary::NotExercised,
        );
        let next = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(
                Revision::initial().next(),
            )),
            profile_facts(),
            FontBoundary::NotExercised,
        );
        assert_ne!(first.evaluation_key(), next.evaluation_key());
    }

    #[test]
    fn commercial_print_resolution_policy_is_named_and_bound_into_evidence() {
        let base = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            profile_facts(),
            FontBoundary::NotExercised,
        );
        let preset = RasterResolutionPreset::COMMERCIAL_PRINT_V1;
        assert_eq!(preset.id(), "commercial-print-v1");
        assert_eq!(
            (
                preset.continuous_tone().target_ppi(),
                preset.continuous_tone().minimum_ppi()
            ),
            (300, 200)
        );
        assert_eq!(
            (
                preset.line_art().target_ppi(),
                preset.line_art().minimum_ppi()
            ),
            (1_200, 600)
        );

        let stricter = RasterResolutionPreset {
            id: "strict-print-test",
            continuous_tone: RasterResolutionRule {
                target_ppi: 350,
                minimum_ppi: 250,
            },
            line_art: RasterResolutionRule {
                target_ppi: 1_200,
                minimum_ppi: 600,
            },
        };
        assert_ne!(
            base.evaluation_key(),
            base.with_raster_resolution_preset(stricter)
                .evaluation_key()
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn evaluation_key_binds_every_tagged_rgb_input_and_worker_identity() {
        let base = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            profile_facts(),
            FontBoundary::NotExercised,
        );
        let boundary = TaggedRgbBoundary::WorkerAttested {
            source_profile_hash: ProfileHash::from_bytes([1; 32]),
            raw_pixel_hash: ArtifactHash::from_bytes([2; 32]),
            width: 2,
            height: 3,
            rendering_intent: ColorRenderingIntent::RelativeColorimetric,
            engine_identity_hash: ArtifactHash::from_bytes([3; 32]),
            rules_identity_hash: ArtifactHash::from_bytes([4; 32]),
            worker_executable_hash: ArtifactHash::from_bytes([5; 32]),
        };
        let expected = base.clone().with_tagged_rgb(boundary).evaluation_key();
        let TaggedRgbBoundary::WorkerAttested {
            source_profile_hash,
            raw_pixel_hash,
            width,
            height,
            rendering_intent,
            engine_identity_hash,
            rules_identity_hash,
            worker_executable_hash,
        } = boundary
        else {
            unreachable!("test boundary is worker-attested")
        };
        let variants = [
            TaggedRgbBoundary::WorkerAttested {
                source_profile_hash: ProfileHash::from_bytes([9; 32]),
                raw_pixel_hash,
                width,
                height,
                rendering_intent,
                engine_identity_hash,
                rules_identity_hash,
                worker_executable_hash,
            },
            TaggedRgbBoundary::WorkerAttested {
                source_profile_hash,
                raw_pixel_hash: ArtifactHash::from_bytes([9; 32]),
                width,
                height,
                rendering_intent,
                engine_identity_hash,
                rules_identity_hash,
                worker_executable_hash,
            },
            TaggedRgbBoundary::WorkerAttested {
                source_profile_hash,
                raw_pixel_hash,
                width: 9,
                height,
                rendering_intent,
                engine_identity_hash,
                rules_identity_hash,
                worker_executable_hash,
            },
            TaggedRgbBoundary::WorkerAttested {
                source_profile_hash,
                raw_pixel_hash,
                width,
                height: 9,
                rendering_intent,
                engine_identity_hash,
                rules_identity_hash,
                worker_executable_hash,
            },
            TaggedRgbBoundary::WorkerAttested {
                source_profile_hash,
                raw_pixel_hash,
                width,
                height,
                rendering_intent,
                engine_identity_hash: ArtifactHash::from_bytes([9; 32]),
                rules_identity_hash,
                worker_executable_hash,
            },
            TaggedRgbBoundary::WorkerAttested {
                source_profile_hash,
                raw_pixel_hash,
                width,
                height,
                rendering_intent,
                engine_identity_hash,
                rules_identity_hash: ArtifactHash::from_bytes([9; 32]),
                worker_executable_hash,
            },
            TaggedRgbBoundary::WorkerAttested {
                source_profile_hash,
                raw_pixel_hash,
                width,
                height,
                rendering_intent,
                engine_identity_hash,
                rules_identity_hash,
                worker_executable_hash: ArtifactHash::from_bytes([9; 32]),
            },
        ];
        for variant in variants {
            assert_ne!(
                expected,
                base.clone().with_tagged_rgb(variant).evaluation_key()
            );
        }
    }
}
