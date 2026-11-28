# Intake spec — supported matrix, error codes, memory budgets, security notes

> P2-010 (docs, Phase 2, E2.1 intake). Status: spec committed 2026-11-28.
> Source of truth for every claim below is the code it cites; where a
> number lives in code, the constant is named so a drift check can find it.
> The pipeline itself is `run_intake` (`crates/engine/src/suite.rs`,
> P2-009, ADR-0078); the pieces it wires are P2-001–P2-008 (ADRs 0071–0077).

## 1. Supported matrix

The intake accepts **images only**. Video kinds sniff fine (P2-001) but are
rejected by the intake as `NotAnImage` — the intake caller for video is
Phase 3.

| Format | Bit depth | Always on | Cargo feature | Notes |
| ------ | --------- | --------- | ------------- | ----- |
| PNG | 8-bit / 16-bit | yes | — | 16-bit decodes to `ImageRgb16`/`ImageRgba16`, never downsampled to 8-bit (FR-D-07). |
| JPEG | 8-bit | yes | — | Baseline + progressive, via zune-jpeg. |
| WebP | 8-bit (lossy + lossless) | yes | — | Lossless via VP8L; lossy proven against the committed Pillow fixtures (the `image` crate's `WebPEncoder` is lossless-only, so no lossy encoder exists in-tree to round-trip against). |
| TIFF | 8-bit / 16-bit | default | `tiff` | Pure Rust (`tiff` crate); disable with `--no-default-features`. |
| AVIF | 8-bit / 10-bit / 12-bit | no | `avif` | Decode needs the `image` crate's `avif-native` feature, which links the SYSTEM libdav1d (>= 1.3.0) — install `libdav1d-dev` before building with it (ADR-0071). Without the feature, AVIF decode fails with `DecodeError::FeatureDisabled`, never silently. AVIF encode (ravif, pure Rust) rides along with the same feature. |

Format is decided **once, by magic bytes, and never re-guessed**: the
P2-001 sniffer (`detag_core::media::sniff_media`, head = first 4096 bytes,
tail = last 4096 bytes — `HEAD_SNIFF_LEN` / `TAIL_SNIFF_LEN`) proves the
kind, and `decode_image` takes that kind as a forced parameter. A file
whose bytes disagree with its sniffed kind fails instead of decoding as
something else (magic wins, end to end). File extensions and MIME types
are never trusted for the verdict.

## 2. Pipeline order (fixed)

`run_intake(bytes, max_bytes, budget)` — one order, no skipping, so the
guard/budget verdicts can never be bypassed by a future caller:

1. **Sniff** — `sniff_media(head, tail, total_size)` (PF-2001).
2. **Guards** — `check_intake_guards` (PF-2003 byte cap, PF-2005 dims).
3. **Budgeted decode** — `decode_budgeted` (PF-2007).
4. **EXIF** — `parse_exif` (malformed → empty summary, intake continues).
5. **ICC** — `extract_icc` (missing/corrupt → `present: false`, intake continues).
6. **Orientation** — `normalize_orientation` (applied to pixels before hashing).
7. **Hashes** — `HashRecord::compute` (sha256 of the raw bytes; dHash/aHash of the **upright** pixels).
8. **Merge** — `merge_metadata_fragments` into the single `uploads.metadata` object (`exif` / `icc` / `orientation` / `hashes` — duplicate key is a typed error, fail-closed).

The engine carries **no database**: the outcome returns `hashes.dhash` +
the merged `metadata`, and the caller stores them via core `set_phash` /
`set_metadata` (both guarded on `status = 'ready'`). The outcome reports
**upright** dims (post-normalization: an orientation-6 image stored 4x3
reports 3x4); `bit_depth` comes from the decoded image (a rotation/flip
cannot change it). The guard's probed dims are the **stored** dims — the
two agree exactly when no orientation tag is present.

## 3. Error codes

All intake failures are typed; the HTTP mapping is the `pf_code()` of the
inner error (the rest carry `None` and surface as 500-class via the
handler's mapping — they are unreachable-after-prior-stages or genuine
inconsistencies, never user input to blame).

| Code | HTTP | Stage | Trigger | Source |
| ---- | ---- | ----- | ------- | ------ |
| PF-2001 | 415 | Sniff | Not an image, empty input, polyglot (PNG+ZIP / JPEG+ZIP), or a kind the build cannot decode | `AppError::UnsupportedMediaType` |
| PF-2003 | 413 | Guards | Input bytes exceed the plan byte cap (`max_bytes`, caller-supplied) | `GuardError::ByteCapExceeded` |
| PF-2005 | 413 | Guards | Claimed dims fail `validate_dimensions`: a side > 8000, total pixels > 64,000,000, or a side < 1 / missing | `GuardError::DimensionsExceeded` (`MAX_IMAGE_SIDE`, `MAX_IMAGE_PIXELS`) |
| PF-2007 | 413 | Decode | Decoded size (from the header via `decoder.total_bytes()`) exceeds the budget | `DecodeError::BudgetExceeded` |
| — (typed, no PF code) | — | Decode | AVIF with the `avif` feature off (`FeatureDisabled`); video kind (`NotAnImage`); empty input | `DecodeError` |
| — (typed, no PF code) | — | EXIF | Out-of-range orientation tag (`InvalidOrientation`) — fails loudly, because P2-005 would apply it into pixels. Malformed EXIF (`Parse`) is NOT an error: it degrades to an empty summary. | `ExifError` |
| — (typed, no PF code) | — | ICC | Only `IccError::Decode` from `into_decoder()` — unreachable in practice, because `decode_budgeted` already built the same decoder on the same bytes; a failure here is a genuine inconsistency and fails closed. | `IccError` |

## 4. Memory budgets

| Budget | Value | Constant | Meaning |
| ------ | ----- | -------- | ------- |
| Decode budget (default) | 512 MiB (536,870,912 B) | `DecodeBudget::DEFAULT_MAX_BYTES` | Admits every image P2-006 admits at worst case (8000×8000 RGBA16 = 512,000,000 B < budget) and equals the `image` crate's default `Limits::default` allocation bound. Decided from the header (`decoder.total_bytes()`) **before** any pixel is allocated — a bomb is rejected without decoding. |
| RSS ceiling | 1 GiB (1,073,741,824 B) | `RSS_CEILING_BYTES` | ~2× the budget; the ceiling a decode must never cross. Asserted as a test (benches do not run in `just verify`, so the ceiling assertion lives in the test suite, ADR-0076 §5). |
| Dimension cap | 8000×8000, 64 MP total | `MAX_IMAGE_SIDE`, `MAX_IMAGE_PIXELS` (`detag_core::media`) | Single source of truth: the guard delegates to core `validate_dimensions`; the budget's worst case is sized against it. |
| Byte cap | plan-dependent | `max_bytes` parameter | Enforced by the guard before the header probe; `max_bytes = 0` fails closed. |

Deliberate duplication: the guard and the budget each re-read the header
(one extra header parse per intake, negligible next to a decode) because
each module owns its verdict — the P2-006/P2-007 precedent. Upstream
`image::Limits` was rejected for the verdict (its `max_alloc` is
non-strict and working buffers count toward it — an exact verdict turned
flaky) and as a backstop.

## 5. Security notes (tied to the threat model)

- **Polyglot rejection is in the sniffer, not the suite.** Head rule +
  tail archive scan + tail markers reject PNG+ZIP / JPEG+ZIP as PF-2001
  (P2-001). The suite adds no polyglot logic of its own — one place owns
  the rule, so it cannot drift.
- **Decompression bombs are rejected before allocation.** The guard
  probes dims from the header only (`into_dimensions()`, no pixel
  allocation) and the budget decides from `total_bytes()` before decoding.
  Committed fixtures prove it: real 20000×20000 PNG bomb, 8001×8001
  one-over, header-only PNG/JPEG/WebP/TIFF 20000×20000 bombs
  (`crates/engine/tests/fixtures/bombs/`, P2-006).
- **Header lies cannot smuggle pixels.** The guard's verdict uses the
  claimed dims; the budget's verdict uses the decoder's own
  `total_bytes()`; both must pass, in order, before `from_decoder` runs.
- **No silent downgrades.** 16-bit stays 16-bit through the suite (a
  16-bit PNG is hashed and reported as 16-bit); AVIF without the feature
  fails loudly; a corrupt ICC profile is recorded as absent, never
  substituted; a malformed EXIF segment yields `"exif": {}`, and the
  intake does NOT record that EXIF was present-but-broken (a future
  observability ticket may add a `parse_failed` flag; the fragment shape
  must stay backward-compatible if it does).
- **Orientation is applied before hashing** (P2-008): the perceptual
  hashes describe the upright pixels, so the same image stored rotated
  and unrotated keys the same. Hashes are similarity keys, not identity —
  consumers compare by Hamming distance, never by equality alone; the
  sha256 is the only exact-identity claim. A perceptual hash says nothing
  about provenance or ownership.
- **GPS is never read** (P2-003): no location field exists in the EXIF
  summary, so no location can leak into `uploads.metadata`.
- **Ownership boundary (AUP).** Intake runs only on uploads the user
  owns — the ownership attestation + AUP gate sits above the intake (the
  job-create handler), not in it. Provenance-strip is OFF and no
  invisible-watermark removal is claimed anywhere in this pipeline: the
  suite removes a *visible* watermark's pixels only when a later ticket
  (the removal engine) is told to, and the intake itself changes no
  pixels except the EXIF-orientation normalization it records as
  `stripped: true` (the tag is consumed; the physical strip lands at
  encode, E2.6/NFR-43).
- **No panics on untrusted data.** Every stage maps malformed input to a
  typed error; the suite contains no unwraps on untrusted data. Fuzz
  coverage for the parse surface lives in `crates/engine/fuzz/`
  (`exif_parse`, P2-003).

## 6. What this spec does NOT cover

- No HTTP endpoint runs the suite yet (job creation is the 501 stub) —
  the suite is in-process only; nothing is stored in production by it.
- Tiling ("later", P2-007): the suite decodes whole images inside budget.
- AVIF decode is default-OFF (the `libdav1d` follow-up is still open).
- The golden suite (E2.7) does not exist yet — the AC is proven by the
  stability tests, not by a timing row; no bench (the suite's cost is
  decode-dominated, P2-002's `decode` bench already covers it).
