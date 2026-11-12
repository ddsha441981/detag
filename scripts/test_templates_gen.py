#!/usr/bin/env python3
"""Unit tests for templates_gen.py (P0-094).

Pure/deterministic helpers only — full pack generation is exercised
by `just templates-gen` + `just templates-check`.
Run: python3 -m unittest discover -s scripts -p "test_*.py"
"""

from __future__ import annotations

import hashlib
import io
import json
import unittest

import templates_gen


class TestPackShape(unittest.TestCase):
    def test_eight_templates_four_badges_four_texts(self) -> None:
        kinds = [spec[1] for spec in templates_gen.TEMPLATES]
        self.assertEqual(len(templates_gen.TEMPLATES), 8)
        self.assertEqual(kinds.count("badge"), 4)
        self.assertEqual(kinds.count("text"), 4)

    def test_names_are_unique(self) -> None:
        names = [spec[0] for spec in templates_gen.TEMPLATES]
        self.assertEqual(len(names), len(set(names)))

    def test_no_brand_or_stock_names(self) -> None:
        # Synthesized generic marks only — no scraped/trademarked text.
        blob = " ".join(spec[2] for spec in templates_gen.TEMPLATES).lower()
        for banned in ("getty", "shutterstock", "adobe", "dreamstime", "istock"):
            self.assertNotIn(banned, blob)


class TestDeterminism(unittest.TestCase):
    def test_render_twice_is_byte_identical(self) -> None:
        font = templates_gen.find_font()
        for spec in templates_gen.TEMPLATES:
            first = io.BytesIO()
            second = io.BytesIO()
            templates_gen.render_template(spec, font).save(first, format="PNG", compress_level=6)
            templates_gen.render_template(spec, font).save(second, format="PNG", compress_level=6)
            self.assertEqual(
                hashlib.sha256(first.getvalue()).hexdigest(),
                hashlib.sha256(second.getvalue()).hexdigest(),
                f"{spec[0]} must render deterministically",
            )

    def test_templates_have_real_alpha(self) -> None:
        font = templates_gen.find_font()
        for spec in templates_gen.TEMPLATES:
            img = templates_gen.render_template(spec, font)
            self.assertEqual(img.mode, "RGBA")
            alpha = img.getchannel("A")
            extrema = alpha.getextrema()
            # Real transparency somewhere (not a fully opaque rect) and
            # visible pixels at the spec's full foreground/background
            # alpha (text marks are translucent by design: fg alpha 200).
            expected_max = max(spec[6][3], spec[7][3])
            self.assertEqual(extrema, (0, expected_max), spec[0])

    def test_badge_and_text_render_differently(self) -> None:
        font = templates_gen.find_font()
        badge = next(s for s in templates_gen.TEMPLATES if s[0] == "badge-ai-generated")
        text = next(s for s in templates_gen.TEMPLATES if s[0] == "text-ai-generated")
        self.assertNotEqual(
            templates_gen.render_template(badge, font).tobytes(),
            templates_gen.render_template(text, font).tobytes(),
        )


class TestManifest(unittest.TestCase):
    def test_entry_schema(self) -> None:
        from pathlib import Path

        doc = templates_gen.template_entry(
            templates_gen.TEMPLATES[0], "badge-ai-generated.png", Path(__file__)
        )
        self.assertEqual(doc["kind"], "badge")
        self.assertTrue(doc["has_alpha"])
        self.assertEqual(doc["params"]["match_threshold"], 0.80)
        self.assertEqual(doc["params"]["scales"], [0.5, 0.75, 1.0, 1.5, 2.0])
        self.assertEqual(doc["params"]["text"], "AI GENERATED")
        json.dumps(doc)  # must be JSON-serializable


if __name__ == "__main__":
    unittest.main()
