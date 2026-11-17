//! Compiler and library settings for the project: the solc version (Auto or
//! locked in `.chasm/chasm.toml`) and Solidity libraries installed once per
//! machine under `~/.chasm/lib/<name>@<version>` and remapped per project.
use crate::config::{global_lib_dir, ChasmConfig};
use std::path::{Path, PathBuf};

/// A library Chasm knows how to install.
pub struct Library {
    pub id: &'static str,
    pub title: &'static str,
    pub repo: &'static str,
    /// Remapping prefix used in imports.
    pub prefix: &'static str,
    /// Folder inside the checkout that the prefix points at.
    pub source: &'static str,
    /// Tag to install when the user gives no version.
    pub default_tag: &'static str,
}

pub const LIBRARIES: [Library; 5] = [
    Library { id: "openzeppelin-contracts", title: "OpenZeppelin Contracts", repo: "https://github.com/OpenZeppelin/openzeppelin-contracts", prefix: "@openzeppelin/contracts/", source: "contracts/", default_tag: "v5.1.0" },
    Library { id: "openzeppelin-contracts-upgradeable", title: "OpenZeppelin Upgradeable", repo: "https://github.com/OpenZeppelin/openzeppelin-contracts-upgradeable", prefix: "@openzeppelin/contracts-upgradeable/", source: "contracts/", default_tag: "v5.1.0" },
    Library { id: "forge-std", title: "forge-std", repo: "https://github.com/foundry-rs/forge-std", prefix: "forge-std/", source: "src/", default_tag: "v1.9.4" },
    Library { id: "solmate", title: "Solmate", repo: "https://github.com/transmissions11/solmate", prefix: "solmate/", source: "src/", default_tag: "v7" },
    Library { id: "solady", title: "Solady", repo: "https://github.com/Vectorized/solady", prefix: "solady/", source: "src/", default_tag: "v0.0.281" },
];

pub fn library(id: &str) -> Option<&'static Library> {
    LIBRARIES.iter().find(|library| library.id == id)
}

/// Installed versions of a library, newest first by tag text.
pub fn installed(id: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(global_lib_dir()) else { return Vec::new() };
    let prefix = format!("{id}@");
    let mut versions: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_string_lossy().strip_prefix(&prefix).map(str::to_string))
        // A clone still in progress lives in `<tag>.partial`.
        .filter(|tag| !tag.ends_with(".partial"))
        .collect();
    versions.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    versions
}

fn version_key(tag: &str) -> Vec<u64> {
    tag.trim_start_matches('v').split(|c: char| !c.is_ascii_digit()).filter_map(|part| part.parse().ok()).collect()
}

fn checkout(id: &str, tag: &str) -> PathBuf {
    global_lib_dir().join(format!("{id}@{tag}"))
}

/// Tags are passed to git; allow only plain version text.
fn valid_tag(tag: &str) -> bool {
    !tag.is_empty() && tag.len() <= 40 && !tag.starts_with('-') && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Shallow clone of one tag into the global library folder. Blocking.
pub fn install(id: &str, tag: &str) -> Result<PathBuf, String> {
    let library = library(id).ok_or("Unknown library.")?;
    let tag = if tag.trim().is_empty() { library.default_tag } else { tag.trim() };
    if !valid_tag(tag) {
        return Err("Enter a release tag such as v5.1.0.".into());
    }
    let target = checkout(id, tag);
    if target.join(library.source).is_dir() {
        return Ok(target);
    }
    std::fs::create_dir_all(global_lib_dir()).map_err(|error| error.to_string())?;
    let partial = target.with_extension("partial");
    let _ = std::fs::remove_dir_all(&partial);
    let mut command = std::process::Command::new("git");
    command.args(["clone", "--depth", "1", "--branch", tag, "--quiet", library.repo]).arg(&partial);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let output = command.output().map_err(|error| format!("Could not run git ({error}). Install Git to download libraries."))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&partial);
        let detail = String::from_utf8_lossy(&output.stderr);
        let line = detail.lines().find(|line| !line.trim().is_empty()).unwrap_or("git failed").trim();
        return Err(format!("Could not download {} {tag}: {line}", library.title));
    }
    std::fs::rename(&partial, &target).map_err(|error| error.to_string())?;
    Ok(target)
}

/// Point the project's remapping for the library at an installed version.
pub fn use_in_project(workspace: &Path, id: &str, tag: &str) -> Result<String, String> {
    let library = library(id).ok_or("Unknown library.")?;
    let source = checkout(id, tag).join(library.source);
    if !source.is_dir() {
        return Err(format!("{} {tag} is not installed.", library.title));
    }
    let path = source.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
    let path = if path.ends_with('/') { path } else { format!("{path}/") };
    let remapping = format!("{}={path}", library.prefix);
    ChasmConfig::update(workspace, |config| {
        config.remappings.retain(|item| !item.starts_with(&format!("{}=", library.prefix)));
        config.remappings.push(remapping.clone());
    })
    .map_err(|error| error.to_string())?;
    Ok(remapping)
}

pub fn remove_from_project(workspace: &Path, id: &str) -> Result<(), String> {
    let library = library(id).ok_or("Unknown library.")?;
    ChasmConfig::update(workspace, |config| config.remappings.retain(|item| !item.starts_with(&format!("{}=", library.prefix))))
        .map_err(|error| error.to_string())
}

