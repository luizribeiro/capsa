//! Benchmarks for VM boot time.
//!
//! Run with: cargo bench --bench boot --features vfkit
//!       or: cargo bench --bench boot --features macos-native

use apple_main::criterion::{criterion_group, Criterion};
use capsa::test_utils::test_vm;
use std::time::Duration;

fn boot_benchmark(c: &mut Criterion) {
    apple_main::init_runtime();

    let mut group = c.benchmark_group("vm_boot");
    group.measurement_time(Duration::from_secs(60));
    group.sample_size(10);

    for vm_name in ["minimal", "minimal-net", "ultra-minimal"] {
        group.bench_function(vm_name, |b| {
            b.iter(|| {
                apple_main::block_on(async {
                    let vm = test_vm(vm_name)
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
    group.finish();
}

criterion_group!(benches, boot_benchmark);
apple_main::criterion_main!(benches);
