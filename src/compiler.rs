use anyhow::Result;
use foundry_compilers::artifacts::ConfigurableContractArtifact;
use foundry_compilers::{Project, ProjectPathsConfig};
use std::path::PathBuf;

pub struct Compiler {
    project: Project,
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

        Ok(Self { project })
    }

    pub fn compile(&self) -> Result<Vec<(String, ConfigurableContractArtifact)>> {
        let output = self.project.compile()?;
        if output.has_compiler_errors() {
            return Err(anyhow::anyhow!("{output}"));
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
