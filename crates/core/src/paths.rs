use std::path::{Component, Path};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathSafetyError {
    #[error("path must be absolute")]
    NotAbsolute,
    #[error("path traversal is not allowed")]
    Traversal,
}

pub fn validate_absolute_no_traversal(path: &Path) -> Result<(), PathSafetyError> {
    if !path.is_absolute() {
        return Err(PathSafetyError::NotAbsolute);
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PathSafetyError::Traversal);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_paths() {
        assert_eq!(
            validate_absolute_no_traversal(Path::new("../snapshot")),
            Err(PathSafetyError::NotAbsolute)
        );
    }
}
