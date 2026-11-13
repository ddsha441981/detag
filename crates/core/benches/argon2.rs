//! Argon2id cost benches (P1-027).
//!
//! What this measures: `hash_password` (the register/login
//! cost) and `verify_password` at the current defaults
//! (m = 64 MiB, t = 3, p = 4) versus the pre-P1-027 defaults
//! (m = 19 MiB, t = 2, p = 1), so a future params change can
//! be priced before it lands. Sample size is small (10) and
//! the measurement window short on purpose: one hash at the
//! new defaults takes ~0.5–1 s, so the default 100-sample
//! run would take minutes for a number nobody re-reads.
//!
//! Run: `cargo bench -p detag-core --bench argon2`.
//! Recorded numbers (2026-11-13, this VM — 1.5-CPU share,
//! so no faster than the 4-vCPU target VPS; treat as an upper
//! bound) live in the deploy runbook's Argon2 note.

use criterion::{criterion_group, criterion_main, Criterion};
use detag_core::password::{hash_password, verify_password, Argon2Params};
use std::hint::black_box;

const PASSWORD: &str = "correct horse battery staple";

fn bench_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("argon2id/hash");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(20));
    for (name, params) in [
        ("current/m=65536,t=3,p=4", Argon2Params::defaults()),
        (
            "previous/m=19456,t=2,p=1",
            Argon2Params::previous_defaults(),
        ),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| hash_password(black_box(PASSWORD), &params).unwrap());
        });
    }
    group.finish();
}

fn bench_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("argon2id/verify");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(20));
    for (name, params) in [
        ("current/m=65536,t=3,p=4", Argon2Params::defaults()),
        (
            "previous/m=19456,t=2,p=1",
            Argon2Params::previous_defaults(),
        ),
    ] {
        let hash = hash_password(PASSWORD, &params).unwrap();
        group.bench_function(name, |b| {
            b.iter(|| verify_password(black_box(PASSWORD), black_box(&hash)).unwrap());
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hash, bench_verify);
criterion_main!(benches);
