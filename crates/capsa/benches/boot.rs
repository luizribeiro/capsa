#![feature(custom_test_frameworks)]
#![test_runner(apple_main::criterion_runner)]

//! Benchmarks for VM boot time.

use apple_main::criterion::Criterion;
use apple_main::criterion_macro::criterion;
use capsa::test_utils::test_vm;
use std::time::Duration;

fn custom_criterion() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .sample_size(10)
}

#[criterion(custom_criterion())]
fn boot_benchmark(c: &mut Criterion) {
    for vm_name in ["minimal", "minimal-net", "ultra-minimal"] {
        c.bench_function(vm_name, |b| {
            b.iter(|| {
                apple_main::block_on(async {
                    let vm = test_vm(vm_name).build().await.expect("Failed to build VM");
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
}
