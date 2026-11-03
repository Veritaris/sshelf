# Bundled fonts

Embedded into the `sshelf` binary with `include_bytes!` (see `src/fonts.rs`). SPEC.md §10 *Visual design* says which font is used where.

| File | Font | Version | Source | Licence |
|---|---|---|---|---|
| `Inter-Regular.ttf` | Inter, 400 | 4.1 | `extras/ttf/` in https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip | SIL OFL 1.1, `Inter-LICENSE.txt` |
| `Inter-SemiBold.ttf` | Inter, 600 | 4.1 | same archive | SIL OFL 1.1, `Inter-LICENSE.txt` |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono, 400 | 2.304 | `fonts/ttf/` in https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip | SIL OFL 1.1, `JetBrainsMono-OFL.txt` |
| `lucide.ttf` | Lucide icons | 1.67.0 | `font/lucide.ttf` in the npm package `lucide-static@1.67.0` (https://registry.npmjs.org/lucide-static/-/lucide-static-1.67.0.tgz) | ISC, with the MIT notice for icons derived from Feather, `lucide-LICENSE` |

## SHA-256

```
40d692fce188e4471e2b3cba937be967878f631ad3ebbbdcd587687c7ebe0c82  Inter-Regular.ttf
78a843fade9d4612a5567302fb595b56976eb5fcebf4fea5a5912d638bafcde3  Inter-SemiBold.ttf
a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f  JetBrainsMono-Regular.ttf
23730b56d04bc009e9ecf6a0598ef9d82a3ba4ba055d4d9115976e4a385eff7e  lucide.ttf
```

## Licence files

- `lucide-LICENSE` is the package's `LICENSE` exactly as published.
- `Inter-LICENSE.txt` is `LICENSE.txt` from the Inter archive exactly as published.
- `JetBrainsMono-OFL.txt` is `OFL.txt` from the JetBrains Mono archive; the only difference is one trailing space at the end of line 21, which was dropped.

## Icon codepoints

`src/icons.rs` names the icons sshelf uses; their codepoints come from `font/codepoints.json` in the same `lucide-static` package. When updating Lucide, check every codepoint in `icons.rs` against the new version's file.
