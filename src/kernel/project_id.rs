pub use crate::sync::state::ProjectId;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_is_sixteen_hex() {
        let dir = std::env::temp_dir();
        let id = ProjectId::for_path(&dir).unwrap();
        assert_eq!(id.as_str().len(), 16);
    }

    #[test]
    fn project_id_is_deterministic_for_same_canonical_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = ProjectId::for_path(dir.path()).unwrap();
        let second = ProjectId::for_path(dir.path()).unwrap();
        assert_eq!(first, second, "the path hash must be stable for one path");
    }
}
