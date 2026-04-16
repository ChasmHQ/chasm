use anyhow::Result;
use foundry_compilers::artifacts::ConfigurableContractArtifact;
use foundry_compilers::artifacts::output_selection::{ContractOutputSelection, OutputSelection};
use foundry_compilers::artifacts::remappings::Remapping;
use foundry_compilers::{ConfigurableArtifacts, Project, ProjectPathsConfig};
use std::path::PathBuf;
use walkdir::WalkDir;

pub struct Compiler {
    project: Project,
    /// Explicit list of .sol files to compile — excludes lib/, node_modules/, etc.
    /// Using compile_files() instead of compile() avoids recursively compiling
    /// OZ internal test/certora files when local lib/ is present.
    src_files: Vec<PathBuf>,
    /// Project root — kept for potential future use; currently source classification
    /// uses src_files set comparison instead of root-relative path checks.
    #[allow(dead_code)]
    root: PathBuf,
}

impl Compiler {
    /// Standard compiler — no storage layout in output (fast, used for the live workspace).
    pub fn new(root: PathBuf, remappings: Vec<String>) -> Result<Self> {
        Self::new_impl(root, remappings, false)
    }

    /// Like new() but requests storageLayout from solc.
    /// Used on-demand by the storage inspector so the overhead only hits
    /// when the user actually opens that tab, not on every recompile.
    pub fn new_for_storage_layout(root: PathBuf, remappings: Vec<String>) -> Result<Self> {
        Self::new_impl(root, remappings, true)
    }

