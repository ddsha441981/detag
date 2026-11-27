#!/usr/bin/env python3
"""Generate decompression-bomb fixtures for P2-006 (dimension/size guards).

A decompression bomb is tiny on disk and enormous decoded: a solid-colour
image compresses to almost nothing, so the file claims huge dimensions
while staying kilobytes. The guard (detag_engine::guards) must reject
these from the HEADER alone, before any full decode allocates the pixels.

Fixtures written (all deterministic — fixed zlib level, no timestamps):
  bomb-png-20000x20000.png   real bomb: valid PNG, IHDR 20000x20000 RGB,
                             IDAT = zlib-compressed zero scanlines streamed
                             (never holds the ~1.2 GB decoded image in RAM).
                             ~1.4 MB on disk, 1.2 GB decoded.
  bomb-png-8001x8001.png     one pixel over the 8000 side cap, same build.
  bomb-png-header-only-20000x20000.png
                             signature + IHDR + an IDAT carrying an empty
                             zlib stream + IEND: no pixel data at all.
                             into_dimensions() still reads the dims — the
                             png crate's read_info() REQUIRES an IDAT
                             chunk to begin (read_until_image_data), so a
                             bare IHDR+IEND file fails as unreadable, not
                             as DimensionsExceeded (corrected 2026-11-27
                             by the 215th — the 214th's bare-header claim
                             was wrong; see guards.rs). The guard rejects
                             it while decode_image() on the same bytes
                             fails with an Image error. That split proves
                             the verdict came from the header, not from a
                             decode attempt.
  bomb-jpeg-20000x20000.jpg  SOI + APP0 (JFIF) + SOF0 claiming 20000x20000
                             + SOS + EOI, no scan data. zune-jpeg's
                             decode_headers() loops over markers until
                             SOS, so a file ending at SOF0 fails as
                             unreadable (corrected 2026-11-27 by the
                             215th); parse_sos() needs no DQT/DHT tables.
  bomb-webp-20000x20000.webp RIFF/WEBP/VP8X chunk claiming 20000x20000 +
                             a minimal VP8L chunk (signature + dims
                             header, 5 bytes). image-webp requires
                             exactly one of VP8/VP8L present after VP8X
                             (corrected 2026-11-27 by the 215th); the
                             VP8L payload is never parsed on the VP8X
                             path, so it stays a header bomb.
  bomb-tiff-20000x20000.tiff little-endian TIFF, one IFD carrying
                             ImageWidth/ImageLength = 20000 plus the minimal
                             tags the tiff decoder needs to accept the IFD.
                             (SHORT entries pack count as u32 — the
                             generator's first version packed it as u16,
                             which the reader read as count ~524289 and
                             failed filling the buffer; fixed 2026-11-27
                             by the 215th.)

Regenerate: python3 gen_bomb_fixtures.py && sha256sum bomb-* > SHA256SUMS
(verify with: sha256sum -c SHA256SUMS)
"""
import struct
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent


def png_chunk(kind: bytes, data: bytes) -> bytes:
    crc = zlib.crc32(kind)
    crc = zlib.crc32(data, crc)
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", crc & 0xFFFFFFFF)


def png_header_only(width: int, height: int) -> bytes:
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)  # 8-bit RGB
    # An IDAT chunk MUST be present: png 0.18's read_info() calls
    # read_until_image_data(), which errors "IDAT or fdAT chunk is
    # missing" at IEND when no IDAT began. The chunk carries an empty
    # zlib stream (8 bytes) — still no pixel data, so a decode fails
    # for lack of rows while the guard rejects from the IHDR dims.
    empty_idat = png_chunk(b"IDAT", zlib.compress(b""))
    return sig + png_chunk(b"IHDR", ihdr) + empty_idat + png_chunk(b"IEND", b"")


def png_bomb(width: int, height: int) -> bytes:
    """Valid solid-black RGB PNG. Scanlines are streamed through one
    zlib compressor in 256-row groups, so peak RAM is a few MB no matter
    how big the claimed image is."""
    out = bytearray(png_header_only(width, height)[:-32])  # drop the empty IDAT + IEND, re-add later
    comp = zlib.compressobj(level=9)
    row = b"\x00" + b"\x00" * (width * 3)  # filter byte 0 + black pixels
    idat = bytearray()
    for start in range(0, height, 256):
        block = row * min(256, height - start)
        idat += comp.compress(block)
        # Keep IDAT chunks bounded: flush a chunk every ~64 KiB of output.
        while len(idat) >= 65536:
            out += png_chunk(b"IDAT", bytes(idat[:65536]))
            del idat[:65536]
    idat += comp.flush()
    while idat:
        out += png_chunk(b"IDAT", bytes(idat[:65536]))
        del idat[:65536]
    out += png_chunk(b"IEND", b"")
    return bytes(out)


