use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
enum SeatbeltFilter {
    Literal(PathBuf),
    Subpath(PathBuf),
    Regex(&'static str),
}

#[derive(Debug, Clone)]
struct SeatbeltRule {
    operations: Vec<&'static str>,
    filter: Option<SeatbeltFilter>,
}

impl SeatbeltRule {
    fn allow(operations: &[&'static str]) -> Self {
        Self {
            operations: operations.to_vec(),
            filter: None,
        }
    }

    fn allow_literal(operations: &[&'static str], path: &Path) -> Self {
        Self {
            operations: operations.to_vec(),
            filter: Some(SeatbeltFilter::Literal(path.to_path_buf())),
        }
    }

    fn allow_subpath(operations: &[&'static str], path: &Path) -> Self {
        Self {
            operations: operations.to_vec(),
            filter: Some(SeatbeltFilter::Subpath(path.to_path_buf())),
        }
    }

    fn allow_regex(operations: &[&'static str], pattern: &'static str) -> Self {
        Self {
            operations: operations.to_vec(),
            filter: Some(SeatbeltFilter::Regex(pattern)),
        }
    }

    fn render(&self) -> String {
        let mut out = String::from("(allow");

        for op in &self.operations {
            out.push(' ');
            out.push_str(op);
        }

        if let Some(filter) = &self.filter {
            out.push(' ');
            match filter {
                SeatbeltFilter::Literal(path) => {
                    out.push_str("(literal \"");
                    out.push_str(&escape_sb_string(path));
                    out.push_str("\")");
                }
                SeatbeltFilter::Subpath(path) => {
                    out.push_str("(subpath \"");
                    out.push_str(&escape_sb_string(path));
                    out.push_str("\")");
                }
                SeatbeltFilter::Regex(pattern) => {
                    out.push_str("(regex \"");
                    out.push_str(pattern);
                    out.push_str("\")");
                }
            }
        }

        out.push_str(")\n");
        out
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct SeatbeltPolicy {
    rules: Vec<SeatbeltRule>,
}

impl SeatbeltPolicy {
    pub(super) fn from_parts(
        allow_network: bool,
        traversal_paths: &[PathBuf],
        read_only_paths: &[PathBuf],
        read_write_paths: &[PathBuf],
    ) -> Self {
        let mut rules = vec![
            SeatbeltRule::allow(&["process*"]),
            SeatbeltRule::allow(&["pseudo-tty"]),
            SeatbeltRule::allow_literal(
                &["file-read*", "file-write*", "file-ioctl"],
                Path::new("/dev/tty"),
            ),
            SeatbeltRule::allow_regex(
                &["file-read*", "file-write*", "file-ioctl"],
                "^/dev/ttys[0-9]*",
            ),
        ];

        for path in traversal_paths {
            rules.push(SeatbeltRule::allow_literal(&["file-read-metadata"], path));
        }

        for path in read_only_paths {
            rules.push(SeatbeltRule::allow_literal(&["file-read*"], path));
            rules.push(SeatbeltRule::allow_subpath(&["file-read*"], path));
            rules.push(SeatbeltRule::allow_literal(&["process-exec"], path));
            rules.push(SeatbeltRule::allow_subpath(&["process-exec"], path));
            rules.push(SeatbeltRule::allow_literal(&["file-map-executable"], path));
            rules.push(SeatbeltRule::allow_subpath(&["file-map-executable"], path));
        }

        for path in read_write_paths {
            rules.push(SeatbeltRule::allow_literal(&["file-read*"], path));
            rules.push(SeatbeltRule::allow_subpath(&["file-read*"], path));
            rules.push(SeatbeltRule::allow_literal(&["file-write*"], path));
            rules.push(SeatbeltRule::allow_subpath(&["file-write*"], path));
            rules.push(SeatbeltRule::allow_literal(&["file-ioctl"], path));
            rules.push(SeatbeltRule::allow_subpath(&["file-ioctl"], path));
            rules.push(SeatbeltRule::allow_literal(&["process-exec"], path));
            rules.push(SeatbeltRule::allow_subpath(&["process-exec"], path));
            rules.push(SeatbeltRule::allow_literal(&["file-map-executable"], path));
            rules.push(SeatbeltRule::allow_subpath(&["file-map-executable"], path));
        }

        if allow_network {
            rules.push(SeatbeltRule::allow(&["network*"]));
        }

        Self { rules }
    }
}

impl From<SeatbeltPolicy> for String {
    fn from(policy: SeatbeltPolicy) -> Self {
        let mut out = String::new();
        out.push_str("(version 1)\n");
        out.push_str("(deny default)\n");
        out.push_str("(import \"system.sb\")\n");

        for rule in policy.rules {
            out.push_str(&rule.render());
        }

        out
    }
}

fn escape_sb_string(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}
