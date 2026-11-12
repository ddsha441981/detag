#!/usr/bin/env python3
"""Deterministic golden-fixture generator (P0-093, E0.10).

Synthesizes the golden media suite from `05-QUALITY/01-test-strategy.md`:
  - 24 marked images: {png, jpeg, webp} x {512^2, 1920x1080, 3840x2160,
    8000x8000} x {badge mark, text mark} — plus 12 clean controls (the
    same scenes unmarked; they are the SSIM/PSNR reference the golden
    suite compares against, so they are NOT counted in the "24").
    The strategy text lists the mark set as {badge, text, clean} but
    labels the set "24 images": 3 formats x 4 sizes x 2 marks = 24
    marked inputs, 12 clean controls, 36 files total. See ISSUES.md
    (ASSUMPTION-013) and fixtures/README.md.
  - 3 videos (ffmpeg, testsrc2 source): 30 s 1080p h264 with badge +
    audio; 10 s 720p clean + audio; 4 s vertical 1080x1920 with badge
    + audio.
  - One expected-metrics envelope per fixture
    (`fixtures/expected/<stem>.json`): identity, seed, sha256, bytes
    and the quality-policy floors the engine's output must meet.
    `measured` is null until the Phase 2 golden suite fills it in.
  - A sha table (`fixtures/SHA256SUMS`, sha256sum format) over every
    generated file.

Determinism contract (AC: "gen twice = same shas"):
  - Fixed seeds. numpy's default_rng (PCG64) and random.Random
    (Mersenne Twister) are both stream-stable across versions.
  - Fixed encoder settings (Pillow quality/subsampling/method;
    libx264 `-threads 1`, pinned `creation_time` — ffmpeg's movenc
    otherwise stamps the current time into the MP4).
  - Same toolchain: Pillow/numpy/ffmpeg versions are recorded in
    every envelope and in the SHA256SUMS header. `--check` fails
    loudly when Pillow/numpy differ (they are pip-pinnable in CI);
    for videos, a differing ffmpeg version skips the byte compare
    and verifies stream properties via ffprobe instead (libx264
    output is encoder-version-dependent; see fixtures/README.md).

Usage:
  fixtures_gen.py [--out DIR]          generate into DIR (default fixtures/)
  fixtures_gen.py --check [--out DIR]  regenerate into a temp dir and
                                       compare against DIR: image bytes,
                                       video bytes (same ffmpeg) or
                                       properties (other ffmpeg),
                                       envelope sha256/bytes fields and
                                       SHA256SUMS. Exit 1 on any drift.
Exit 0 on success, 1 on failure. No network, no secrets, no spending.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

GENERATOR_VERSION = 1
BASE_SEED = 20261112  # fixed; scenes derive from it, never from time/random

# (stem, width, height) — the 4 sizes of the strategy matrix.
SIZES = [
    ("512", 512, 512),
    ("1080p", 1920, 1080),
    ("4k", 3840, 2160),
    ("8k", 8000, 8000),
]
FORMATS = ["png", "jpg", "webp"]  # png/jpeg/webp; jpg = JPEG
MARKS = ["clean", "badge", "text"]

# (stem, width, height, seconds, mark, audio_hz)
VIDEOS = [
    ("vid-1080p-30s-badge", 1920, 1080, 30, "badge", 440),
    ("vid-720p-10s-clean", 1280, 720, 10, "clean", 880),
    ("vid-vertical-4s-badge", 1080, 1920, 4, "badge", 440),
]

VIDEO_CRF = 23  # fixture SOURCE quality (x264 default; the CRF <= 16
# floor in the quality policy applies to the ENGINE's output, not
# the fixture — the envelope records both separately). testsrc2 is
# high-detail, so CRF 18 cost ~37 MB for the 30 s clip; CRF 23
# keeps the set at a sane clone weight with no loss of fixture value.
VIDEO_PRESET = "medium"
VIDEO_FPS = 30
# Pinned so the MP4 carries no wall-clock timestamp.
VIDEO_CREATION_TIME = "1970-01-01T00:00:00Z"

FONT_CANDIDATES = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
]

# Quality-policy floors (02-REQUIREMENTS/04-quality-policy.md). These
# are the gates the ENGINE's output must meet on the fixture — not the
# fixture's own encode settings (the fixture is a source, encoded at
# VIDEO_CRF / quality 90 below).
IMAGE_GATES = {
    "ssim_unmasked_min": 0.98,
    "psnr_unmasked_db_min": 38.0,
    "resolution": "identical",
    "format": "preserved",
    "png_untouched_pixel": "bit-exact",
    "jpeg_reencode_quality_min": 95,
    "jpeg_reencode_chroma": "4:4:4",
    "webp_reencode_quality_min": 90,
}
VIDEO_GATES = {
    "vmaf_min": 93,
    "ms_ssim_fallback_min": 0.97,
    "resolution_fps": "identical",
    "duration_within_frames": 1,
    "audio": "stream-copy bit-exact",
    "codec": "h264",
    "encode_crf_max": 16,
    "encode_preset": "slow",
    "pix_fmt": "yuv420p",
}


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def tool_versions() -> dict[str, str]:
    import PIL

    ffmpeg = "missing"
    try:
        out = subprocess.run(
            ["ffmpeg", "-version"], capture_output=True, text=True, check=True
        ).stdout.splitlines()[0]
        ffmpeg = out.replace("ffmpeg version", "").strip().split(" ")[0]
    except (subprocess.CalledProcessError, FileNotFoundError, IndexError):
        pass
    return {"pillow": PIL.__version__, "numpy": np.__version__, "ffmpeg": ffmpeg}


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
        "fixtures_gen: no usable TTF font found (looked for DejaVu/Liberation); "
        "install fonts-dejavu-core — refusing a silent fallback that would "
        "change the rendered marks"
    )


def base_seed(size_index: int) -> int:
    """One seed per size; all formats/marks of a size share the scene."""
    return BASE_SEED + size_index * 1000


def make_base(width: int, height: int, seed: int) -> Image.Image:
    """The unmarked scene: smooth ramps + sinusoid texture + seeded
    shapes + a small noise patch. Vectorized (no per-pixel Python) so
    the 8000x8000 base stays fast and under ~1 GB peak."""
    rnd = random.Random(seed)
    xx = np.linspace(0.0, 1.0, width, dtype=np.float32)[None, :]
    yy = np.linspace(0.0, 1.0, height, dtype=np.float32)[:, None]
    phase = rnd.uniform(0.0, 6.2831853)
    # 1-D ramps broadcast to the full frame (no HxW float temporaries
    # beyond one channel at a time).
    r = np.broadcast_to(xx * 200.0 + 30.0 + 18.0 * np.sin(xx * 40.0 + phase),
                        (height, width)).astype(np.uint8)
    g = np.broadcast_to(yy * 200.0 + 30.0 + 18.0 * np.sin(yy * 36.0 + phase * 2.0),
                        (height, width)).astype(np.uint8)
    b = (128.0 + 80.0 * np.sin((xx * 25.0 - yy * 25.0) + phase)).astype(np.uint8)
    arr = np.stack([r, g, b], axis=2)
    del r, g, b

    # Small high-frequency patch (512x512 corner): gives the metric
    # implementations real texture to chew on without inflating the
    # 8K PNG (noise over the whole frame would cost tens of MB).
    patch = min(512, width // 2, height // 2)
    noise = np.random.default_rng(seed + 7).integers(
        0, 256, size=(patch, patch, 3), dtype=np.uint8
    )
    region = arr[:patch, :patch, :].astype(np.int16)
    region = (region * 6 + noise.astype(np.int16) * 4) // 10
    arr[:patch, :patch, :] = np.clip(region, 0, 255).astype(np.uint8)
    del noise, region

    img = Image.fromarray(arr, mode="RGB")
    draw = ImageDraw.Draw(img)
    # Seeded solid shapes: rectangles, ellipses, a diagonal band.
    for _ in range(10):
        x0 = rnd.randint(0, width - 1)
        y0 = rnd.randint(0, height - 1)
        x1 = min(width, x0 + rnd.randint(width // 16, width // 3))
        y1 = min(height, y0 + rnd.randint(height // 16, height // 3))
        color = (rnd.randint(0, 255), rnd.randint(0, 255), rnd.randint(0, 255))
        if rnd.random() < 0.5:
            draw.rectangle([x0, y0, x1, y1], fill=color)
        else:
            draw.ellipse([x0, y0, x1, y1], fill=color)
    # Thin grid lines for edge content. Sparse at large sizes: a
    # line every 250 px at 8K is mostly what made the 8K JPEGs 5 MB.
    step = max(64, width // 16)
    for gx in range(0, width, step):
        draw.line([(gx, 0), (gx, height)], fill=(255, 255, 255), width=1)
    for gy in range(0, height, step):
        draw.line([(0, gy), (width, gy)], fill=(0, 0, 0), width=1)
    return img


def apply_badge(img: Image.Image, font_path: str) -> Image.Image:
    """Translucent white badge, bottom-right, with DETAG text."""
    width, height = img.size
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    bw, bh = width // 6, height // 12
    bx, by = width - bw - width // 40, height - bh - height // 40
    draw.rounded_rectangle([bx, by, bx + bw, by + bh], radius=bh // 5, fill=(255, 255, 255, 170))
    draw.ellipse([bx + bw // 12, by + bh // 4, bx + bw // 4, by + 3 * bh // 4], fill=(30, 30, 30, 200))
    font = ImageFont.truetype(font_path, size=max(12, bh // 2))
    draw.text((bx + bw // 3, by + bh // 4), "DETAG", fill=(30, 30, 30, 220), font=font)
    return Image.alpha_composite(img.convert("RGBA"), overlay).convert("RGB")


def apply_text_mark(img: Image.Image, font_path: str) -> Image.Image:
    """Diagonal translucent SAMPLE text band across the centre."""
    width, height = img.size
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    font = ImageFont.truetype(font_path, size=max(12, height // 16))
    text = "SAMPLE  •  DETAG  •  SAMPLE  •  DETAG"
    # Tile the text on a rotated layer so the band covers the frame.
    layer = Image.new("RGBA", (width * 2, height // 3), (0, 0, 0, 0))
    layer_draw = ImageDraw.Draw(layer)
    tw = layer_draw.textlength(text, font=font)
    x = 0
    while x < width * 2:
        layer_draw.text((x, height // 12), text, fill=(20, 20, 20, 110), font=font)
        x += int(tw) + 40
    rotated = layer.rotate(-25, expand=True, resample=Image.BICUBIC)
    overlay.paste(rotated, (-width // 4, height // 3), rotated)
    return Image.alpha_composite(img.convert("RGBA"), overlay).convert("RGB")


def save_image(img: Image.Image, path: Path, fmt: str) -> None:
    if fmt == "png":
        img.save(path, format="PNG", compress_level=6)
    elif fmt == "jpg":
        img.save(path, format="JPEG", quality=90, subsampling=0)  # 4:4:4
    elif fmt == "webp":
        img.save(path, format="WEBP", quality=90, method=6)
    else:  # pragma: no cover - guarded by FORMATS
        raise ValueError(f"unknown format {fmt}")


def image_stem(size_stem: str, fmt: str, mark: str) -> str:
    return f"img-{size_stem}-{fmt}-{mark}"


def ext_for(fmt: str) -> str:
    return {"png": "png", "jpg": "jpg", "webp": "webp"}[fmt]


def envelope(
    stem: str,
    kind: str,
    versions: dict[str, str],
    seed: int,
    extra: dict,
) -> dict:
    doc: dict = {
        "fixture": stem,
        "kind": kind,
        "generator": "scripts/fixtures_gen.py",
        "generator_version": GENERATOR_VERSION,
        "seed": seed,
        "source": "synthesized",
        "tool_versions": versions,
        "gates": IMAGE_GATES if kind == "image" else VIDEO_GATES,
        # Filled by the Phase 2 golden suite (crates/engine/tests/golden/):
        # measured metrics of the engine's output on this fixture.
        "measured": None,
    }
    doc.update(extra)
    return doc


def write_envelope(path: Path, doc: dict) -> None:
    path.write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def generate_images(out: Path, versions: dict[str, str], font_path: str) -> list[dict]:
    """Returns the envelope docs (sha/bytes filled after writing)."""
    docs: list[dict] = []
    images_dir = out / "images"
    images_dir.mkdir(parents=True, exist_ok=True)
    for size_index, (size_stem, width, height) in enumerate(SIZES):
        seed = base_seed(size_index)
        base = make_base(width, height, seed)
        for fmt in FORMATS:
            for mark in MARKS:
                stem = image_stem(size_stem, fmt, mark)
                img = base
                if mark == "badge":
                    img = apply_badge(base, font_path)
                elif mark == "text":
                    img = apply_text_mark(base, font_path)
                rel = Path("images") / f"{stem}.{ext_for(fmt)}"
                save_image(img, out / rel, fmt)
                docs.append(
                    (
                        stem,
                        "image",
                        seed,
                        {
                            "file": str(rel),
                            "format": fmt,
                            "width": width,
                            "height": height,
                            "mark": mark,
                            "sha256": sha256_file(out / rel),
                            "bytes": (out / rel).stat().st_size,
                        },
                    )
                )
    return [envelope(stem, kind, versions, seed, extra) for stem, kind, seed, extra in docs]


def generate_videos(out: Path, versions: dict[str, str], font_path: str) -> list[dict]:
    docs: list[dict] = []
    videos_dir = out / "videos"
    videos_dir.mkdir(parents=True, exist_ok=True)
    for index, (stem, width, height, seconds, mark, audio_hz) in enumerate(VIDEOS):
        seed = BASE_SEED + 5000 + index * 1000
        rel = Path("videos") / f"{stem}.mp4"
        target = out / rel
        filt = [
            f"testsrc2=size={width}x{height}:rate={VIDEO_FPS}:duration={seconds}",
            "format=yuv420p",
        ]
        filters = ",".join(filt)
        if mark == "badge":
            bw, bh = width // 6, height // 12
            filters += (
                f",drawbox=x=iw-{bw}-iw/40:y=ih-{bh}-ih/40:w={bw}:h={bh}:color=white@0.66:t=fill"
                f",drawtext=fontfile={font_path}:text='DETAG':fontsize={max(12, bh // 2)}"
                f":fontcolor=black:x=w-tw-{bw // 3}:y=h-th-{bh // 4}"
            )
        cmd = [
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", filters,
            "-f", "lavfi", "-i", f"sine=frequency={audio_hz}:sample_rate=48000:duration={seconds}",
            "-map", "0:v", "-map", "1:a",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            "-crf", str(VIDEO_CRF), "-preset", VIDEO_PRESET,
            "-threads", "1", "-r", str(VIDEO_FPS),
            "-c:a", "aac", "-b:a", "128k",
            "-metadata", f"creation_time={VIDEO_CREATION_TIME}",
            "-movflags", "+faststart",
            str(target),
        ]
        subprocess.run(cmd, check=True)
        docs.append(
            envelope(
                stem,
                "video",
                versions,
                seed,
                {
                    "file": str(rel),
                    "format": "mp4",
                    "codec": "h264",
                    "width": width,
                    "height": height,
                    "duration_s": seconds,
                    "fps": VIDEO_FPS,
                    "mark": mark,
                    "audio_hz": audio_hz,
                    "encode": {
                        "crf": VIDEO_CRF,
                        "preset": VIDEO_PRESET,
                        "pix_fmt": "yuv420p",
                        "threads": 1,
                    },
                    "sha256": sha256_file(target),
                    "bytes": target.stat().st_size,
                },
            )
        )
    return docs


def write_all_envelopes(out: Path, docs: list[dict]) -> None:
    expected_dir = out / "expected"
    expected_dir.mkdir(parents=True, exist_ok=True)
    for doc in docs:
        stem = doc["fixture"]
        write_envelope(expected_dir / f"{stem}.json", doc)


def write_sha_table(out: Path, versions: dict[str, str]) -> None:
    lines = [
        "# SHA256SUMS — generated by scripts/fixtures_gen.py (P0-093).",
        "# Regenerate with `just fixtures-gen`; verify with `just fixtures-check`.",
        f"# generator_version={GENERATOR_VERSION} "
        f"pillow={versions['pillow']} numpy={versions['numpy']} "
        f"ffmpeg={versions['ffmpeg']}",
    ]
    files: list[Path] = []
    for sub in ("images", "videos", "expected"):
        files.extend(sorted((out / sub).glob("*")))
    for path in files:
        rel = path.relative_to(out)
        lines.append(f"{sha256_file(path)}  {rel}")
    (out / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")


def generate(out: Path) -> list[dict]:
    versions = tool_versions()
    if versions["ffmpeg"] == "missing":
        raise SystemExit("fixtures_gen: ffmpeg not found on PATH — videos need it")
    font_path = find_font()
    out.mkdir(parents=True, exist_ok=True)
    docs = generate_images(out, versions, font_path)
    docs += generate_videos(out, versions, font_path)
    write_all_envelopes(out, docs)
    write_sha_table(out, versions)
    return docs


def ffprobe_props(path: Path) -> dict:
    out = subprocess.run(
        [
            "ffprobe", "-v", "error",
            "-show_entries", "stream=codec_type,codec_name,width,height,r_frame_rate",
            "-show_entries", "format=duration",
            "-of", "json", str(path),
        ],
        capture_output=True, text=True, check=True,
    ).stdout
    data = json.loads(out)
    video = next(s for s in data["streams"] if s["codec_type"] == "video")
    num, den = (int(p) for p in video["r_frame_rate"].split("/"))
    return {
        "codec": video["codec_name"],
        "width": int(video["width"]),
        "height": int(video["height"]),
        "fps": num // den if den else num,
        "duration_s": float(data["format"]["duration"]),
        "has_audio": any(s["codec_type"] == "audio" for s in data["streams"]),
    }


def check(out: Path) -> int:
    """Regenerate into a temp dir; compare against `out`. Returns 0/1."""
    versions = tool_versions()
    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="fixtures-check-") as tmp:
        fresh = Path(tmp) / "fixtures"
        try:
            fresh_docs = generate(fresh)
        except subprocess.CalledProcessError as exc:
            print(f"fixtures-check: regeneration failed: {exc}", file=sys.stderr)
            return 1
        # 1. Toolchain: Pillow/numpy must match the COMMITTED
        #    envelopes (pip-pinnable); ffmpeg may differ — video
        #    bytes are encoder-version-dependent.
        committed_envs = sorted((out / "expected").glob("*.json"))
        if not committed_envs:
            print("fixtures-check: no committed envelopes in expected/", file=sys.stderr)
            return 1
        recorded = json.loads(committed_envs[0].read_text(encoding="utf-8"))["tool_versions"]
        committed_table = (out / "SHA256SUMS").read_text(encoding="utf-8")
        for key in ("pillow", "numpy"):
            if key in recorded and versions[key] != recorded[key]:
                failures.append(
                    f"toolchain drift: {key} is {versions[key]}, fixtures were "
                    f"generated with {recorded[key]} — run `just fixtures-gen` "
                    f"with the pinned versions and commit the new shas"
                )
        ffmpeg_matches = versions["ffmpeg"] == recorded["ffmpeg"]
        if not ffmpeg_matches:
            print(
                f"fixtures-check: ffmpeg is {versions['ffmpeg']}, fixtures were "
                f"generated with {recorded['ffmpeg']} — video bytes are "
                f"encoder-version-dependent, so video shas are skipped and "
                f"stream properties are verified instead",
                file=sys.stderr,
            )
        # 2. Byte compare (images always; videos only on ffmpeg match).
        for doc in fresh_docs:
            rel = Path(doc["file"])
            fresh_path, committed_path = fresh / rel, out / rel
            if not committed_path.exists():
                failures.append(f"missing committed file: {rel}")
                continue
            if doc["kind"] == "video" and not ffmpeg_matches:
                props = ffprobe_props(committed_path)
                want = {
                    "codec": doc["codec"],
                    "width": doc["width"],
                    "height": doc["height"],
                    "fps": doc["fps"],
                }
                for key, value in want.items():
                    if props[key] != value:
                        failures.append(f"video {rel}: {key} {props[key]} != {value}")
                if abs(props["duration_s"] - doc["duration_s"]) > 0.1:
                    failures.append(
                        f"video {rel}: duration {props['duration_s']} != {doc['duration_s']}"
                    )
                if not props["has_audio"]:
                    failures.append(f"video {rel}: audio stream missing")
                continue
            if sha256_file(fresh_path) != sha256_file(committed_path):
                failures.append(f"sha drift: {rel} — run `just fixtures-gen` and commit")
        # 3. Envelopes: sha256/bytes fields must match the committed files.
        for doc in fresh_docs:
            env_path = out / "expected" / f"{doc['fixture']}.json"
            if not env_path.exists():
                failures.append(f"missing envelope: {doc['fixture']}.json")
                continue
            committed_env = json.loads(env_path.read_text(encoding="utf-8"))
            rel = Path(doc["file"])
            if committed_env.get("sha256") != sha256_file(out / rel):
                failures.append(f"envelope sha256 stale: {doc['fixture']}.json")
            if committed_env.get("bytes") != (out / rel).stat().st_size:
                failures.append(f"envelope bytes stale: {doc['fixture']}.json")
        # 4. SHA256SUMS must match the committed files.
        for line in committed_table.splitlines():
            if line.startswith("#") or not line.strip():
                continue
            want_sha, _, rel = line.partition("  ")
            path = out / rel.strip()
            if not path.exists():
                failures.append(f"SHA256SUMS lists missing file: {rel.strip()}")
            elif sha256_file(path) != want_sha.strip():
                failures.append(f"SHA256SUMS drift: {rel.strip()}")
    if failures:
        print("fixtures-check: FAILED", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("fixtures-check: OK — committed fixtures match the generator")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=None,
                        help="fixtures dir (default: <repo>/fixtures)")
    parser.add_argument("--check", action="store_true",
                        help="regenerate to temp and compare; do not write")
    args = parser.parse_args(argv)
    out = args.out or (repo_root() / "fixtures")
    if args.check:
        return check(out)
    docs = generate(out)
    total_bytes = sum(doc["bytes"] for doc in docs)
    print(
        f"fixtures-gen: wrote {len(docs)} fixtures "
        f"({sum(1 for d in docs if d['kind'] == 'image')} images, "
        f"{sum(1 for d in docs if d['kind'] == 'video')} videos) "
        f"to {out} ({total_bytes / 1e6:.1f} MB total)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
