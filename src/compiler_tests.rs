#[cfg(test)]
mod tests {
    use crate::compiler::debug_build_info;
    use std::{path::PathBuf, process::Command};

    #[test]
    fn debugger_build_info_imports_into_python_with_exact_runtime() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let python = root.join("debugger/.venv").join(if cfg!(windows) {
            "Scripts/python.exe"
        } else {
            "bin/python"
        });
        if !python.is_file() {
            eprintln!("Skipping build-info check: run `uv sync --project debugger --locked` to install the worker");
            return;
        }
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join("contracts")).unwrap();
        std::fs::write(
            project.path().join("contracts/Probe.sol"),
            "// SPDX-License-Identifier: MIT\npragma solidity =0.8.26; contract Probe { uint96 public feeBps; mapping(address=>uint256) public balances; function fee(uint256 n) public view returns(uint256) { return n * feeBps / 10000; } }",
        ).unwrap();
        let info = debug_build_info(project.path().to_path_buf(), vec![]).unwrap();
        assert!(info["input"]["sources"].as_object().unwrap().len() >= 1);
        assert!(!info["chasmBuildId"].as_str().unwrap().is_empty());
        let output = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(output.path(), serde_json::to_vec(&info).unwrap()).unwrap();
        let check = Command::new(python)
            .arg("-I").arg("-c")
            .arg("import sys; from chasm_debugger.compile import load_build_info, compare_runtime; p=load_build_info(sys.argv[1]); a=p.artifact('Probe'); assert a and a.deployed_source_map and a.storage_layout; assert p.asts and all(s.file_id >= 0 for s in p.sources.values()); assert compare_runtime(a,a.deployed_bytecode)['exact_match']; print('build-info source map, AST, storage layout, runtime verified')")
            .arg(output.path()).output().unwrap();
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
        assert!(!project.path().join("out").exists());
        assert!(!project.path().join("cache").exists());
        println!("{}", String::from_utf8_lossy(&check.stdout));
    }

    /// The worker's build-info uses the solc the workspace locked, not the
    /// newest one the pragma allows; two versions name both.
    #[test]
    fn debugger_build_info_follows_the_workspace_solc_lock() {
        let installed = foundry_compilers::solc::Solc::installed_versions();
        let has = |version: &str| installed.iter().any(|item| item.to_string() == version);
        if !has("0.8.26") || !has("0.8.28") {
            eprintln!("Skipping: solc 0.8.26 and 0.8.28 must both be installed");
            return;
        }
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join("contracts")).unwrap();
        std::fs::write(project.path().join("contracts/A.sol"), "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.20; contract A { uint public x; }").unwrap();
        crate::config::ChasmConfig::update(project.path(), |config| config.compiler.solc = "0.8.26".into()).unwrap();
        let info = debug_build_info(project.path().to_path_buf(), vec![]).unwrap();
        assert_eq!(info["solcVersion"], "0.8.26", "the lock wins over the newest matching solc");
        assert_eq!((info["chasmSettings"]["locked"].as_bool(), info["chasmSettings"]["source"].as_str()), (Some(true), Some("chasm")));
        // Unlocked, two files that need different compilers give two units.
        crate::config::ChasmConfig::update(project.path(), |config| config.compiler.solc.clear()).unwrap();
        std::fs::write(project.path().join("contracts/A.sol"), "// SPDX-License-Identifier: MIT\npragma solidity =0.8.26; contract A {}").unwrap();
        std::fs::write(project.path().join("contracts/B.sol"), "// SPDX-License-Identifier: MIT\npragma solidity =0.8.28; contract B {}").unwrap();
        let error = debug_build_info(project.path().to_path_buf(), vec![]).unwrap_err().to_string();
        assert!(error.contains("2 solc versions") && error.contains("0.8.26") && error.contains("0.8.28"), "{error}");
    }
}
