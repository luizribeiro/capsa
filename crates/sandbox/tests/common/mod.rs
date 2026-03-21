use std::path::PathBuf;

pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(prefix: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push("capsa-sandbox-tests");
        path.push(format!(
            "{}-{}-{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("failed to create test dir {}: {e}", path.display()));

        Self { path }
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn run_probe(spec: &capsa_sandbox::SandboxSpec, args: &[&str]) -> bool {
    let probe = probe_binary();
    let argv = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();

    let child = capsa_sandbox::spawn_sandboxed(&probe, &argv, spec).unwrap_or_else(|e| {
        panic!(
            "failed to spawn sandboxed probe {} with args {:?}: {e}",
            probe.display(),
            args
        )
    });

    let status = child
        .wait()
        .unwrap_or_else(|e| panic!("failed to wait on sandboxed probe: {e}"));

    status.success()
}

fn probe_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_sandbox_probe") {
        return PathBuf::from(path);
    }

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../target/debug/sandbox_probe");

    if cfg!(target_os = "windows") {
        path.set_extension("exe");
    }

    assert!(
        path.exists(),
        "sandbox probe binary not found at {}",
        path.display()
    );

    path.canonicalize()
        .unwrap_or_else(|e| panic!("failed to canonicalize {}: {e}", path.display()))
}
