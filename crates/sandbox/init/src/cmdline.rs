//! Kernel command line parsing for capsa sandbox.
//!
//! Parses parameters:
//! - `capsa.mount=tag:path` - virtiofs mount specification
//! - `capsa.run=path:arg1:arg2` - main process to run
//! - `capsa.oci=image:arg1:arg2` - OCI container to run (not yet implemented)

use std::fs;
use std::path::Path;

#[derive(Debug)]
pub struct CapsaConfig {
    pub mounts: Vec<MountConfig>,
    pub main_process: Option<MainProcessConfig>,
}

#[derive(Debug)]
pub struct MountConfig {
    pub tag: String,
    pub path: String,
}

#[derive(Debug)]
pub struct MainProcessConfig {
    pub path: String,
    pub args: Vec<String>,
}

#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    InvalidMountPath(String),
    EmptyMountTag,
    EmptyMountPath,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Io(e) => write!(f, "failed to read cmdline: {}", e),
            ParseError::InvalidMountPath(p) => write!(f, "invalid mount path: {}", p),
            ParseError::EmptyMountTag => write!(f, "empty mount tag"),
            ParseError::EmptyMountPath => write!(f, "empty mount path"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e)
    }
}

fn validate_mount_path(path: &str) -> Result<(), ParseError> {
    if path.is_empty() {
        return Err(ParseError::EmptyMountPath);
    }

    let p = Path::new(path);

    if !p.is_absolute() {
        return Err(ParseError::InvalidMountPath(format!(
            "'{}' is not absolute",
            path
        )));
    }

    if path.contains("..") {
        return Err(ParseError::InvalidMountPath(format!(
            "'{}' contains path traversal",
            path
        )));
    }

    Ok(())
}

pub fn parse_cmdline() -> Result<CapsaConfig, ParseError> {
    let cmdline = fs::read_to_string("/proc/cmdline")?;
    parse_cmdline_str(&cmdline)
}

fn parse_cmdline_str(cmdline: &str) -> Result<CapsaConfig, ParseError> {
    let mut mounts = Vec::new();
    let mut main_process = None;

    for part in cmdline.split_whitespace() {
        if let Some(mount_spec) = part.strip_prefix("capsa.mount=") {
            if let Some((tag, path)) = mount_spec.split_once(':') {
                if tag.is_empty() {
                    return Err(ParseError::EmptyMountTag);
                }
                validate_mount_path(path)?;
                mounts.push(MountConfig {
                    tag: tag.to_string(),
                    path: path.to_string(),
                });
            }
        } else if let Some(run_spec) = part.strip_prefix("capsa.run=") {
            let parts: Vec<&str> = run_spec.split(':').collect();
            if !parts.is_empty() {
                main_process = Some(MainProcessConfig {
                    path: parts[0].to_string(),
                    args: parts[1..].iter().map(|s| s.to_string()).collect(),
                });
            }
        }
    }

    Ok(CapsaConfig {
        mounts,
        main_process,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_cmdline() {
        let config = parse_cmdline_str("console=hvc0 panic=-1 quiet").unwrap();
        assert!(config.mounts.is_empty());
        assert!(config.main_process.is_none());
    }

    #[test]
    fn parse_single_mount() {
        let config = parse_cmdline_str("capsa.mount=share0:/mnt/src").unwrap();
        assert_eq!(config.mounts.len(), 1);
        assert_eq!(config.mounts[0].tag, "share0");
        assert_eq!(config.mounts[0].path, "/mnt/src");
    }

    #[test]
    fn parse_multiple_mounts() {
        let config =
            parse_cmdline_str("capsa.mount=share0:/mnt/src capsa.mount=share1:/mnt/data").unwrap();
        assert_eq!(config.mounts.len(), 2);
        assert_eq!(config.mounts[0].tag, "share0");
        assert_eq!(config.mounts[0].path, "/mnt/src");
        assert_eq!(config.mounts[1].tag, "share1");
        assert_eq!(config.mounts[1].path, "/mnt/data");
    }

    #[test]
    fn parse_run_no_args() {
        let config = parse_cmdline_str("capsa.run=/bin/sh").unwrap();
        let main = config.main_process.unwrap();
        assert_eq!(main.path, "/bin/sh");
        assert!(main.args.is_empty());
    }

    #[test]
    fn parse_run_with_args() {
        let config = parse_cmdline_str("capsa.run=/bin/sh:-c:ls").unwrap();
        let main = config.main_process.unwrap();
        assert_eq!(main.path, "/bin/sh");
        assert_eq!(main.args, vec!["-c", "ls"]);
    }

    #[test]
    fn parse_run_with_multiple_args() {
        let config = parse_cmdline_str("capsa.run=/usr/bin/python:/app/main.py:--verbose").unwrap();
        let main = config.main_process.unwrap();
        assert_eq!(main.path, "/usr/bin/python");
        assert_eq!(main.args, vec!["/app/main.py", "--verbose"]);
    }

    #[test]
    fn parse_full_cmdline() {
        let cmdline = "console=hvc0 panic=-1 quiet capsa.mount=share0:/mnt capsa.run=/bin/sh:-c:ls";
        let config = parse_cmdline_str(cmdline).unwrap();

        assert_eq!(config.mounts.len(), 1);
        assert_eq!(config.mounts[0].tag, "share0");
        assert_eq!(config.mounts[0].path, "/mnt");

        let main = config.main_process.unwrap();
        assert_eq!(main.path, "/bin/sh");
        assert_eq!(main.args, vec!["-c", "ls"]);
    }

    #[test]
    fn reject_path_traversal() {
        let result = parse_cmdline_str("capsa.mount=share:/../etc");
        assert!(matches!(result, Err(ParseError::InvalidMountPath(_))));
    }

    #[test]
    fn reject_relative_path() {
        let result = parse_cmdline_str("capsa.mount=share:mnt/data");
        assert!(matches!(result, Err(ParseError::InvalidMountPath(_))));
    }

    #[test]
    fn reject_empty_tag() {
        let result = parse_cmdline_str("capsa.mount=:/mnt");
        assert!(matches!(result, Err(ParseError::EmptyMountTag)));
    }

    #[test]
    fn reject_empty_path() {
        let result = parse_cmdline_str("capsa.mount=share:");
        assert!(matches!(result, Err(ParseError::EmptyMountPath)));
    }

    #[test]
    fn malformed_mount_no_colon_is_ignored() {
        let config = parse_cmdline_str("capsa.mount=invalid").unwrap();
        assert!(config.mounts.is_empty());
    }

    #[test]
    fn duplicate_run_last_wins() {
        let config = parse_cmdline_str("capsa.run=/bin/sh capsa.run=/bin/bash").unwrap();
        let main = config.main_process.unwrap();
        assert_eq!(main.path, "/bin/bash");
    }
}