    fn new_impl(root: PathBuf, remappings: Vec<String>, with_storage_layout: bool) -> Result<Self> {
        // Strip Windows UNC prefix (\\?\) from root so that all derived paths
        // (src_path, lib path, WalkDir entries) are in plain C:\... format.
        // canonicalize() on Windows returns \\?\C:\... which causes solc to
        // construct invalid //?/C:/... paths when resolving relative imports.
        let root = {
            let s = root.to_string_lossy();
            if let Some(stripped) = s.strip_prefix(r"\\?\") {
                PathBuf::from(stripped)
            } else {
                root
            }
        };

        // Attempt to find contracts folder
        let contracts_dir = root.join("contracts");
        let src_path = if contracts_dir.exists() {
            contracts_dir
        } else {
            root.clone() // Fallback to root if contracts dir missing
        };

        // Create a temporary cache directory for this compilation session
        let cache_dir = std::env::temp_dir().join(format!("chasm-cache-{}", std::process::id()));
        let artifacts_dir = std::env::temp_dir().join(format!("chasm-artifacts-{}", std::process::id()));

        // Parse remapping strings menjadi Remapping structs
        // Format: "name=path" (e.g. "@openzeppelin/contracts/=lib/openzeppelin-contracts/contracts/")
        let parsed_remappings: Vec<Remapping> = remappings
            .iter()
            .filter_map(|r| {
                let mut parts = r.splitn(2, '=');
                let name = parts.next()?.to_string();
                let path = parts.next()?.to_string();
                Some(Remapping { context: None, name, path })
            })
            .collect();

        // Build paths configuration explicitly, without relying on foundry.toml
        let paths = ProjectPathsConfig::builder()
            .root(&root)
            .sources(&src_path)
            .lib(root.join("lib"))
            .artifacts(&artifacts_dir)
            .cache(&cache_dir)
            .build_infos(&artifacts_dir.join("build-info"))
            .remappings(parsed_remappings)
            .build()?;

        // ConfigurableArtifacts::default() requests only the basic outputs (abi, bytecode).
        // Enabling storage_layout adds storageLayout to every contract in the compilation
        // unit — including all OZ library contracts — which can make the JSON very large
        // and slow down every recompile. We therefore enable it only when requested.
        //
        // Two separate things must line up here:
        //   1. `ConfigurableArtifacts.additional_values.storage_layout` → controls whether
        //      `contract_to_artifact` copies storage layout from solc output into the
        //      `ConfigurableContractArtifact.storage_layout` field.
        //   2. The project's solc `OutputSelection` → controls whether solc actually
        //      *emits* the storageLayout in its output. `.artifacts()` does NOT propagate
        //      `ConfigurableArtifacts.output_selection()` into the project settings, so we
        //      have to patch the output selection on the built `Project` ourselves.
        let mut artifacts_config = ConfigurableArtifacts::default();
        if with_storage_layout {
            artifacts_config.additional_values.storage_layout = true;
        }

        let mut project = Project::builder()
            .paths(paths)
            .artifacts(artifacts_config)
            .ephemeral()
            .no_artifacts()
            .build(Default::default())?;

        if with_storage_layout {
            let mut outputs: Vec<String> = ContractOutputSelection::basic()
                .iter()
                .map(ToString::to_string)
                .collect();
            outputs.push("storageLayout".to_string());
            project.update_output_selection(|selection| {
                *selection = OutputSelection::common_output_selection(outputs.clone());
            });
        }

        // Collect source files explicitly, excluding lib/, node_modules/, and other
        // dependency directories. This mirrors global OZ behavior — only user-written
        // contracts are compiled; lib/ is used solely for import resolution via remappings.
        let excluded = [
            root.join("lib"),
            root.join("node_modules"),
            root.join("out"),
            root.join("cache"),
            root.join(".chasm"),
        ];
        let src_files: Vec<PathBuf> = WalkDir::new(&src_path)
            .into_iter()
            .filter_entry(|e| {
                let p = e.path();
                !excluded.iter().any(|ex| p.starts_with(ex))
            })
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path().extension().map_or(false, |ext| ext == "sol")
            })
            .map(|e| e.into_path())
            .collect();

        Ok(Self { project, src_files, root })
    }

    pub fn compile(&self) -> Result<Vec<(String, String, ConfigurableContractArtifact)>> {
        let output = self.project.compile_files(self.src_files.clone())?;
        if output.has_compiler_errors() {
            return Err(anyhow::anyhow!("{}", strip_ansi(&output.to_string())));
        }

        // Build a set of normalized src file paths for O(1) lookup.
        // Normalization strips the Windows UNC prefix (\\?\) and converts backslashes to
        // forward slashes so paths from WalkDir (which may carry \\?\) and paths from
        // foundry-compilers ArtifactId.source (which use the remapping path, already
        // stripped of \\?\) compare equal.
        let src_set: std::collections::HashSet<String> = self.src_files
            .iter()
            .map(|p| normalize_path(&p.to_string_lossy()))
            .collect();

        Ok(output.into_artifacts()
            .map(|(id, artifact)| {
                let source_type = if src_set.contains(&normalize_path(&id.source.to_string_lossy())) {
                    "src".to_string()
                } else {
                    "lib".to_string()
                };
                (id.name, source_type, artifact)
            })
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
             source_type: String,
             artifact: ConfigurableContractArtifact,
        }

        let contracts_data: Vec<ContractData> = artifacts.into_iter().map(|(name, source_type, artifact)| {
             ContractData { name, source_type, artifact }
        }).collect();

        let msg = CompileSuccess {
             r#type: "compile_success".to_string(),
             contracts: contracts_data,
        };

        Ok(serde_json::to_string(&msg)?)
    }
}

/// Normalize a file path for cross-format comparison:
/// - Replace backslashes with forward slashes
/// - Strip Windows UNC prefix (\\?\ or //?/)
/// This ensures WalkDir paths (\\?\C:\...) and remapping-resolved paths (C:\...)
/// compare equal despite different representations.
fn normalize_path(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s).to_string();
    s
}

/// Hapus ANSI escape sequences dari string (misalnya \x1b[31m, \x1b[0m, dll).
/// foundry-compilers menghasilkan output berwarna dengan ESC byte raw yang
/// menyebabkan JSON.parse gagal di frontend jika tidak dibersihkan.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Lewati seluruh escape sequence: ESC [ ... <letter>
            if chars.peek() == Some(&'[') {
                chars.next(); // konsumsi '['
                for inner in chars.by_ref() {
                    if inner.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            // ESC tanpa '[' juga diabaikan
        } else {
            out.push(c);
        }
    }
    out
}
