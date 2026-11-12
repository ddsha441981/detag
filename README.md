# Detag

[![CI](https://github.com/ddsha441981/detag/actions/workflows/ci.yml/badge.svg)](https://github.com/ddsha441981/detag/actions/workflows/ci.yml)

Detag removes watermarks, badges and overlaid marks from AI-generated
images and videos that the user owns or has rights to — with a hard,
machine-verified promise: the output is indistinguishable from the input
everywhere except the removed regions. No resizing, no recompression soup,
no blurred smudges. Every job returns a quality report (image SSIM ≥ 0.98
and PSNR ≥ 38 dB on unmasked pixels; video VMAF ≥ 93; audio bit-exact).

Rust backend (Axum + Postgres + Redis + S3-compatible storage), LaMa ONNX
inpainting, Next.js console, admin panel, JWT auth, plans + billing,
quality gates, observability. Provenance marks (C2PA) are preserved by
default; stripping them is a gated feature, and invisible-watermark
(SynthID-class) removal is research-only — see the compliance doc.

## Docs

The full build plan and spec live in [`docs/`](docs/) (the agent kit):
start at [`docs/00-AGENT-BOOT.md`](docs/00-AGENT-BOOT.md), then
[`docs/01-WORKFLOW.md`](docs/01-WORKFLOW.md). Product overview:
[`docs/02-REQUIREMENTS/01-product-overview.md`](docs/02-REQUIREMENTS/01-product-overview.md).
Compliance and acceptable use:
[`docs/02-REQUIREMENTS/05-compliance-aup.md`](docs/02-REQUIREMENTS/05-compliance-aup.md).

## Develop

```sh
cp .env.example .env   # dev placeholders only, never real secrets
just dev               # build + start api/worker, wait for /healthz
just verify            # fmt + clippy -D warnings + tests
just deny              # RUSTSEC advisories + license allow-list
just secrets           # gitleaks full-repo scan
just env-check         # .env.example drift vs code + runbook
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for toolchain, commits and the
definition of done.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