def jpeg_header_bomb(width: int, height: int) -> bytes:
    out = bytearray(b"\xff\xd8")  # SOI
    app0 = b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
    out += b"\xff\xe0" + struct.pack(">H", len(app0) + 2) + app0
    # SOF0: precision 8, height, width, 3 components (Y/Cb/Cr, 4:4:4).
    sof = struct.pack(">BHHB", 8, height, width, 3)
    for comp_id, sampling in ((1, 0x11), (2, 0x11), (3, 0x11)):
        sof += struct.pack(">BBB", comp_id, sampling, 0)
    out += b"\xff\xc0" + struct.pack(">H", len(sof) + 2) + sof
    # SOS is REQUIRED: zune-jpeg's decode_headers() loops over markers
    # until SOS and hits EOF (unreadable) on a file ending at SOF0.
    # parse_sos() needs no DQT/DHT tables — it only checks the scan
    # header against the SOF components — so no scan data follows.
    sos = struct.pack(">HB", 12, 3)
    for comp_id, tables in ((1, 0x00), (2, 0x11), (3, 0x11)):
        sos += struct.pack(">BB", comp_id, tables)
    sos += bytes((0, 63, 0))  # Ss=0, Se=63, Ah/Al=0
    out += b"\xff\xda" + sos  # sos already starts with its length (12)
    out += b"\xff\xd9"  # EOI
    return bytes(out)


def webp_header_bomb(width: int, height: int) -> bytes:
    # VP8X: flags 0, 3 reserved bytes, width-1 / height-1 as u24 LE.
    vp8x = b"\x00\x00\x00\x00" + (width - 1).to_bytes(3, "little") + (height - 1).to_bytes(3, "little")
    chunk = b"VP8X" + struct.pack("<I", len(vp8x)) + vp8x
    # A minimal VP8L chunk is REQUIRED: image-webp rejects a VP8X-only
    # file ("An expected chunk was missing") — a non-animated image
    # must have exactly one of VP8/VP8L. The VP8L payload is just its
    # signature + dims header (5 bytes); it is never parsed on the
    # VP8X path, so the file stays a header bomb. Odd-sized payload
    # takes one pad byte.
    vp8l_payload = b"\x2f" + struct.pack(
        "<I", (width - 1) | ((height - 1) << 14)
    )
    vp8l = b"VP8L" + struct.pack("<I", len(vp8l_payload)) + vp8l_payload + b"\x00"
    riff_payload = b"WEBP" + chunk + vp8l
    return b"RIFF" + struct.pack("<I", len(riff_payload)) + riff_payload


def tiff_header_bomb(width: int, height: int) -> bytes:
    # Little-endian TIFF, one IFD. Tags (sorted): 256 width, 257 height,
    # 258 BitsPerSample, 259 Compression=1, 262 Photometric=1, 273
    # StripOffsets, 277 SamplesPerPixel=1, 278 RowsPerStrip, 279
    # StripByteCounts. A 1x1 black strip follows the IFD so the offsets
    # point at real bytes.
    entries = [
        (256, 4, 1, width),   # ImageWidth, LONG
        (257, 4, 1, height),  # ImageLength, LONG
        (258, 3, 1, 8),       # BitsPerSample, SHORT
        (259, 3, 1, 1),       # Compression: none
        (262, 3, 1, 1),       # Photometric: BlackIsZero
        (273, 4, 1, 0),       # StripOffsets — patched below
        (277, 3, 1, 1),       # SamplesPerPixel: 1 (grayscale)
        (278, 4, 1, height),  # RowsPerStrip: whole image, one strip
        (279, 4, 1, width),   # StripByteCounts: one row of bytes
    ]
    ifd_size = 2 + len(entries) * 12 + 4
    strip_offset = 8 + ifd_size
    entries = [(t, ty, c, strip_offset if t == 273 else v) for (t, ty, c, v) in entries]
    out = bytearray(b"II*\x00" + struct.pack("<I", 8))
    out += struct.pack("<H", len(entries))
    for tag, typ, count, value in entries:
        if typ == 3:  # SHORT: count is u32, value is u16 + 2 pad bytes
            out += struct.pack("<HHIHH", tag, typ, count, value, 0)
        else:
            out += struct.pack("<HHII", tag, typ, count, value)
    out += struct.pack("<I", 0)  # next IFD: none
    out += b"\x00" * width  # one black strip row (the declared strip bytes)
    return bytes(out)


def main() -> None:
    files = {
        "bomb-png-20000x20000.png": png_bomb(20000, 20000),
        "bomb-png-8001x8001.png": png_bomb(8001, 8001),
        "bomb-png-header-only-20000x20000.png": png_header_only(20000, 20000),
        "bomb-jpeg-20000x20000.jpg": jpeg_header_bomb(20000, 20000),
        "bomb-webp-20000x20000.webp": webp_header_bomb(20000, 20000),
        "bomb-tiff-20000x20000.tiff": tiff_header_bomb(20000, 20000),
    }
    for name, data in files.items():
        (HERE / name).write_bytes(data)
        decoded = "n/a (header-only)" if "header-only" in name else "see generator"
        print(f"{name}: {len(data)} bytes on disk, {decoded}")
    print("regenerate SHA256SUMS with: cd crates/engine/tests/fixtures/bombs && sha256sum bomb-* gen_bomb_fixtures.py > SHA256SUMS")


if __name__ == "__main__":
    main()
