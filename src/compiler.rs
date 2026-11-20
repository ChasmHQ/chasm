use anyhow::Result;
use foundry_compilers::artifacts::ConfigurableContractArtifact;
use foundry_compilers::artifacts::output_selection::{ContractOutputSelection, OutputSelection};
use foundry_compilers::artifacts::remappings::Remapping;
use foundry_compilers::{ConfigurableArtifacts, Project, ProjectPathsConfig};
use std::path::PathBuf;
use walkdir::WalkDir;

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod compiler_tests;

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
    /// Applies `[compiler]` settings: optimizer, runs, via-IR, EVM version and
    /// metadata hash. Unset keys keep solc's defaults.
    pub fn apply_settings(&mut self, config: &crate::config::CompilerConfig) -> Result<()> {
        config.validate().map_err(|error| anyhow::anyhow!(error))?;
        let settings = &mut self.project.settings.solc.settings;
        if let Some(enabled) = config.optimizer {
            settings.optimizer.enabled = Some(enabled);
            if enabled {
                settings.optimizer.runs = Some(config.optimizer_runs.unwrap_or(200));
            }
        } else if let Some(runs) = config.optimizer_runs {
            settings.optimizer.runs = Some(runs);
        }
        if let Some(via_ir) = config.via_ir {
            settings.via_ir = Some(via_ir);
        }
        if !config.evm_version.is_empty() {
            settings.evm_version = Some(config.evm_version.parse().map_err(|_| anyhow::anyhow!("invalid evm_version"))?);
        }
        if !config.bytecode_hash.is_empty() {
            let hash = config.bytecode_hash.parse().map_err(|error: String| anyhow::anyhow!(error))?;
            match &mut settings.metadata {
                Some(metadata) => metadata.bytecode_hash = Some(hash),
                None => settings.metadata = Some(hash.into()),
            }
        }
        Ok(())
    }

    /// Requests storageLayout from solc, and compiles with an exact solc version ("0.8.24"),
    /// installed into the svm folder when missing. Empty means Auto.
    pub fn new_for_storage_layout_with_solc(root: PathBuf, remappings: Vec<String>, solc: &str) -> Result<Self> {
        let mut compiler = Self::new_impl(root, remappings, true)?;
        if !solc.is_empty() {
            let version = semver::Version::parse(solc.trim()).map_err(|_| anyhow::anyhow!("solc version {solc:?} is not a version like 0.8.24."))?;
            let binary = match foundry_compilers::solc::Solc::find_svm_installed_version(&version)? {
                Some(binary) => binary,
                None => foundry_compilers::solc::Solc::blocking_install(&version)
                    .map_err(|error| anyhow::anyhow!("Could not download solc {version}: {error}"))?,
            };
            compiler.project.compiler = foundry_compilers::multi::MultiCompiler::new(Some(foundry_compilers::solc::SolcCompiler::Specific(binary)), None)?;
        }
        Ok(compiler)
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
            artifacts_config.additional_values.ast = true;
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
                let mut value = serde_json::to_value(OutputSelection::common_output_selection(outputs.clone())).unwrap_or_default();
                // The AST of each workspace file names local variables for the debugger.
                value["*"][""] = serde_json::json!(["ast"]);
                *selection = serde_json::from_value(value).unwrap_or_else(|_| OutputSelection::common_output_selection(outputs.clone()));
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

    pub fn debug_build_info(&mut self) -> Result<serde_json::Value> {
        self.project.build_info = true;
        self.project.offline = true;
        self.project.update_output_selection(|selection| {
            *selection = serde_json::from_value(serde_json::json!({
                "*": {
                    "": ["ast"],
                    "*": [
                        "abi", "storageLayout", "evm.bytecode.object",
                        "evm.bytecode.sourceMap", "evm.bytecode.linkReferences",
                        "evm.deployedBytecode.object", "evm.deployedBytecode.sourceMap",
                        "evm.deployedBytecode.linkReferences",
                        "evm.deployedBytecode.immutableReferences", "evm.methodIdentifiers"
                    ]
                }
            })).expect("static debugger output selection");
        });
        let output = self.project.compile_files(self.src_files.clone())?;
        if output.has_compiler_errors() {
            return Err(anyhow::anyhow!("{}", strip_ansi(&output.to_string())));
        }
        let infos = &output.output().build_infos;
        if infos.len() != 1 {
            let versions: Vec<String> = infos.iter().map(|info| info.build_info.get("solcVersion").and_then(|version| version.as_str()).unwrap_or("?").to_string()).collect();
            return Err(anyhow::anyhow!(
                "The workspace compiled with {} solc versions ({}); the offline engine takes one build-info. Lock one version in Compiler and libraries, or upload a build-info file.",
                infos.len(),
                versions.join(", ")
            ));
        }
        let mut value = serde_json::to_value(&infos[0])?;
        value["chasmBuildId"] = serde_json::Value::String(infos[0].id.clone());
        Ok(value)
    }

    /// Contracts with their source type ("src"/"lib") and source file path.
    pub fn compile_with_paths(&self) -> Result<Vec<(String, String, String, ConfigurableContractArtifact)>> {
        let output = self.project.compile_files(self.src_files.clone())?;
        if output.has_compiler_errors() {
            return Err(anyhow::anyhow!("{}", strip_ansi(&output.to_string())));
        }
        let src_set: std::collections::HashSet<String> = self.src_files
            .iter()
            .map(|p| normalize_path(&p.to_string_lossy()))
            .collect();
        Ok(output.into_artifacts()
            .map(|(id, artifact)| {
                let path = normalize_path(&id.source.to_string_lossy());
                let source_type = if src_set.contains(&path) { "src" } else { "lib" }.to_string();
                (id.name, source_type, path, artifact)
            })
            .collect())
    }

    pub fn compile_to_json(&self) -> Result<String> {
        let artifacts = self.compile_with_paths()?;

        #[derive(serde::Serialize)]
        struct CompileSuccess {
             r#type: String,
             contracts: Vec<ContractData>,
        }

        #[derive(serde::Serialize)]
        struct ContractData {
             name: String,
             source_type: String,
             source_path: String,
             artifact: ConfigurableContractArtifact,
        }

        let contracts_data: Vec<ContractData> = artifacts.into_iter().map(|(name, source_type, source_path, artifact)| {
             ContractData { name, source_type, source_path, artifact }
        }).collect();

        let msg = CompileSuccess {
             r#type: "compile_success".to_string(),
             contracts: contracts_data,
        };

        Ok(serde_json::to_string(&msg)?)
    }
}

pub fn debug_build_info(root: PathBuf, remappings: Vec<String>) -> Result<serde_json::Value> {
    // The offline engine must see the same bytecode as the workspace build:
    // the same build source, settings and locked solc. A broken config is an
    // error, not a silent fallback to defaults that would build other code.
    let _ = remappings;
    let effective = crate::build_config::resolve(&root).map_err(|error| anyhow::anyhow!(error))?;
    let mut compiler = Compiler::new_for_storage_layout_with_solc(root, effective.remappings, &effective.compiler.solc)?;
    compiler.apply_settings(&effective.compiler)?;
    let mut info = compiler.debug_build_info()?;
    info["chasmSettings"] = serde_json::json!({
        "solc": info["solcVersion"].clone(),
        "locked": !effective.compiler.solc.is_empty(),
        "source": effective.active.label(),
        "settings": effective.compiler.describe(),
    });
    Ok(info)
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
