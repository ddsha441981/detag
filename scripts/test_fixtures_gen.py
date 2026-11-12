#!/usr/bin/env python3
"""Unit tests for fixtures_gen.py (P0-093).

Only the pure/deterministic helpers — the full generation (ffmpeg,
8K images) is exercised by `just fixtures-gen` + `just fixtures-check`.
Run: python3 -m unittest discover -s scripts -p "test_*.py"
"""

from __future__ import annotations

import hashlib
import io
import json
import unittest
from pathlib import Path

import fixtures_gen


class TestSeedsAndStems(unittest.TestCase):
    def test_base_seed_derivation_is_stable(self) -> None:
        self.assertEqual(fixtures_gen.base_seed(0), 20261112)
        self.assertEqual(fixtures_gen.base_seed(3), 20261112 + 3000)
        self.assertEqual(len({fixtures_gen.base_seed(i) for i in range(4)}), 4)

    def test_image_stems_cover_the_matrix(self) -> None:
        stems = {
            fixtures_gen.image_stem(size, fmt, mark)
            for size, _, _ in fixtures_gen.SIZES
            for fmt in fixtures_gen.FORMATS
            for mark in fixtures_gen.MARKS
        }
        self.assertEqual(len(stems), 36)  # 12 bases x 3 variants
        marked = {s for s in stems if s.endswith(("-badge", "-text"))}
        self.assertEqual(len(marked), 24)  # the "24 images matrix" of the ticket
        clean = {s for s in stems if s.endswith("-clean")}
        self.assertEqual(len(clean), 12)  # clean controls (SSIM/PSNR reference)


class TestDeterminism(unittest.TestCase):
    def test_make_base_is_deterministic_at_small_size(self) -> None:
        a = fixtures_gen.make_base(64, 48, 1234)
        b = fixtures_gen.make_base(64, 48, 1234)
        self.assertEqual(a.tobytes(), b.tobytes())
        c = fixtures_gen.make_base(64, 48, 1235)
        self.assertNotEqual(a.tobytes(), c.tobytes())  # different seed, different scene

    def test_marks_change_pixels_but_keep_size(self) -> None:
        base = fixtures_gen.make_base(128, 96, 42)
        font = fixtures_gen.find_font()
        for marked in (
            fixtures_gen.apply_badge(base, font),
            fixtures_gen.apply_text_mark(base, font),
        ):
            self.assertEqual(marked.size, base.size)
            self.assertNotEqual(marked.tobytes(), base.tobytes())

    def test_save_image_roundtrip_is_deterministic(self) -> None:
        base = fixtures_gen.make_base(64, 48, 99)
        for fmt in ("png", "jpg", "webp"):
            first = io.BytesIO()
            second = io.BytesIO()
            fixtures_gen.save_image(base, first, fmt)
            fixtures_gen.save_image(base, second, fmt)
            self.assertEqual(
                hashlib.sha256(first.getvalue()).hexdigest(),
                hashlib.sha256(second.getvalue()).hexdigest(),
            )


class TestEnvelopes(unittest.TestCase):
    def test_image_envelope_schema(self) -> None:
        doc = fixtures_gen.envelope(
            "img-512-png-badge",
            "image",
            {"pillow": "10.2.0", "numpy": "1.26.4", "ffmpeg": "8.1.2"},
            20261112,
            {
                "file": "images/img-512-png-badge.png",
                "format": "png",
                "width": 512,
                "height": 512,
                "mark": "badge",
                "sha256": "x" * 64,
                "bytes": 1,
            },
        )
        self.assertEqual(doc["generator_version"], fixtures_gen.GENERATOR_VERSION)
        self.assertEqual(doc["source"], "synthesized")
        self.assertEqual(doc["gates"]["ssim_unmasked_min"], 0.98)
        self.assertEqual(doc["gates"]["psnr_unmasked_db_min"], 38.0)
        self.assertIsNone(doc["measured"])
        json.dumps(doc)  # must be JSON-serializable

    def test_video_envelope_gates(self) -> None:
        doc = fixtures_gen.envelope(
            "vid-1080p-30s-badge",
            "video",
            {"pillow": "10.2.0", "numpy": "1.26.4", "ffmpeg": "8.1.2"},
            20266112,
            {"file": "videos/vid-1080p-30s-badge.mp4"},
        )
        self.assertEqual(doc["gates"]["vmaf_min"], 93)
        self.assertEqual(doc["gates"]["codec"], "h264")


if __name__ == "__main__":
    unittest.main()
