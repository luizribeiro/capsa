use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdlineArg {
    KeyValue { key: String, value: String },
    Flag(String),
}

impl CmdlineArg {
    pub fn parse(s: impl AsRef<str>) -> Self {
        let s = s.as_ref();
        if let Some((key, value)) = s.split_once('=') {
            Self::KeyValue {
                key: key.to_string(),
                value: value.to_string(),
            }
        } else {
            Self::Flag(s.to_string())
        }
    }

    pub fn kv(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self::KeyValue {
            key: key.into(),
            value: value.into(),
        }
    }

    pub fn flag(name: impl Into<String>) -> Self {
        Self::Flag(name.into())
    }

    pub fn key(&self) -> &str {
        match self {
            Self::KeyValue { key, .. } => key,
            Self::Flag(name) => name,
        }
    }
}

impl fmt::Display for CmdlineArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyValue { key, value } => write!(f, "{key}={value}"),
            Self::Flag(name) => write!(f, "{name}"),
        }
    }
}

/// Builder for Linux kernel command line arguments.
///
/// Use the builder's `cmdline_arg` and `cmdline_flag` methods for typical usage.
/// This type is for advanced scenarios requiring direct manipulation.
#[derive(Debug, Clone, Default)]
pub struct KernelCmdline {
    args: Vec<CmdlineArg>,
    override_value: Option<String>,
}

