#!/usr/bin/env python3
"""Deterministic ICC fixtures for P2-004 (engine/intake, FR-D-07).

Generates, never hand-edits:
  - Three ICC profiles, built with LittleCMS 2 (liblcms2) via ctypes:
      srgb.icc       — cmsCreate_sRGBProfile() (canonical sRGB, 588 bytes)
      display-p3.icc — cmsCreateRGBProfileTHR, Display P3 primaries
                       (0.68,0.32 / 0.265,0.69 / 0.15,0.06), D65 white,
                       sRGB parametric TRC (type 4) — a genuine Display P3
                       matrix-shaper profile, desc "Display P3"
      test-rgb.icc   — same builder, invented primaries (0.68,0.30 /
                       0.22,0.68 / 0.12,0.08), gamma 2.2, desc "TestRGB"
                       (the "unknown profile" — extraction must pass it
                       through verbatim, never validate or replace it)
  - Pillow images embedding those profiles (16x16 gradient, fixed
    encoder settings): PNG/JPEG/WebP/TIFF with sRGB, P3, TestRGB, and
    no-ICC controls.
  - Two corrupt-ICC images: png-corrupt-icc.png (one flipped byte in
    the iCCP zlib stream, CRC repaired — the container is valid, the
    profile data is not) and jpeg-corrupt-icc.jpg (APP2 chunk-count
    byte 1 -> 2, so the chunk set disagrees with itself). Both must
    extract as ABSENT (the decoders' lenient behaviour — ADR-0073),
    never panic, never fail the decode.
  - SHA256SUMS over every generated file (sha256sum format).

Verification: every image is re-opened with Pillow and its
`icc_profile` is asserted equal to the profile bytes (or None for the
no-ICC / corrupt cases) — the fixture's "known" bytes come from lcms2,
the container bytes from Pillow, two independent writers.

Toolchain: Pillow 10.2.0, liblcms2.so.2 (2.0.14). Re-run to regenerate:
`python3 crates/engine/tests/fixtures/icc/gen_icc_fixtures.py`.
"""
import ctypes
import hashlib
import struct
import sys
import zlib
from ctypes import (
    POINTER, Structure, byref, c_char_p, c_double, c_float, c_int,
    c_uint32, c_void_p, create_string_buffer,
)
from pathlib import Path

from PIL import Image

OUT = Path(__file__).resolve().parent

# ---------------------------------------------------------------- lcms2
lcms = ctypes.CDLL("liblcms2.so.2")


class CIExyY(Structure):
    _fields_ = [("x", c_double), ("y", c_double), ("Y", c_double)]


class CIExyYTriple(Structure):
    _fields_ = [("Red", CIExyY), ("Green", CIExyY), ("Blue", CIExyY)]


lcms.cmsCreate_sRGBProfile.restype = c_void_p
lcms.cmsBuildGamma.restype = c_void_p
lcms.cmsBuildGamma.argtypes = [c_void_p, c_double]
lcms.cmsBuildParametricToneCurve.restype = c_void_p
lcms.cmsBuildParametricToneCurve.argtypes = [c_void_p, c_int, POINTER(c_double)]
lcms.cmsCreateRGBProfileTHR.restype = c_void_p
lcms.cmsCreateRGBProfileTHR.argtypes = [
    c_void_p, POINTER(CIExyY), POINTER(CIExyYTriple), POINTER(c_void_p),
]
lcms.cmsSaveProfileToMem.restype = c_int
lcms.cmsSaveProfileToMem.argtypes = [c_void_p, c_void_p, POINTER(c_uint32)]
lcms.cmsOpenProfileFromMem.restype = c_void_p
lcms.cmsOpenProfileFromMem.argtypes = [c_void_p, c_uint32]
lcms.cmsGetColorSpace.restype = c_uint32
lcms.cmsGetColorSpace.argtypes = [c_void_p]
lcms.cmsCloseProfile.restype = c_int
lcms.cmsMLUalloc.restype = c_void_p
lcms.cmsMLUalloc.argtypes = [c_void_p, c_uint32]
lcms.cmsMLUsetASCII.restype = c_int
lcms.cmsMLUsetASCII.argtypes = [c_void_p, c_char_p, c_char_p, c_char_p]
lcms.cmsWriteTag.restype = c_int
lcms.cmsWriteTag.argtypes = [c_void_p, c_uint32, c_void_p]
lcms.cmsEvalToneCurveFloat.restype = c_float
lcms.cmsEvalToneCurveFloat.argtypes = [c_void_p, c_float]


