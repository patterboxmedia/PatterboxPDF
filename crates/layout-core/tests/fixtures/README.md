# DejaVu Sans test fixture

The DejaVu Sans 2.37 regular, bold, oblique, and bold-oblique faces are used
only by exact-font layout and print integration tests. The regular face was
copied from the font assets distributed with the locked `pdf_oxide` 0.3.73
crate. The other three faces came from the official DejaVu 2.37 TTF release.
All four are redistributed under the adjacent `LICENSE-DejaVu` terms.

- `DejaVuSans.ttf`: `abdc775b21b1bc470d50c97e790d276f2054b7504e56e5bd3e64f48d68582322`
- `DejaVuSans-Bold.ttf`: `e6476c1b80502924294eed40894c5b18e06c181444ca953e5334262df9c27724`
- `DejaVuSans-Oblique.ttf`: `4af75fa16ee6d3ad43e1ecec41862c24954af26a55c6bb1ebb27bd486a50f5f4`
- `DejaVuSans-BoldOblique.ttf`: `eb436dca0c2594b73d8b603b892e374fdfd8d885d25ffb4f18df4c4c0b49e50f`
- Upstream project: <https://dejavu-fonts.github.io/>
- Official release: <https://github.com/dejavu-fonts/dejavu-fonts/releases/tag/version_2_37>
- Redistribution source: `pdf_oxide-0.3.73/src/fonts/assets/DejaVuSans.ttf`

## Noto Emoji fallback fixture

`NotoEmoji-Regular.ttf` is a monochrome fallback face used only to prove a
real second-face fallback without depending on the operating system's font
inventory. It is redistributed under the adjacent `LICENSE-NotoEmoji` terms.
Copyright 2013 Google LLC and contributors.

- SHA-256: `415dc6290378574135b64c808dc640c1df7531973290c4970c51fdeb849cb0c5`
- Upstream project: <https://github.com/googlefonts/noto-emoji>
- Pinned source commit: `2f1ffdd6fbbd05d6f382138a3d3adcd89c5ce800`
- Pinned source path: `fonts/NotoEmoji-Regular.ttf`
- License source commit: `8998f5dd683424a73e2314a8c1f1e359c19e8742`

`Noto-COLRv1-noflags.ttf` is the upstream no-flags color build used only to
exercise the real COLR/CPAL path. It shares `LICENSE-NotoEmoji` and the same
copyright attribution.

- SHA-256: `469e3b92d63cfc203789f8742f1835b8672c7b5995ab4a832f1699b712a5afcc`
- Pinned source commit: `8998f5dd683424a73e2314a8c1f1e359c19e8742`
- Pinned source path: `fonts/Noto-COLRv1-noflags.ttf`

## Noto multiscript fallback fixtures

These three faces are used only to prove deterministic, publication-authored
fallback for representative Devanagari, Thai, and Simplified Chinese text.
Each is redistributed under its adjacent OFL 1.1 license. The Devanagari and
Thai binaries are pinned Google Fonts builds; the CJK face is the official
static Regular build from Noto CJK. A Google Fonts variable CJK build was
evaluated and rejected because its thin default instance required synthetic
weight at ordinary 400 text, which Publisher V2 correctly refuses for exact
print output.

- `NotoSansDevanagari-VF.ttf`: 647,144 bytes; SHA-256 `9ce7b04f60e363d8870e5997744cf85cf69d38a4d7d129d364d92a3b14b461d7`
- `NotoSansThai-VF.ttf`: 218,652 bytes; SHA-256 `5a1c559bb539583c8a1fd99d1c5b9491e5e14478c9cd2bd0970d5c3096cc9ef8`
- Upstream project: <https://github.com/google/fonts>
- Google Fonts source commit: `f265cc2d8e08067dac782ba633458b97661ab85d`
- Google Fonts source paths: `ofl/notosansdevanagari/NotoSansDevanagari[wdth,wght].ttf` and `ofl/notosansthai/NotoSansThai[wdth,wght].ttf`
- Adjacent license SHA-256 values: `LICENSE-NotoSansDevanagari` is `a216f6f8d85c7228093e0ee5e258d9d377e6671f68acb4db1930b29583d0f331`; `LICENSE-NotoSansThai` is `2e98fd23a52d253db8612cd5942c8f2ff4111b21d2367050fdca91d8ccc374a0`
- `NotoSansCJKsc-Regular.otf`: 16,437,364 bytes; SHA-256 `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b`
- Upstream project: <https://github.com/notofonts/noto-cjk>
- Noto CJK source commit: `523d033d6cb47f4a80c58a35753646f5c3608a78` (`Sans2.004`)
- Noto CJK source path: `Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf`
- Adjacent `LICENSE-NotoSansCJK` SHA-256: `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2`

## Real SVG and bitmap color-font fixtures

These faces exercise actual unsupported color-artwork tables, not synthetic
parser structures. Both remain explicit preview facts and must be rejected
before print preparation while Publisher V2 has no trusted color-font painter.
They are Noto Emoji derivatives redistributed under `LICENSE-NotoEmoji`.

- `NotoColorEmoji-SVG.otf`: 13,296,760 bytes; SHA-256 `3c0a539c58c7f3f40717a9d8e27af6addb137491d1e33434e9230205af89de56`
- Upstream project: <https://github.com/adobe-fonts/noto-emoji-svg>
- Official release: `0.008`; annotated tag commit `f375d5db957a8ea47bd17b13af928bde1948c433`
- Official release asset: `NotoColorEmoji-SVG.otf`
- `NotoColorEmoji-CBDT.ttf`: 10,673,480 bytes; SHA-256 `72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b`
- Upstream project: <https://github.com/googlefonts/noto-emoji>
- Pinned source commit: `8998f5dd683424a73e2314a8c1f1e359c19e8742`
- Pinned source path: `fonts/NotoColorEmoji.ttf`
