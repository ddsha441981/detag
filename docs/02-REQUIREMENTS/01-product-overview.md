# Product Overview

## Vision

PureFrame is a SaaS that removes watermarks, badges, and overlaid marks from **AI-generated images and videos that the user owns or has rights to**, with a hard promise: **the output is indistinguishable from the input everywhere except the removed regions**. No resizing, no recompression soup, no blurred smudges. Every job returns a machine-verifiable quality report.

The wedge against existing tools: they destroy quality (resize, heavy re-encode, crude inpainting) and have zero provenance awareness. PureFrame treats quality as a tested gate and provenance as a first-class, compliance-gated concern.

## Users / personas

| Persona | Need | Shape of use |
| --- | --- | --- |
| Solo creator (Priya) | Cleans her own Gemini/Sora/Midjourney outputs for client delivery; hates quality loss | Web uploader, few jobs/day, free plan then Pro |
| Developer / agency (Arjun) | Bulk cleans generated media before publishing pipelines; wants API + webhooks | API keys, batch jobs, Pro/Business |
| Studio / ops team (Meera) | Many videos/day, needs admin oversight, quotas, billing | Business plan, admin panel, audit trail |

## Scope

**In scope (v1):**
- Images: PNG / JPEG / WebP, up to 8000×8000. Detection of AI badges/text marks + manual hint mode.
- Video: MP4/MKV H.264 (VP9, HEVC best-effort), up to 4 GB, detection on keyframes + mask tracking.
- Removal via inpainting (LaMa ONNX primary, in-house diffusion fallback), format-preserving encode.
- Modes: `auto`, `manual` (user hints), `detect_only` (returns mask, no edit), `metadata_only` (true lossless stream-copy).
- SaaS: register/login (JWT), plans + quotas, Stripe billing (Razorpay adapter behind flag), API keys, webhooks, admin panel, audit.
- Quality gates with per-job report (SSIM/PSNR images, VMAF video).

**Out of scope (v1, and policy-prohibited):**
- Removing watermarks from stock-photo/agency content (Getty, Shutterstock, photographer marks) — AUP violation, see `05-compliance-aup.md`.
- Any tooling marketed or used to defeat provenance for deception. Provenance-strip (C2PA) is a gated feature; SynthID removal is research-only with honest feasibility.
- Social media scraping/downloaders.
- Mobile apps (web only in v1).

## Value propositions

1. **Quality guarantee, proven per job** — masked-region metrics + stream preservation rules, surfaced in UI and API.
2. **Real engineering** — idempotent jobs, resumable uploads, signed URLs, audit trail, SLOs, runbooks.
3. **Compliance-aware** — ownership attestation, AUP enforcement, provenance-preserve default; the product a serious company can actually ship.

## Success metrics (v1 targets, 90 days post-launch)

| Metric | Target |
| --- | --- |
| Job success rate (succeeded / completed jobs) | >= 97% |
| Quality gate first-pass rate | >= 90% |
| Activation (signup → first successful job) | >= 60% |
| Free → Pro conversion | >= 4% |
| p95 image job time (<= 4K, CPU) | <= 30 s |
| Video processing throughput (1080p, CPU) | >= 0.5x realtime |

## Naming

`PureFrame` is a working title. Rename is a one-day config/brand task; the owner decides (`owner-input-needed`). Nothing in engineering depends on the name.

## Traces

Functional requirements: `02-functional-requirements.md`. Quality doctrine: `04-quality-policy.md`. Legal boundaries: `05-compliance-aup.md`.