def _determinize(blob: bytes) -> bytes:
    """Pin the ICC header so regeneration is byte-identical.

    lcms2 stamps the current time into the profile header (bytes 24..36)
    — it changes every run. Pin the date to a fixed valid value; the
    profile ID (bytes 84..100) is already zero in lcms2 output (the ICC
    spec's explicit "ID not calculated" value), so it needs no patch.
    The profile stays a valid lcms2-built profile, and the bytes are
    now stable across runs.
    """
    b = bytearray(blob)
    assert b[36:40] == b"acsp"
    b[24:36] = struct.pack(">6H", 2026, 11, 26, 0, 0, 0)
    return bytes(b)


def _save(h) -> bytes:
    size = c_uint32(0)
    assert lcms.cmsSaveProfileToMem(h, None, byref(size)) == 1
    buf = create_string_buffer(size.value)
    assert lcms.cmsSaveProfileToMem(h, buf, byref(size)) == 1
    return _determinize(buf.raw[: size.value])


def _set_desc(h, text: str) -> None:
    mlu = lcms.cmsMLUalloc(None, 1)
    assert lcms.cmsMLUsetASCII(mlu, b"en", b"US", text.encode()) == 1
    assert lcms.cmsWriteTag(h, 0x64657363, mlu) == 1  # 'desc'


def _srgb_trc():
    # sRGB EOTF as a type-4 parametric curve: (aX+b)^Gamma + e for X >= d,
    # cX below. Verified: evals to the sRGB formula within 1e-6.
    params = (c_double * 6)(2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045, 0.0)
    curve = lcms.cmsBuildParametricToneCurve(None, 4, params)
    for x, exp in ((0.5, ((0.5 + 0.055) / 1.055) ** 2.4), (1.0, 1.0)):
        assert abs(lcms.cmsEvalToneCurveFloat(curve, x) - exp) < 1e-6
    return curve


def build_profiles() -> dict[str, bytes]:
    out = {}
    h = lcms.cmsCreate_sRGBProfile()
    out["srgb.icc"] = _save(h)
    assert out["srgb.icc"][36:40] == b"acsp"
    lcms.cmsCloseProfile(h)

    d65 = CIExyY(0.3127, 0.3290, 1.0)
    trc = _srgb_trc()
    curves = (c_void_p * 3)(trc, trc, trc)
    p3 = CIExyYTriple(CIExyY(0.68, 0.32, 1.0), CIExyY(0.265, 0.69, 1.0), CIExyY(0.15, 0.06, 1.0))
    h = lcms.cmsCreateRGBProfileTHR(None, byref(d65), byref(p3), curves)
    assert h, "P3 profile creation failed"
    _set_desc(h, "Display P3")
    out["display-p3.icc"] = _save(h)
    lcms.cmsCloseProfile(h)

    gamma22 = lcms.cmsBuildGamma(None, 2.2)
    assert gamma22, "gamma 2.2 curve failed"
    curves22 = (c_void_p * 3)(gamma22, gamma22, gamma22)
    odd = CIExyYTriple(CIExyY(0.68, 0.30, 1.0), CIExyY(0.22, 0.68, 1.0), CIExyY(0.12, 0.08, 1.0))
    h = lcms.cmsCreateRGBProfileTHR(None, byref(d65), byref(odd), curves22)
    assert h, "TestRGB profile creation failed"
    _set_desc(h, "TestRGB")
    out["test-rgb.icc"] = _save(h)
    lcms.cmsCloseProfile(h)
    return out


# ---------------------------------------------------------------- images
def gradient() -> Image.Image:
    im = Image.new("RGB", (16, 16))
    px = im.load()
    for y in range(16):
        for x in range(16):
            px[x, y] = ((x * 16) % 256, (y * 16) % 256, ((x + y) * 8) % 256)
    return im


def corrupt_png_iccp(data: bytes) -> bytes:
    """Flip one byte inside the iCCP chunk's zlib stream, repair the CRC.

    The container stays valid; only the profile data is corrupt.
    """
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    out = bytearray(data[:8])
    pos = 8
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        ctype = data[pos + 4 : pos + 8]
        chunk = data[pos + 8 : pos + 8 + length]
        if ctype == b"iCCP":
            nul = chunk.index(b"\x00")
            # name + NUL + compression-method byte, then the zlib stream.
            assert chunk[nul + 1] == 0
            body = bytearray(chunk)
            body[nul + 2] ^= 0xFF
            chunk = bytes(body)
        crc = zlib.crc32(ctype + chunk) & 0xFFFFFFFF
        out += struct.pack(">I", length) + ctype + chunk + struct.pack(">I", crc)
        pos += 12 + length
    return bytes(out)


