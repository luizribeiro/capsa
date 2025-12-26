//! Benchmarks for VM boot time.

use capsa::test_utils::test_vm;
use criterion::{Criterion, criterion_group, criterion_main};
use std::time::Duration;

fn custom_criterion() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .sample_size(10)
}

fn boot_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("boot", |b| {
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
}

criterion_group! {
    name = benches;
    config = custom_criterion();
    targets = boot_benchmark
}

criterion_main!(benches);