/// The version a project remapping points at, if it is a Chasm-managed one.
pub fn used_version(workspace: &Path, id: &str) -> Option<String> {
    let library = library(id)?;
    let config = ChasmConfig::load(workspace).ok()?;
    let remapping = config.remappings.iter().find(|item| item.starts_with(&format!("{}=", library.prefix)))?;
    let marker = format!("{id}@");
    let rest = &remapping[remapping.find(&marker)? + marker.len()..];
    Some(rest.split('/').next()?.to_string())
        .or_else(|| Some("custom path".into()))
}

/// Imports in the workspace sources that no remapping or file resolves, by
/// prefix, so the UI can offer the matching library.
pub fn missing_imports(workspace: &Path) -> Vec<&'static Library> {
    let remappings = ChasmConfig::load(workspace).map(|config| config.remappings).unwrap_or_default();
    let mut missing = Vec::new();
    for entry in walkdir::WalkDir::new(workspace)
        .max_depth(6)
        .into_iter()
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some("lib" | "node_modules" | ".chasm" | "out" | "cache" | ".git")))
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sol"))
    {
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        for library in &LIBRARIES {
            let used = text.lines().any(|line| line.trim_start().starts_with("import") && line.contains(&format!("\"{}", library.prefix)));
            let mapped = remappings.iter().any(|item| item.starts_with(library.prefix));
            let vendored = workspace.join("lib").join(library.id).is_dir();
            if used && !mapped && !vendored && !missing.iter().any(|known: &&Library| known.id == library.id) {
                missing.push(library);
            }
        }
    }
    missing
}

/// Installed solc versions, newest first.
pub fn installed_solc() -> Vec<String> {
    let mut versions = foundry_compilers::solc::Solc::installed_versions();
    versions.sort();
    versions.reverse();
    versions.into_iter().map(|version| version.to_string()).collect()
}

/// The locked solc version, or empty for Auto.
pub fn locked_solc(workspace: &Path) -> String {
    ChasmConfig::load(workspace).map(|config| config.compiler.solc).unwrap_or_default()
}

pub fn lock_solc(workspace: &Path, version: &str) -> Result<(), String> {
    let version = version.trim();
    if !version.is_empty() {
        semver::Version::parse(version).map_err(|_| "Enter a version like 0.8.24, or leave it empty for Auto.".to_string())?;
    }
    ChasmConfig::update(workspace, |config| config.compiler.solc = version.to_string()).map_err(|error| error.to_string())
}

/// The `pragma solidity` line of each workspace source, for the compiler view.
pub fn pragmas(workspace: &Path) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for entry in walkdir::WalkDir::new(workspace)
        .max_depth(6)
        .into_iter()
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some("lib" | "node_modules" | ".chasm" | "out" | "cache" | ".git")))
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sol"))
        .take(200)
    {
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let pragma = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("pragma solidity"))
            .map(|line| line.trim_start_matches("pragma solidity").trim().trim_end_matches(';').trim().to_string())
            .unwrap_or_else(|| "no pragma".into());
        let name = entry.path().strip_prefix(workspace).unwrap_or(entry.path()).to_string_lossy().replace('\\', "/");
        rows.push((name, pragma));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_plain_and_versions_sort_numerically() {
        assert!(valid_tag("v5.1.0"));
        assert!(!valid_tag("--upload-pack=evil"));
        assert!(!valid_tag("v1;rm"));
        let mut tags = vec!["v4.9.6".to_string(), "v10.0.0".to_string(), "v5.1.0".to_string()];
        tags.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
        assert_eq!(tags, ["v10.0.0", "v5.1.0", "v4.9.6"]);
    }

    #[test]
    fn project_settings_lock_solc_and_report_missing_imports() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("A.sol"), "pragma solidity ^0.8.20;\nimport \"@openzeppelin/contracts/token/ERC20/ERC20.sol\";\nimport \"solady/utils/LibString.sol\";\n").unwrap();
        assert_eq!(pragmas(workspace.path()), vec![("A.sol".to_string(), "^0.8.20".to_string())]);
        let missing: Vec<_> = missing_imports(workspace.path()).iter().map(|library| library.id).collect();
        assert_eq!(missing, vec!["openzeppelin-contracts", "solady"]);
        assert!(lock_solc(workspace.path(), "0.8.x").is_err());
        lock_solc(workspace.path(), "0.8.24").unwrap();
        assert_eq!(locked_solc(workspace.path()), "0.8.24");
        let saved = std::fs::read_to_string(workspace.path().join(".chasm/chasm.toml")).unwrap();
        assert!(saved.contains("[compiler]") && saved.contains("solc = \"0.8.24\""), "{saved}");
        lock_solc(workspace.path(), "").unwrap();
        assert!(!std::fs::read_to_string(workspace.path().join(".chasm/chasm.toml")).unwrap().contains("[compiler]"));
        ChasmConfig::update(workspace.path(), |config| config.remappings.push("solady/=/x/solady@v0.0.1/src/".into())).unwrap();
        assert_eq!(used_version(workspace.path(), "solady").as_deref(), Some("v0.0.1"));
        let missing: Vec<_> = missing_imports(workspace.path()).iter().map(|library| library.id).collect();
        assert_eq!(missing, vec!["openzeppelin-contracts"]);
        remove_from_project(workspace.path(), "solady").unwrap();
        assert_eq!(used_version(workspace.path(), "solady"), None);
    }
}