def corrupt_jpeg_app2(data: bytes) -> bytes:
    """Change the APP2 ICC chunk-count byte from 1 to 2.

    One APP2 segment exists, but it now claims to be 1 of 2 — the chunk
    set disagrees with itself, so the decoder must report no ICC.
    """
    idx = data.find(b"ICC_PROFILE\x00")
    assert idx > 0, "no ICC_PROFILE marker found"
    count_off = idx + len(b"ICC_PROFILE\x00") + 1  # chunk_no, then count
    assert data[count_off] == 1, f"expected count 1, got {data[count_off]}"
    out = bytearray(data)
    out[count_off] = 2
    return bytes(out)


def main() -> int:
    profiles = build_profiles()
    for name, blob in profiles.items():
        (OUT / name).write_bytes(blob)
        # The determinized bytes must still open as a valid RGB profile.
        h = lcms.cmsOpenProfileFromMem(blob, len(blob))
        assert h, f"{name}: lcms2 cannot reopen determinized profile"
        assert lcms.cmsGetColorSpace(h) == 0x52474220, f"{name}: not RGB"
        lcms.cmsCloseProfile(h)
        print(f"{name}: {len(blob)} bytes sha256={hashlib.sha256(blob).hexdigest()[:16]}…")

    im = gradient()
    jobs = [
        ("png-srgb.png", "PNG", {"icc_profile": profiles["srgb.icc"]}, "srgb.icc"),
        ("png-p3.png", "PNG", {"icc_profile": profiles["display-p3.icc"]}, "display-p3.icc"),
        ("png-test-rgb.png", "PNG", {"icc_profile": profiles["test-rgb.icc"]}, "test-rgb.icc"),
        ("png-no-icc.png", "PNG", {}, None),
        ("jpeg-srgb.jpg", "JPEG", {"quality": 95, "subsampling": 0, "icc_profile": profiles["srgb.icc"]}, "srgb.icc"),
        ("jpeg-p3.jpg", "JPEG", {"quality": 95, "subsampling": 0, "icc_profile": profiles["display-p3.icc"]}, "display-p3.icc"),
        ("jpeg-no-icc.jpg", "JPEG", {"quality": 95, "subsampling": 0}, None),
        ("webp-srgb.webp", "WEBP", {"lossless": True, "icc_profile": profiles["srgb.icc"]}, "srgb.icc"),
        ("webp-no-icc.webp", "WEBP", {"lossless": True}, None),
        ("tiff-srgb.tiff", "TIFF", {"icc_profile": profiles["srgb.icc"]}, "srgb.icc"),
        ("tiff-no-icc.tiff", "TIFF", {}, None),
    ]
    for name, fmt, kw, want in jobs:
        im.save(OUT / name, fmt, **kw)
        got = Image.open(OUT / name).info.get("icc_profile")
        exp = profiles[want] if want else None
        assert got == exp, f"{name}: Pillow read-back mismatch (got {None if got is None else len(got)}, want {None if exp is None else len(exp)})"
        print(f"{name}: Pillow read-back OK ({'no ICC' if got is None else f'{len(got)} bytes'})")

    # Corrupt cases (derived from the verified-good files above).
    png_bad = corrupt_png_iccp((OUT / "png-srgb.png").read_bytes())
    (OUT / "png-corrupt-icc.png").write_bytes(png_bad)
    got = Image.open(OUT / "png-corrupt-icc.png").info.get("icc_profile")
    print(f"png-corrupt-icc.png: Pillow reads icc_profile={got!r} (want None)")
    assert got is None

    jpg_bad = corrupt_jpeg_app2((OUT / "jpeg-srgb.jpg").read_bytes())
    (OUT / "jpeg-corrupt-icc.jpg").write_bytes(jpg_bad)
    got = Image.open(OUT / "jpeg-corrupt-icc.jpg").info.get("icc_profile")
    print(f"jpeg-corrupt-icc.jpg: Pillow reads icc_profile={got!r} (want None)")
    assert got is None

    lines = []
    for p in sorted(OUT.iterdir()):
        if p.name in ("gen_icc_fixtures.py", "SHA256SUMS"):
            continue
        lines.append(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n")
    (OUT / "SHA256SUMS").write_text("".join(lines))
    print(f"SHA256SUMS: {len(lines)} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
