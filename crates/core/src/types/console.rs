use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleMode {
    // TODO: rename this as Default as it's actually going to use whatever the backend
    // default behavior is
    #[default]
    Disabled,
    Enabled,
    Stdio,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        assert_eq!(ConsoleMode::default(), ConsoleMode::Disabled);
    }

    #[test]
    fn serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ConsoleMode::Disabled).unwrap(),
            "\"disabled\""
        );
        assert_eq!(
            serde_json::to_string(&ConsoleMode::Enabled).unwrap(),
            "\"enabled\""
        );
        assert_eq!(
            serde_json::to_string(&ConsoleMode::Stdio).unwrap(),
            "\"stdio\""
        );
    }

    #[test]
    fn deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<ConsoleMode>("\"disabled\"").unwrap(),
            ConsoleMode::Disabled
        );
        assert_eq!(
            serde_json::from_str::<ConsoleMode>("\"enabled\"").unwrap(),
            ConsoleMode::Enabled
        );
        assert_eq!(
            serde_json::from_str::<ConsoleMode>("\"stdio\"").unwrap(),
            ConsoleMode::Stdio
        );
    }
}
