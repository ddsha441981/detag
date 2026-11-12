# Template packs

Synthesized badge/text templates for FR-D-01's multi-scale template
matching (ADR-0005). Generated, never hand-edited or scraped:
`just templates-gen` regenerates both packs; `just templates-check`
fails on any drift.

## Packs

| Pack | Path | Contents |
| --- | --- | --- |
| dev | `templates/dev/` | Same 8 templates, labeled `SYNTHETIC-TEST-ONLY`, file names prefixed `synthetic-`. Tests pin this pack so they never depend on prod files. |
| prod | `templates/prod/` | The v1 production pack (`pack_version: 1`). |

Both packs are rendered by the same tooling from the same seeds, so
their pixels are identical — only the pack label, directory and file
prefix differ. That is the whole point: a dev template can never be
mistaken for a real mark, and the production pack still comes from
the same (non-scraping) tooling.

## Templates (8)

Badges — translucent rounded-rect pill + text (RGBA):
`badge-ai-generated`, `badge-made-with-ai`, `badge-ai`,
`badge-sparkle-ai` (sparkle glyph + "AI").

Text marks — transparent text only (RGBA):
`text-ai-generated`, `text-made-with-ai`, `text-generated-with-ai`,
`text-ai`.

All text is generic ("AI GENERATED", "Made with AI", …) — no brand
or stock-agency marks are synthesized or scraped. Stock-agency mark
signatures, if ever needed, are a separate owner-approved decision
(they route to `needs_review` per the AUP, they are not removal
targets).

## Manifest (`manifest.json` per pack)

Pack-level: `pack`, `pack_version`, `label`, `generator`,
`generator_version`, `source: "synthesized"`, `tool_versions`.
Per template: `name`, `file`, `sha256`, `bytes`, `kind`,
`width`/`height`, `has_alpha`, and `params` (render text, font,
sizes, RGBA colors, corner radius, plus the matcher's recommended
`match_threshold` and `scales`).

## Versioning

`pack_version` bumps when a template is added, removed or
re-rendered. A shipped version's files are never mutated in place —
bump the version, regenerate, commit. The matcher reads the pack
version from the manifest and reports it, so a detection result is
always traceable to the pack that produced it.

## Regenerating

```sh
just templates-gen    # regenerate both packs in place
just templates-check  # regenerate to temp, compare bytes + manifest checksums
```

Determinism: fixed seeds, Pillow rendering only, DejaVuSans from the
system font path (fails loudly if missing — a silent fallback font
would change the pixels). Pillow's version is recorded in the
manifest and checked by `templates-check`.
