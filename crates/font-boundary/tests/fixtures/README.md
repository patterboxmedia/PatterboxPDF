`truncated-dejavu-sfnt.ttf` is the first 336 bytes of the existing
`crates/layout-core/tests/fixtures/DejaVuSans.ttf` test font (SHA-256
`abdc775b21b1bc470d50c97e790d276f2054b7504e56e5bd3e64f48d68582322`).
It preserves the SFNT header, all 20 table-directory records, and four bytes of
table payload, then stops before the declared tables. It exercises malformed
TrueType parsing after format recognition. The complete font is the valid
control in the same confined-worker test.

DejaVu's license is retained at
`crates/layout-core/tests/fixtures/LICENSE-DejaVu`; the truncated derivative
is test data and is not a usable font.