impl KernelCmdline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arg(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        let key = key.into();
        self.args.retain(|a| a.key() != key);
        self.args.push(CmdlineArg::KeyValue {
            key,
            value: value.into(),
        });
        self
    }

    pub fn flag(&mut self, name: impl Into<String>) -> &mut Self {
        let name = name.into();
        self.args.retain(|a| a.key() != name);
        self.args.push(CmdlineArg::Flag(name));
        self
    }

    pub fn add(&mut self, arg: CmdlineArg) -> &mut Self {
        self.args.retain(|a| a.key() != arg.key());
        self.args.push(arg);
        self
    }

    pub fn root(&mut self, device: &str) -> &mut Self {
        self.arg("root", device)
    }

    pub fn console(&mut self, device: &str) -> &mut Self {
        self.arg("console", device)
    }

    pub fn override_with(&mut self, cmdline: impl Into<String>) -> &mut Self {
        self.override_value = Some(cmdline.into());
        self
    }

    pub fn contains(&self, key: &str) -> bool {
        self.args.iter().any(|a| a.key() == key)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.args.iter().find_map(|a| match a {
            CmdlineArg::KeyValue { key: k, value } if k == key => Some(value.as_str()),
            _ => None,
        })
    }

    pub fn remove(&mut self, key: &str) -> Option<CmdlineArg> {
        if let Some(pos) = self.args.iter().position(|a| a.key() == key) {
            Some(self.args.remove(pos))
        } else {
            None
        }
    }

    pub fn build(&self) -> String {
        if let Some(override_val) = &self.override_value {
            return override_val.clone();
        }

        self.args
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn merge(&mut self, other: &KernelCmdline) -> &mut Self {
        for arg in &other.args {
            self.add(arg.clone());
        }
        self
    }

    pub fn is_overridden(&self) -> bool {
        self.override_value.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod cmdline_arg {
        use super::*;

        #[test]
        fn parse_key_value() {
            let arg = CmdlineArg::parse("root=/dev/vda");
            assert_eq!(
                arg,
                CmdlineArg::KeyValue {
                    key: "root".to_string(),
                    value: "/dev/vda".to_string()
                }
            );
        }

        #[test]
        fn parse_flag() {
            let arg = CmdlineArg::parse("quiet");
            assert_eq!(arg, CmdlineArg::Flag("quiet".to_string()));
        }

        #[test]
        fn parse_key_value_with_multiple_equals() {
            let arg = CmdlineArg::parse("init=/bin/sh -c echo=test");
            assert_eq!(
                arg,
                CmdlineArg::KeyValue {
                    key: "init".to_string(),
                    value: "/bin/sh -c echo=test".to_string()
                }
            );
        }

        #[test]
        fn kv_constructor() {
            let arg = CmdlineArg::kv("console", "ttyS0");
            assert_eq!(
                arg,
                CmdlineArg::KeyValue {
                    key: "console".to_string(),
                    value: "ttyS0".to_string()
                }
            );
        }

        #[test]
        fn flag_constructor() {
            let arg = CmdlineArg::flag("ro");
            assert_eq!(arg, CmdlineArg::Flag("ro".to_string()));
        }

        #[test]
        fn key_returns_key_for_key_value() {
            let arg = CmdlineArg::kv("root", "/dev/vda");
            assert_eq!(arg.key(), "root");
        }

        #[test]
        fn key_returns_name_for_flag() {
            let arg = CmdlineArg::flag("quiet");
            assert_eq!(arg.key(), "quiet");
        }

        #[test]
        fn display_key_value() {
            let arg = CmdlineArg::kv("root", "/dev/vda");
            assert_eq!(arg.to_string(), "root=/dev/vda");
        }

        #[test]
        fn display_flag() {
            let arg = CmdlineArg::flag("quiet");
            assert_eq!(arg.to_string(), "quiet");
        }
    }

    mod kernel_cmdline {
        use super::*;

        #[test]
        fn new_creates_empty_cmdline() {
            let cmdline = KernelCmdline::new();
            assert_eq!(cmdline.build(), "");
        }

        #[test]
        fn arg_adds_key_value() {
            let mut cmdline = KernelCmdline::new();
            cmdline.arg("root", "/dev/vda");
            assert_eq!(cmdline.build(), "root=/dev/vda");
        }

        #[test]
        fn flag_adds_flag() {
            let mut cmdline = KernelCmdline::new();
            cmdline.flag("quiet");
            assert_eq!(cmdline.build(), "quiet");
        }

        #[test]
        fn multiple_args_joined_with_space() {
            let mut cmdline = KernelCmdline::new();
            cmdline
                .arg("root", "/dev/vda")
                .flag("quiet")
                .arg("console", "ttyS0");
            assert_eq!(cmdline.build(), "root=/dev/vda quiet console=ttyS0");
        }

        #[test]
        fn arg_replaces_existing_key() {
            let mut cmdline = KernelCmdline::new();
            cmdline.arg("root", "/dev/vda").arg("root", "/dev/vdb");
            assert_eq!(cmdline.build(), "root=/dev/vdb");
        }

        #[test]
        fn flag_replaces_existing_flag() {
            let mut cmdline = KernelCmdline::new();
            cmdline.flag("quiet").flag("quiet");
            assert_eq!(cmdline.build(), "quiet");
        }

        #[test]
        fn root_sets_root_arg() {
            let mut cmdline = KernelCmdline::new();
            cmdline.root("/dev/vda");
            assert_eq!(cmdline.build(), "root=/dev/vda");
        }

        #[test]
        fn console_sets_console_arg() {
            let mut cmdline = KernelCmdline::new();
            cmdline.console("hvc0");
            assert_eq!(cmdline.build(), "console=hvc0");
        }

        #[test]
        fn override_with_replaces_entire_cmdline() {
            let mut cmdline = KernelCmdline::new();
            cmdline
                .arg("root", "/dev/vda")
                .flag("quiet")
                .override_with("custom=cmdline");
            assert_eq!(cmdline.build(), "custom=cmdline");
        }

        #[test]
        fn is_overridden_false_by_default() {
            let cmdline = KernelCmdline::new();
            assert!(!cmdline.is_overridden());
        }

        #[test]
        fn is_overridden_true_after_override() {
            let mut cmdline = KernelCmdline::new();
            cmdline.override_with("test");
            assert!(cmdline.is_overridden());
        }

        #[test]
        fn contains_returns_true_for_existing_key() {
            let mut cmdline = KernelCmdline::new();
            cmdline.arg("root", "/dev/vda");
            assert!(cmdline.contains("root"));
        }

        #[test]
        fn contains_returns_false_for_missing_key() {
            let cmdline = KernelCmdline::new();
            assert!(!cmdline.contains("root"));
        }

        #[test]
        fn contains_works_for_flags() {
            let mut cmdline = KernelCmdline::new();
            cmdline.flag("quiet");
            assert!(cmdline.contains("quiet"));
        }

        #[test]
        fn get_returns_value_for_key_value() {
            let mut cmdline = KernelCmdline::new();
            cmdline.arg("root", "/dev/vda");
            assert_eq!(cmdline.get("root"), Some("/dev/vda"));
        }

        #[test]
        fn get_returns_none_for_flag() {
            let mut cmdline = KernelCmdline::new();
            cmdline.flag("quiet");
            assert_eq!(cmdline.get("quiet"), None);
        }

        #[test]
        fn get_returns_none_for_missing_key() {
            let cmdline = KernelCmdline::new();
            assert_eq!(cmdline.get("root"), None);
        }

        #[test]
        fn remove_removes_and_returns_arg() {
            let mut cmdline = KernelCmdline::new();
            cmdline.arg("root", "/dev/vda").flag("quiet");
            let removed = cmdline.remove("root");
            assert_eq!(
                removed,
                Some(CmdlineArg::KeyValue {
                    key: "root".to_string(),
                    value: "/dev/vda".to_string()
                })
            );
            assert_eq!(cmdline.build(), "quiet");
        }

        #[test]
        fn remove_returns_none_for_missing_key() {
            let mut cmdline = KernelCmdline::new();
            assert_eq!(cmdline.remove("root"), None);
        }

        #[test]
        fn add_accepts_cmdline_arg() {
            let mut cmdline = KernelCmdline::new();
            cmdline.add(CmdlineArg::kv("root", "/dev/vda"));
            assert_eq!(cmdline.build(), "root=/dev/vda");
        }

        #[test]
        fn merge_combines_cmdlines() {
            let mut cmdline1 = KernelCmdline::new();
            cmdline1.arg("root", "/dev/vda").flag("quiet");

            let mut cmdline2 = KernelCmdline::new();
            cmdline2.arg("console", "ttyS0").flag("ro");

            cmdline1.merge(&cmdline2);
            assert!(cmdline1.contains("root"));
            assert!(cmdline1.contains("quiet"));
            assert!(cmdline1.contains("console"));
            assert!(cmdline1.contains("ro"));
        }

        #[test]
        fn merge_overwrites_existing_keys() {
            let mut cmdline1 = KernelCmdline::new();
            cmdline1.arg("root", "/dev/vda");

            let mut cmdline2 = KernelCmdline::new();
            cmdline2.arg("root", "/dev/vdb");

            cmdline1.merge(&cmdline2);
            assert_eq!(cmdline1.get("root"), Some("/dev/vdb"));
        }
    }
}
