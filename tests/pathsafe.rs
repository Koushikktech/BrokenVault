use brokenvault::core::errors::PathError;
use brokenvault::core::pathsafe::{resolve_under_root, validate_path_set, validate_relative_path};
use std::path::Path;

#[test]
fn test_valid_relative_paths() {
    assert!(validate_relative_path("file.txt").is_ok());
    assert!(validate_relative_path("dir/file.txt").is_ok());
    assert!(validate_relative_path("nested/deep/directory/structure/file.bin").is_ok());
    assert!(validate_relative_path("dir").is_ok());
}

#[test]
fn test_reject_empty_path() {
    assert_eq!(validate_relative_path(""), Err(PathError::EmptyPath));
    assert_eq!(validate_relative_path("a//b"), Err(PathError::EmptyPath));
    assert_eq!(validate_relative_path("a/b/"), Err(PathError::EmptyPath));
}

#[test]
fn test_reject_absolute_path() {
    assert!(matches!(
        validate_relative_path("/etc/passwd"),
        Err(PathError::AbsolutePath(_))
    ));
    assert!(matches!(
        validate_relative_path("/root"),
        Err(PathError::AbsolutePath(_))
    ));
}

#[test]
fn test_reject_traversal() {
    assert!(matches!(
        validate_relative_path("../escape"),
        Err(PathError::TraversalComponent(_))
    ));
    assert!(matches!(
        validate_relative_path("dir/../escape"),
        Err(PathError::TraversalComponent(_))
    ));
    assert!(matches!(
        validate_relative_path("./local"),
        Err(PathError::TraversalComponent(_))
    ));
    assert!(matches!(
        validate_relative_path("dir/./file"),
        Err(PathError::TraversalComponent(_))
    ));
}

#[test]
fn test_reject_windows_prefix_and_backslash() {
    assert!(matches!(
        validate_relative_path("C:/autoexec.bat"),
        Err(PathError::WindowsPrefix(_))
    ));
    assert!(matches!(
        validate_relative_path("dir\\file.txt"),
        Err(PathError::BackslashSeparator(_))
    ));
}

#[test]
fn test_reject_invalid_characters() {
    assert!(matches!(
        validate_relative_path("null\0byte"),
        Err(PathError::InvalidCharacter(_))
    ));
    assert!(matches!(
        validate_relative_path("control\x07char"),
        Err(PathError::InvalidCharacter(_))
    ));
}

#[test]
fn test_validate_path_set_duplicates() {
    let paths = vec!["alpha/file.txt", "beta/file.txt", "alpha/file.txt"];
    assert!(matches!(
        validate_path_set(paths),
        Err(PathError::DuplicatePath(_))
    ));
}

#[test]
fn test_validate_path_set_file_dir_conflict() {
    let paths = vec!["docs/notes", "docs/notes/details.txt"];
    assert!(matches!(
        validate_path_set(paths),
        Err(PathError::FileDirectoryConflict(_))
    ));
}

#[test]
fn test_resolve_under_root_success() {
    let root = Path::new("/tmp/test_vault_restore");
    let resolved = resolve_under_root(root, "docs/sub/file.txt").unwrap();
    assert_eq!(resolved, root.join("docs/sub/file.txt"));
}

#[test]
fn test_resolve_under_root_reject_escape() {
    let root = Path::new("/tmp/test_vault_restore");
    assert!(resolve_under_root(root, "../outside").is_err());
}
