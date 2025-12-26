//! Benchmarks for IPC overhead (subprocess backend).

use capsa::test_utils::test_vm;
use criterion::{Criterion, criterion_group, criterion_main};
use std::time::Duration;

fn custom_criterion() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(120))
        .sample_size(10)
}

fn native_backend_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("native_backend");

    group.bench_function("vm_lifecycle", |b| {
        b.iter(|| {
            rt.block_on(async {
                let vm = test_vm("default")
                    .build()
                    .await
                    .expect("Failed to build VM");
                let console = vm.console().await.expect("Failed to get console");
                console
                    .wait_for_timeout("Boot successful", Duration::from_secs(30))
                    .await
                    .expect("VM did not boot");
                vm.kill().await.expect("Failed to stop VM");
            })
        })
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = custom_criterion();
    targets = native_backend_benchmark
}

criterion_main!(benches);
