#!/usr/bin/env python3
"""Synthesized template-pack generator (P0-094, E0.10).

Renders the deterministic, versioned template pack FR-D-01 needs for
multi-scale template matching (ADR-0005: template matching first, ML
text-det second). Every template is synthesized in-house — no scraped
assets, no brand marks (see `02-REQUIREMENTS/06-open-questions.md`
#16): generic badge/text marks an AI generator might burn in.

Two packs from the same tooling, same seeds, same pixels:
  - `templates/dev/`  — the test pack. Pack label
    `SYNTHETIC-TEST-ONLY`, file names prefixed `synthetic-`; tests
    pin this pack so they never depend on prod files, and the label
    guarantees a dev template can never be mistaken for a real mark.
  - `templates/prod/` — the v1 production pack (`pack_version: 1`).
    New templates or any pixel change bumps `pack_version`; a shipped
    version's files are never mutated in place.

Templates (8): 4 badges (rounded-rect pill + text, translucent) and
4 text marks (transparent text only), all RGBA PNGs with real alpha.
Canonical sizes are recorded in the manifest; multi-scale matching
is the matcher's job (FR-D-01), not the pack's — the manifest
carries the recommended `scales` and `match_threshold` per template.

Manifest (`manifest.json` per pack): name, file, sha256, bytes, kind,
dimensions, `has_alpha`, `params` (text, font, sizes, colors,
opacity, corner radius, match threshold, scales), plus pack-level
`pack`, `pack_version`, `label`, `source: "synthesized"`,
`generator_version` and tool versions.

Determinism: fixed seeds (Pillow rendering only — no numpy, no
timestamps in PNG output), DejaVuSans from the system font path
(fail loudly if missing — a silent fallback font would change the
rendered pixels).

Usage:
  templates_gen.py [--out DIR]         generate both packs (default <repo>/templates)
  templates_gen.py --check [--out DIR] regenerate to temp and compare:
                                       template bytes, manifest sha256/
                                       bytes fields, pack labels.
Exit 0 on success, 1 on failure. No network, no secrets, no spending.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

GENERATOR_VERSION = 1
PACK_VERSION = 1
DEV_LABEL = "SYNTHETIC-TEST-ONLY"

FONT_CANDIDATES = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
]

# name, kind, text, font_size, width, height, bg(RGBA), fg(RGBA),
# corner_radius (badges only; 0 for text marks)
TEMPLATES = [
    ("badge-ai-generated", "badge", "AI GENERATED", 44, 480, 120,
     (255, 255, 255, 217), (30, 30, 30, 255), 24),
    ("badge-made-with-ai", "badge", "Made with AI", 44, 480, 120,
     (30, 30, 30, 204), (255, 255, 255, 255), 24),
    ("badge-ai", "badge", "AI", 56, 160, 120,
     (255, 255, 255, 217), (30, 30, 30, 255), 24),
    ("badge-sparkle-ai", "badge", "AI", 56, 200, 120,
     (255, 255, 255, 217), (30, 30, 30, 255), 24),
    ("text-ai-generated", "text", "AI GENERATED", 56, 640, 80,
     (0, 0, 0, 0), (255, 255, 255, 200), 0),
    ("text-made-with-ai", "text", "Made with AI", 56, 640, 80,
     (0, 0, 0, 0), (20, 20, 20, 200), 0),
    ("text-generated-with-ai", "text", "Generated with AI", 48, 720, 80,
     (0, 0, 0, 0), (255, 255, 255, 200), 0),
    ("text-ai", "text", "AI", 64, 160, 80,
     (0, 0, 0, 0), (255, 255, 255, 200), 0),
]

# Recommended matcher settings per kind (FR-D-01: multi-scale NCC).
MATCH_PARAMS = {
    "badge": {"match_threshold": 0.80, "scales": [0.5, 0.75, 1.0, 1.5, 2.0]},
    "text": {"match_threshold": 0.75, "scales": [0.5, 0.75, 1.0, 1.5, 2.0]},
}


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def tool_versions() -> dict[str, str]:
    import PIL

    return {"pillow": PIL.__version__}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def find_font() -> str:
    for candidate in FONT_CANDIDATES:
        if os.path.exists(candidate):
            return candidate
    raise SystemExit(
        "templates_gen: no usable TTF font found (looked for DejaVu/Liberation); "
        "install fonts-dejavu-core — refusing a silent fallback that would "
        "change the rendered templates"
    )


def draw_sparkle(draw: ImageDraw.ImageDraw, cx: int, cy: int, size: int, fill) -> None:
    """Four-point sparkle glyph (two nested diamonds)."""
    draw.polygon(
        [(cx, cy - size), (cx + size // 3, cy - size // 3),
         (cx + size, cy), (cx + size // 3, cy + size // 3),
         (cx, cy + size), (cx - size // 3, cy + size // 3),
         (cx - size, cy), (cx - size // 3, cy - size // 3)],
        fill=fill,
    )


def render_template(spec, font_path: str) -> Image.Image:
    name, kind, text, font_size, width, height, bg, fg, radius = spec
    img = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    font = ImageFont.truetype(font_path, size=font_size)
    if kind == "badge":
        draw.rounded_rectangle([0, 0, width - 1, height - 1], radius=radius, fill=bg)
        text_x = 0
        if name == "badge-sparkle-ai":
            # Sparkle glyph left, "AI" right of it.
            draw_sparkle(draw, 36, height // 2, 26, fg)
            text_x = 72
            draw.text((text_x, (height - font_size) // 2 - 4), text, fill=fg, font=font)
        else:
            tw = draw.textlength(text, font=font)
            draw.text(((width - tw) // 2, (height - font_size) // 2 - 4),
                      text, fill=fg, font=font)
    else:
        tw = draw.textlength(text, font=font)
        draw.text(((width - tw) // 2, (height - font_size) // 2 - 4),
                  text, fill=fg, font=font)
    return img


def template_entry(spec, file_name: str, path: Path) -> dict:
    name, kind, text, font_size, width, height, bg, fg, radius = spec
    params = {
        "text": text,
        "font": Path(find_font()).name,
        "font_size": font_size,
        "bg_rgba": list(bg),
        "fg_rgba": list(fg),
        "corner_radius": radius,
        **MATCH_PARAMS[kind],
    }
    return {
        "name": name,
        "file": file_name,
        "sha256": sha256_file(path),
        "bytes": path.stat().st_size,
        "kind": kind,
        "width": width,
        "height": height,
        "has_alpha": True,
        "params": params,
    }


def generate_pack(out_dir: Path, pack: str, versions: dict[str, str], font_path: str) -> dict:
    """Render one pack (dev or prod) into out_dir/<pack>/; return its manifest."""
    pack_dir = out_dir / pack
    pack_dir.mkdir(parents=True, exist_ok=True)
    label = DEV_LABEL if pack == "dev" else None
    prefix = "synthetic-" if pack == "dev" else ""
    entries = []
    for spec in TEMPLATES:
        name = spec[0]
        file_name = f"{prefix}{name}.png"
        path = pack_dir / file_name
        render_template(spec, font_path).save(path, format="PNG", compress_level=6)
        entries.append(template_entry(spec, file_name, path))
    manifest = {
        "pack": pack,
        "pack_version": PACK_VERSION,
        "label": label,
        "generator": "scripts/templates_gen.py",
        "generator_version": GENERATOR_VERSION,
        "source": "synthesized",
        "tool_versions": versions,
        "templates": entries,
    }
    (pack_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return manifest


def generate(out: Path) -> list[dict]:
    versions = tool_versions()
    font_path = find_font()
    out.mkdir(parents=True, exist_ok=True)
    return [generate_pack(out, pack, versions, font_path) for pack in ("dev", "prod")]


def check(out: Path) -> int:
    versions = tool_versions()
    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="templates-check-") as tmp:
        fresh = Path(tmp) / "templates"
        generate(fresh)
        # Pillow must match the committed manifests (pip-pinnable).
        committed_dev = json.loads((out / "dev" / "manifest.json").read_text(encoding="utf-8"))
        recorded = committed_dev["tool_versions"]
        if versions["pillow"] != recorded.get("pillow"):
            failures.append(
                f"toolchain drift: pillow is {versions['pillow']}, packs were "
                f"generated with {recorded.get('pillow')} — run "
                f"`just templates-gen` with the pinned version and commit"
            )
        for pack in ("dev", "prod"):
            manifest_path = out / pack / "manifest.json"
            if not manifest_path.exists():
                failures.append(f"missing manifest: {pack}/manifest.json")
                continue
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            want_label = DEV_LABEL if pack == "dev" else None
            if manifest.get("label") != want_label:
                failures.append(
                    f"{pack}/manifest.json: label {manifest.get('label')!r} != {want_label!r}"
                )
            if manifest.get("pack_version") != PACK_VERSION:
                failures.append(f"{pack}/manifest.json: pack_version drift")
            fresh_manifest = json.loads((fresh / pack / "manifest.json").read_text(encoding="utf-8"))
            fresh_by_name = {t["name"]: t for t in fresh_manifest["templates"]}
            for entry in manifest["templates"]:
                path = out / pack / entry["file"]
                if not path.exists():
                    failures.append(f"missing template file: {pack}/{entry['file']}")
                    continue
                actual = sha256_file(path)
                if entry.get("sha256") != actual:
                    failures.append(
                        f"manifest checksum stale: {pack}/{entry['file']} "
                        f"(manifest {entry.get('sha256', '')[:12]}… != file {actual[:12]}…)"
                    )
                if entry.get("bytes") != path.stat().st_size:
                    failures.append(f"manifest bytes stale: {pack}/{entry['file']}")
                fresh_entry = fresh_by_name.get(entry["name"])
                if fresh_entry is None:
                    failures.append(f"generator dropped template: {entry['name']}")
                elif fresh_entry["sha256"] != actual:
                    failures.append(
                        f"regen drift: {pack}/{entry['file']} — run "
                        f"`just templates-gen` and commit"
                    )
            if {t["name"] for t in manifest["templates"]} != set(fresh_by_name):
                failures.append(f"{pack}: template set differs from the generator")
    if failures:
        print("templates-check: FAILED", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("templates-check: OK — committed packs match the generator, manifest checksums verified")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=None,
                        help="templates dir (default: <repo>/templates)")
    parser.add_argument("--check", action="store_true",
                        help="regenerate to temp and compare; do not write")
    args = parser.parse_args(argv)
    out = args.out or (repo_root() / "templates")
    if args.check:
        return check(out)
    manifests = generate(out)
    total = sum(len(m["templates"]) for m in manifests)
    print(f"templates-gen: wrote {total} templates (dev + prod, v{PACK_VERSION}) to {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
