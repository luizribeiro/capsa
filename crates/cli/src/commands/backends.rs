//! The `backends` command - shows available backends and their capabilities.

use capsa::capabilities::{HostPlatform, HypervisorBackend, available_backends};
use clap::Args;

#[derive(Args)]
pub struct BackendsArgs {
    /// Output in JSON format
    #[arg(long)]
    json: bool,
}

pub fn run(args: BackendsArgs) {
    let backends = available_backends();

    if args.json {
        print_json(&backends);
    } else {
        print_text(&backends);
    }
}

fn print_json(backends: &[Box<dyn HypervisorBackend>]) {
    println!("{{");
    println!("  \"backends\": [");
    for (i, backend) in backends.iter().enumerate() {
        let caps = backend.capabilities();
        println!("    {{");
        println!("      \"name\": \"{}\",", backend.name());
        println!(
            "      \"platform\": \"{}\",",
            platform_name(backend.platform())
        );
        println!("      \"available\": {},", backend.is_available());
        println!("      \"capabilities\": {{");
        println!("        \"guest_os\": {{");
        println!("          \"linux\": {}", caps.guest_os.linux);
        println!("        }},");
        println!("        \"boot_methods\": {{");
        println!(
            "          \"linux_direct\": {}",
            caps.boot_methods.linux_direct
        );
        println!("        }},");
        println!("        \"image_formats\": {{");
        println!("          \"raw\": {},", caps.image_formats.raw);
        println!("          \"qcow2\": {}", caps.image_formats.qcow2);
        println!("        }},");
        println!("        \"network_modes\": {{");
        println!("          \"none\": {},", caps.network_modes.none);
        println!("          \"nat\": {}", caps.network_modes.nat);
        println!("        }},");
        println!("        \"share_mechanisms\": {{");
        println!(
            "          \"virtio_fs\": {},",
            caps.share_mechanisms.virtio_fs
        );
        println!(
            "          \"virtio_9p\": {}",
            caps.share_mechanisms.virtio_9p
        );
        println!("        }},");
        println!(
            "        \"max_cpus\": {},",
            caps.max_cpus.map_or("null".to_string(), |n| n.to_string())
        );
        println!(
            "        \"max_memory_mb\": {}",
            caps.max_memory_mb
                .map_or("null".to_string(), |n| n.to_string())
        );
        println!("      }}");
        if i < backends.len() - 1 {
            println!("    }},");
        } else {
            println!("    }}");
        }
    }
    println!("  ]");
    println!("}}");
}

fn print_text(backends: &[Box<dyn HypervisorBackend>]) {
    if backends.is_empty() {
        println!("No backends available.");
        return;
    }

    println!("Available backends:");
    println!();

    for backend in backends {
        let caps = backend.capabilities();
        let status = if backend.is_available() {
            "Available"
        } else {
            "Not available"
        };

        println!(
            "  {} ({})",
            backend.name(),
            platform_name(backend.platform())
        );
        println!("    Status: {status}");
        println!("    Guest OS: Linux={}", yes_no(caps.guest_os.linux));
        println!(
            "    Boot methods: direct={}",
            yes_no(caps.boot_methods.linux_direct)
        );
        println!(
            "    Disk formats: raw={}, qcow2={}",
            yes_no(caps.image_formats.raw),
            yes_no(caps.image_formats.qcow2)
        );
        println!(
            "    Network: none={}, nat={}",
            yes_no(caps.network_modes.none),
            yes_no(caps.network_modes.nat)
        );
        println!(
            "    Shares: virtio-fs={}, 9p={}",
            yes_no(caps.share_mechanisms.virtio_fs),
            yes_no(caps.share_mechanisms.virtio_9p)
        );
        if caps.max_cpus.is_some() || caps.max_memory_mb.is_some() {
            println!(
                "    Limits: cpus={}, memory={}",
                caps.max_cpus
                    .map_or("unlimited".to_string(), |n| n.to_string()),
                caps.max_memory_mb
                    .map_or("unlimited".to_string(), |n| format!("{n} MB"))
            );
        }
        println!();
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn platform_name(platform: HostPlatform) -> &'static str {
    match platform {
        HostPlatform::MacOs => "macos",
        HostPlatform::Linux => "linux",
    }
}
