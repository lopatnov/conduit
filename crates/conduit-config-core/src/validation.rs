/// A single config validation failure: the config path that's wrong, and why.
#[derive(Debug, PartialEq, Clone, serde::Serialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_path_and_message() {
        let e = ValidationError::new("sites[0].port", "port is required");
        assert_eq!(e.path, "sites[0].port");
        assert_eq!(e.message, "port is required");
    }

    #[test]
    fn equality_compares_both_fields() {
        let a = ValidationError::new("x", "y");
        let b = ValidationError::new("x", "y");
        let c = ValidationError::new("x", "z");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
