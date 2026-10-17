# Font controls

The CFF controls are renamed, tiny derivatives of the in-repository Noto Sans CJK source. Copyright and OFL metadata are retained, and `LICENSE-OFL` accompanies the files. `provenance.json` binds the approved source, the reproducible parent subset and every exact derivative. `generate_controls.py` uses fontTools directly on that source; it has no evidence-directory input.

The positive controls cover default, explicit Top and relative FD affine matrices, reflection, all four flex operators, fractional curve extrema, a completed empty glyph and distinct OpenType/charstring advances. `absent-top.otf` is retained only to characterize normalization; it is excluded from positive qualification. The three files listed as malformed in the provenance deliberately omit termination or complete path data.

`original.ttf` is an original project control under MIT OR Apache-2.0. Its rights flag requires full embedding. It contains an empty glyph, a simple capital, a quadratic and an unused composite attached by point indices, with a distinct top-level bearing. The expected PDF enclosure is `[0, 0, 380, 751]`; the quadratic reaches 750.5 rather than its 801-unit control-point height.

Regenerate with `python apps/font-worker/src/tests/generate_controls.py` in a development environment containing fontTools. Run the focused independent probe with explicit `--worker PATH --output NEW_DIRECTORY` arguments. The probe measures exact prepared programs; confinement and final PDF qualification are separate checks.

The implicit/explicit default, scaled, and translated pairs exercise CID
initialization when Top FontMatrix is absent. Explicit-default controls force
that raw DICT entry to remain present; fontTools normally elides the default.
The signed tiny decimal translation controls retain exact BCD +/-1E-18 operands.
Their final box is a certified numeric enclosure encoded outward in whole PDF
units. The output is not described as the mathematically minimal integer box.
The probe records ideal floating geometry, emitted bounds, conditional outward
representation expansion, exact Fraction matrix/line controls, and independent
source/prepared path and advance bindings. Finite controls do not establish an
exact symbolic proof for arbitrary glyph curves.

The three ascent-* controls append an actual H-plus-circumflex role to the same
approved source subset. ascent-facts.json records the complete authored role
set[2], source identities and expected Ascent880 independently of cmap. The
high unencoded role remains unresolved for print without facts; coded U+0124
is automatically excluded, and the low unencoded role cannot change the known
maximum. Native validation checks fact shape, range and identity; semantic
truth remains the author's factual premise. None and an explicit empty set
have distinct attested identities. These exclusions affect Ascent alone.
