// SPDX-License-Identifier: GPL-3.0-or-later
//! Start menu search cost per keystroke (runs on the UI thread).
//! `cargo bench -p explorer-core --bench search`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use explorer_core::catalog::{AppEntry, AppSource, Catalog, LaunchHistory, LaunchTarget};
use explorer_core::search::Searcher;

const WORDS: &[&str] = &[
    "Microsoft",
    "Visual",
    "Studio",
    "Code",
    "Bloc",
    "de",
    "notas",
    "Configuración",
    "Panel",
    "Control",
    "Terminal",
    "PowerShell",
    "Herramienta",
    "Recortes",
    "Editor",
    "Registro",
    "Administrador",
    "Tareas",
    "Explorador",
    "archivos",
    "Calculadora",
    "Símbolo",
    "sistema",
    "Teclado",
    "pantalla",
    "Copias",
    "seguridad",
    "Windows",
];

fn catalog(n: usize) -> Catalog {
    // Deterministic pseudo-random 2-4 word names.
    let mut x: u32 = 0x2545_F491;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x as usize
    };
    Catalog::build((0..n).map(|i| {
        let words = 2 + next() % 3;
        let name = (0..words)
            .map(|_| WORDS[next() % WORDS.len()])
            .collect::<Vec<_>>()
            .join(" ");
        AppEntry {
            name: format!("{name} {i}"),
            target: LaunchTarget::AppsFolder(format!("app{i}")),
            source: AppSource::StartMenu,
            keywords: vec![],
        }
    }))
}

fn bench(c: &mut Criterion) {
    let cat = catalog(300);
    let hist = LaunchHistory::default();
    let mut s = Searcher::new();
    for q in ["b", "blo", "config", "vsc", "panel control", "zzzz"] {
        c.bench_function(&format!("search 300 apps {q:?}"), |b| {
            b.iter(|| s.search(black_box(&cat), &hist, black_box(q), 50, 0).len())
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
