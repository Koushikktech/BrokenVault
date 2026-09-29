use crate::core::errors::PathError;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

pub fn validate_relative_path(path: &str) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::EmptyPath);
    }
    if path.contains('\\') {
        return Err(PathError::BackslashSeparator(path.to_string()));
    }
    if path.contains('\0') || path.chars().any(|c| c.is_control()) {
        return Err(PathError::InvalidCharacter(path.to_string()));
    }
    if path.starts_with('/') {
        return Err(PathError::AbsolutePath(path.to_string()));
    }
    if let Some(first) = path.chars().next() {
        if first.is_ascii_alphabetic() && path.len() >= 2 && path.chars().nth(1) == Some(':') {
            return Err(PathError::WindowsPrefix(path.to_string()));
        }
    }

    let parts: Vec<&str> = path.split('/').collect();
    for part in parts {
        if part.is_empty() {
            return Err(PathError::EmptyPath);
        }
        if part == "." || part == ".." {
            return Err(PathError::TraversalComponent(path.to_string()));
        }
    }

    Ok(())
}

pub fn validate_path_set<'a, I>(paths: I) -> Result<(), PathError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = HashSet::new();
    let mut dir_prefixes = HashSet::new();
    let mut collected = Vec::new();

    for path in paths {
        validate_relative_path(path)?;
        if !seen.insert(path) {
            return Err(PathError::DuplicatePath(path.to_string()));
        }
        collected.push(path);

        let mut current = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for i in 0..parts.len().saturating_sub(1) {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(parts[i]);
            dir_prefixes.insert(current.clone());
        }
    }

    for path in collected {
        if dir_prefixes.contains(path) {
            return Err(PathError::FileDirectoryConflict(path.to_string()));
        }
    }

    Ok(())
}

pub fn resolve_under_root(root: &Path, rel_path: &str) -> Result<PathBuf, PathError> {
    validate_relative_path(rel_path)?;

    let mut dest = PathBuf::from(root);
    for component in Path::new(rel_path).components() {
        match component {
            Component::Normal(segment) => dest.push(segment),
            _ => return Err(PathError::TraversalComponent(rel_path.to_string())),
        }
    }

    let normalized_root = normalize_path(root);
    let normalized_dest = normalize_path(&dest);

    if !normalized_dest.starts_with(&normalized_root) {
        return Err(PathError::DestinationEscape(rel_path.to_string()));
    }

    Ok(dest)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut stack = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                stack.push(Component::Prefix(prefix).as_os_str().to_os_string())
            }
            Component::RootDir => stack.push(Component::RootDir.as_os_str().to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = stack.last() {
                    if last != Component::RootDir.as_os_str() {
                        stack.pop();
                    }
                }
            }
            Component::Normal(segment) => stack.push(segment.to_os_string()),
        }
    }
    let mut result = PathBuf::new();
    for seg in stack {
        result.push(seg);
    }
    result
}
