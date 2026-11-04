// SPDX-License-Identifier: GPL-3.0-or-later
//! Benchmarks: index build time, memory per entry, query latency and
//! snapshot load/save at 1M and 5M synthetic entries.
//!
//! Run: `cargo bench -p explorer-index`. Set `EXPLORER_INDEX_BENCH_SIZES`
//! (comma-separated, e.g. `1000000`) to change the sizes. Memory per entry
//! is printed once per size (criterion measures time only).

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use explorer_index::index::{Index, VolumeKind};
use explorer_index::query::{search, Query, QueryOptions};
use explorer_index::{snapshot, synth};

const QUERIES: &[&str] = &[
    "report",
    "e",
    "setup.exe",
    "^read",
    "ext:pdf budget",
    "~prjfnl",
    "path:windows driver",
    "config !ext:json",
    r"program\update",
    "size:>10mb ext:mp4",
];

fn sizes() -> Vec<usize> {
    std::env::var("EXPLORER_INDEX_BENCH_SIZES")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1_000_000, 5_000_000])
}

fn build(n: usize) -> Index {
    let tree = synth::tree(n, 42);
    let mut ix = Index::new();
    let v = ix.add_volume("C:\\", VolumeKind::Walk).unwrap();
    ix.replace_tree(v, &tree).unwrap();
    ix.shrink_to_fit();
    ix
}

fn bench(c: &mut Criterion) {
    for n in sizes() {
        let label = format!("{}M", n as f64 / 1e6);
        let t0 = Instant::now();
        let ix = build(n);
        let build_ms = t0.elapsed().as_millis();
        let mem = ix.memory_usage();
        let names: usize = ix.live_ids().map(|id| ix.folded_name(id).len()).sum();
        eprintln!(
            "[{label}] entries={} build={build_ms}ms memory={} MiB bytes/entry={:.1} (avg name {:.1} bytes)",
            ix.len(),
            mem >> 20,
            mem as f64 / ix.len() as f64,
            names as f64 / ix.len() as f64
        );

        let mut g = c.benchmark_group(format!("query-{label}"));
        g.sample_size(20);
        for q in QUERIES {
            let parsed = Query::parse(q, QueryOptions::default()).unwrap();
            let cancel = AtomicBool::new(false);
            g.bench_function(*q, |b| {
                b.iter(|| black_box(search(&ix, &parsed, 50, &cancel).unwrap().total))
            });
        }
        g.finish();

        let mut g = c.benchmark_group(format!("build-{label}"));
        g.sample_size(10);
        let tree = synth::tree(n, 42);
        g.bench_function("replace_tree", |b| {
            b.iter_batched(
                || {
                    let mut ix = Index::new();
                    let v = ix.add_volume("C:\\", VolumeKind::Walk).unwrap();
                    (ix, v)
                },
                |(mut ix, v)| {
                    ix.replace_tree(v, &tree).unwrap();
                    ix
                },
                BatchSize::LargeInput,
            )
        });
        let bytes = snapshot::encode(&ix);
        eprintln!("[{label}] snapshot size={} MiB", bytes.len() >> 20);
        g.bench_function("snapshot_encode", |b| {
            b.iter(|| black_box(snapshot::encode(&ix).len()))
        });
        g.bench_function("snapshot_decode", |b| {
            b.iter(|| black_box(snapshot::decode(&bytes).unwrap().len()))
        });
        g.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
