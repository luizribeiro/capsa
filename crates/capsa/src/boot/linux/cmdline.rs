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

impl From<&str> for CmdlineArg {
    fn from(s: &str) -> Self {
        Self::parse(s)
    }
}

impl From<String> for CmdlineArg {
    fn from(s: String) -> Self {
        Self::parse(s)
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

#[derive(Debug, Clone, Default)]
pub struct KernelCmdline {
    args: Vec<CmdlineArg>,
    override_value: Option<String>,
}

impl KernelCmdline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arg(&mut self, arg: impl Into<CmdlineArg>) -> &mut Self {
        let arg = arg.into();
        let key = arg.key();
        self.args.retain(|a| a.key() != key);
        self.args.push(arg);
        self
    }

    pub fn args(&mut self, args: impl IntoIterator<Item = impl Into<CmdlineArg>>) -> &mut Self {
        for arg in args {
            self.arg(arg);
        }
        self
    }

    pub fn root(&mut self, device: &str) -> &mut Self {
        self.arg(CmdlineArg::kv("root", device))
    }

    pub fn console(&mut self, device: &str) -> &mut Self {
        self.arg(CmdlineArg::kv("console", device))
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
            self.arg(arg.clone());
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

    #[test]
    fn parse_key_value() {
        let arg = CmdlineArg::parse("root=/dev/vda");
        assert_eq!(
            arg,
            CmdlineArg::KeyValue {
                key: "root".into(),
                value: "/dev/vda".into()
            }
        );
    }

    #[test]
    fn parse_flag() {
        let arg = CmdlineArg::parse("ro");
        assert_eq!(arg, CmdlineArg::Flag("ro".into()));
    }

    #[test]
    fn cmdline_deduplication() {
        let mut cmdline = KernelCmdline::new();
        cmdline.arg("console=ttyS0");
        cmdline.arg("console=hvc0");
        assert_eq!(cmdline.build(), "console=hvc0");
    }

    #[test]
    fn cmdline_override() {
        let mut cmdline = KernelCmdline::new();
        cmdline.arg("console=ttyS0");
        cmdline.override_with("custom cmdline here");
        assert_eq!(cmdline.build(), "custom cmdline here");
    }

    #[test]
    fn cmdline_merge() {
        let mut base = KernelCmdline::new();
        base.arg("console=ttyS0");
        base.arg("ro");

        let mut extra = KernelCmdline::new();
        extra.arg("console=hvc0");
        extra.arg("quiet");

        base.merge(&extra);
        assert_eq!(cmdline_to_sorted(&base), "console=hvc0 quiet ro");
    }

    fn cmdline_to_sorted(cmdline: &KernelCmdline) -> String {
        let built = cmdline.build();
        let mut parts: Vec<_> = built.split_whitespace().collect();
        parts.sort();
        parts.join(" ")
    }

    #[test]
    fn cmdline_contains_and_get() {
        let mut cmdline = KernelCmdline::new();
        cmdline.arg("root=/dev/vda");
        cmdline.arg("ro");

        assert!(cmdline.contains("root"));
        assert!(cmdline.contains("ro"));
        assert!(!cmdline.contains("quiet"));
        assert_eq!(cmdline.get("root"), Some("/dev/vda"));
        assert_eq!(cmdline.get("ro"), None);
    }

    #[test]
    fn cmdline_remove() {
        let mut cmdline = KernelCmdline::new();
        cmdline.arg("console=ttyS0");
        cmdline.arg("ro");

        let removed = cmdline.remove("console");
        assert!(removed.is_some());
        assert!(!cmdline.contains("console"));
        assert!(cmdline.contains("ro"));
    }
}
