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
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn allow(&mut self, operations: &[&'static str]) {
        self.rules.push(SeatbeltRule::allow(operations));
    }

    pub(super) fn allow_literal(&mut self, operations: &[&'static str], path: &Path) {
        self.rules
            .push(SeatbeltRule::allow_literal(operations, path));
    }

    pub(super) fn allow_subpath(&mut self, operations: &[&'static str], path: &Path) {
        self.rules
            .push(SeatbeltRule::allow_subpath(operations, path));
    }

    pub(super) fn allow_regex(&mut self, operations: &[&'static str], pattern: &'static str) {
        self.rules
            .push(SeatbeltRule::allow_regex(operations, pattern));
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
