use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    None,
    #[default]
    Nat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_nat() {
        assert_eq!(NetworkMode::default(), NetworkMode::Nat);
    }

    #[test]
    fn serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&NetworkMode::None).unwrap(),
            "\"none\""
        );
        assert_eq!(serde_json::to_string(&NetworkMode::Nat).unwrap(), "\"nat\"");
    }

    #[test]
    fn deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<NetworkMode>("\"none\"").unwrap(),
            NetworkMode::None
        );
        assert_eq!(
            serde_json::from_str::<NetworkMode>("\"nat\"").unwrap(),
            NetworkMode::Nat
        );
    }
}
