#!/usr/bin/env python3
"""Image size gate (P0-083, NFR-63): fail if a built image is too big.

Reads the image size with `docker image inspect --format={{.Size}}`
(the uncompressed on-disk size) and fails when it reaches the limit.
The uncompressed size is always >= the compressed size, so passing
this gate guarantees NFR-63's "API images < 150 MB compressed" limit.

Usage:
    check_image_size.py --image detag-api:check [--max-mb 150]

The image itself is built by the caller (`just image-size-check` runs
`docker build` first); this script only inspects. It exits 2 when
docker is missing or the image does not exist, so a broken gate is
loud instead of silently green.
"""

import argparse
import json
import subprocess
import sys

DEFAULT_MAX_MB = 150
MB = 1024 * 1024


def size_ok(size_bytes: int, max_mb: int) -> bool:
    """True when the image fits under the limit (strictly smaller)."""
    return size_bytes < max_mb * MB


def parse_inspect_size(output: str) -> int:
    """Parse `docker image inspect` output.

    Accepts the raw `{{.Size}}` integer, or a JSON array with one
    object carrying a `Size` field (what `docker image inspect`
    prints without --format).
    """
    text = output.strip()
    if text.startswith("["):
        data = json.loads(text)
        if not isinstance(data, list) or len(data) != 1 or "Size" not in data[0]:
            raise ValueError(f"unexpected inspect output: {text[:200]}")
        return int(data[0]["Size"])
    return int(text)


def inspect_size(image: str) -> int:
    """Return the image size in bytes via `docker image inspect`."""
    try:
        result = subprocess.run(
            ["docker", "image", "inspect", "--format", "{{.Size}}", image],
            capture_output=True,
            text=True,
            check=True,
        )
    except FileNotFoundError:
        print("check_image_size: docker not found", file=sys.stderr)
        sys.exit(2)
    except subprocess.CalledProcessError as exc:
        print(
            f"check_image_size: cannot inspect image {image!r}: "
            f"{exc.stderr.strip()}",
            file=sys.stderr,
        )
        sys.exit(2)
    try:
        return parse_inspect_size(result.stdout)
    except (ValueError, TypeError) as exc:
        print(f"check_image_size: {exc}", file=sys.stderr)
        sys.exit(2)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, help="image to check, e.g. detag-api:check")
    parser.add_argument("--max-mb", type=int, default=DEFAULT_MAX_MB,
                        help=f"size limit in MB (default {DEFAULT_MAX_MB})")
    args = parser.parse_args(argv)
    if args.max_mb <= 0:
        parser.error("--max-mb must be >= 1")

    size = inspect_size(args.image)
    size_mb = size / MB
    if size_ok(size, args.max_mb):
        print(f"image-size OK: {args.image} is {size_mb:.1f} MB (< {args.max_mb} MB)")
        return 0
    print(
        f"image-size FAIL: {args.image} is {size_mb:.1f} MB (>= {args.max_mb} MB limit, NFR-63)",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
