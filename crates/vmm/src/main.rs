use anyhow::{Context, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);

    let flag = args.next();
    let launch_spec_json = args.next();

    if flag.as_deref() != Some("--launch-spec-json")
        || launch_spec_json.is_none()
        || args.next().is_some()
    {
        anyhow::bail!("usage: capsa-vmm --launch-spec-json <json>");
    }

    let launch_spec: capsa_core::VmmLaunchSpec = serde_json::from_str(
        launch_spec_json
            .as_deref()
            .expect("checked above: launch spec json is present"),
    )
    .context("failed to parse VMM launch spec JSON")?;

    capsa_core::start_vm(&launch_spec)
}
