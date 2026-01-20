use anyhow::Result;
use foundry_compilers::artifacts::ConfigurableContractArtifact;
use foundry_compilers::{Project, ProjectPathsConfig};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::process::Command;
use walkdir::WalkDir;

pub struct Compiler {
    project: Project,
    root: PathBuf,
}

impl Compiler {
    pub fn new(root: PathBuf) -> Result<Self> {
        // Attempt to find contracts folder
        let contracts_dir = root.join("contracts");
        let src_path = if contracts_dir.exists() {
            contracts_dir
        } else {
            root.clone() // Fallback to root if contracts dir missing
        };

        let paths = ProjectPathsConfig::builder()
            .root(&root)
            .sources(&src_path) // Explicitly set sources
            .build()?;
        
        let project = Project::builder()
            .paths(paths)
            .ephemeral()
            .no_artifacts()
            .build(Default::default())?;

        Ok(Self { project, root })
    }

    pub fn compile(&self) -> Result<Vec<(String, ConfigurableContractArtifact)>> {
        ensure_solc_version(&self.root);
        let output = self.project.compile()?;
        if output.has_compiler_errors() {
            return Err(anyhow::anyhow!("Compilation failed"));
        }
        
        Ok(output.into_artifacts()
            .map(|(id, artifact)| (id.name, artifact))
            .collect())
    }

    pub fn compile_to_json(&self) -> Result<String> {
        let artifacts = self.compile()?;
        
        #[derive(serde::Serialize)]
        struct CompileSuccess {
             r#type: String,
             contracts: Vec<ContractData>,
        }
        
        #[derive(serde::Serialize)]
        struct ContractData {
             name: String,
             artifact: ConfigurableContractArtifact, 
        }

        let contracts_data: Vec<ContractData> = artifacts.into_iter().map(|(name, artifact)| {
             ContractData { name, artifact }
        }).collect();

        let msg = CompileSuccess {
             r#type: "compile_success".to_string(),
             contracts: contracts_data,
        };

        Ok(serde_json::to_string(&msg)?)
    }
}

static SOLC_CACHE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn ensure_solc_version(root: &Path) {
    let version = match detect_solc_version(root) {
        Some(v) => v,
        None => return,
    };

    let cache = SOLC_CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap();
        if guard.as_ref() == Some(&version) {
            return;
        }
    }

    let install_status = Command::new("svm")
        .arg("install")
        .arg(&version)
        .status();

    if let Ok(status) = install_status {
        if status.success() {
            let _ = Command::new("svm").arg("use").arg(&version).status();
            if let Ok(mut guard) = cache.lock() {
                *guard = Some(version);
            }
        }
    }
}

fn detect_solc_version(root: &Path) -> Option<String> {
    let mut best: Option<(u32, u32, u32)> = None;

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("sol") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            for line in content.lines().take(20) {
                if !line.contains("pragma solidity") {
                    continue;
                }
                for token in extract_versions(line) {
                    if let Some(parsed) = parse_version(&token) {
                        if best.map_or(true, |b| parsed > b) {
                            best = Some(parsed);
                        }
                    }
                }
            }
        }
    }

    best.map(|(a, b, c)| format!("{}.{}.{}", a, b, c))
}

fn extract_versions(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(cur.clone());
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter().filter(|s| s.matches('.').count() == 2).collect()
}

fn parse_version(value: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let major = parts[0].parse().ok()?;
    let minor = parts[1].parse().ok()?;
    let patch = parts[2].parse().ok()?;
    Some((major, minor, patch))
}