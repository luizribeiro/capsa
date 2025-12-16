//! Benchmarks for VM boot time.

use capsa::test_utils::test_vm;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;
use tokio::runtime::Runtime;

fn bench_vm_boot(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("vm_boot");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(60));

    for vm_name in ["default", "minimal", "minimal-net", "ultra-minimal"] {
        group.bench_with_input(BenchmarkId::new("to_shell", vm_name), &vm_name, |b, &name| {
            b.to_async(&rt).iter(|| async move {
                let vm = test_vm(name)
                    .build()
                    .await
                    .expect("Failed to build VM");

                let console = vm.console().await.expect("Failed to get console");

                console
                    .wait_for_timeout("Boot successful", Duration::from_secs(30))
                    .await
                    .expect("VM did not boot");

                vm.stop().await.expect("Failed to stop VM");
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_vm_boot);

criterion_main!(benches);
