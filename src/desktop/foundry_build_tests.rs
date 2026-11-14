use super::*;
use serde_json::json;

const BLACK_TITHE: &str = r#"
[profile.default]
src = "src"
out = "out"
libs = ["lib"]
solc = "0.8.28"
optimizer = true
optimizer_runs = "200"
via_ir = true
evm_version = "cancun"
bytecode_hash = "none"
cbor_metadata = false
"#;

fn fixture() -> (tempfile::TempDir, FoundryPreflight) {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("foundry.toml"), BLACK_TITHE).unwrap();
    fs::write(
        temp.path().join("src/Tithe.sol"),
        "pragma solidity 0.8.28; contract Tithe {}\n",
    )
    .unwrap();
    let review = preflight(temp.path()).unwrap();
    (temp, review)
}

fn build_info(review: &FoundryPreflight) -> Value {
    let contents = "pragma solidity 0.8.28; contract Tithe {}\n";
    assert_eq!(review.source_manifest[0].keccak256, hash(contents));
    json!({
        "solcVersion":"0.8.28",
        "input": {
            "settings": {
                "optimizer":{"enabled":true,"runs":200},
                "viaIR":true,
                "evmVersion":"cancun",
                "metadata":{"bytecodeHash":"none","appendCBOR":false}
            },
            "sources":{"src/Tithe.sol":{"content":contents}}
        },
        "output":{"contracts":{"src/Tithe.sol":{"Tithe":{
            "abi":[{"type":"function","name":"withdraw","inputs":[],"outputs":[]}],
            "storageLayout":{"storage":[],"types":{}},
            "evm":{
                "methodIdentifiers":{"withdraw()":"3ccfd60b"},
                "bytecode":{"object":"6001","sourceMap":"0:1:0"},
                "deployedBytecode":{"object":"6002","sourceMap":"1:2:0","immutableReferences":{"4":[{"start":0,"length":2}]}}
            }
        }}}}
    })
}

#[test]
fn black_tithe_profile_is_explicit_and_effective() {
    let (_, review) = fixture();
    assert_eq!(review.settings.solc, "0.8.28");
    assert_eq!(review.settings.evm_version, "cancun");
    assert_eq!(
        review.settings.optimizer,
        EffectiveValue {
            value: true,
            explicit: true
        }
    );
    assert_eq!(
        review.settings.optimizer_runs,
        EffectiveValue {
            value: 200,
            explicit: true
        }
    );
    assert_eq!(
        review.settings.via_ir,
        EffectiveValue {
            value: true,
            explicit: true
        }
    );
    assert_eq!(
        review.settings.bytecode_hash,
        EffectiveValue {
            value: "none".into(),
            explicit: true
        }
    );
    assert_eq!(
        review.settings.cbor_metadata,
        EffectiveValue {
            value: false,
            explicit: true
        }
    );
    assert!(review.profile_summary().contains("via IR: true (explicit)"));
    assert!(review.settings.matches_required(&review.settings));
    assert_eq!(review.source_manifest.len(), 1);
    assert!(review.trust_warning.contains("not a sandbox"));
    assert!(review.profile_summary().contains(&review.digest));
}

#[test]
fn defaults_and_required_comparison_are_distinct() {
    let settings =
        parse_settings(b"[profile.default]\nsolc_version='0.8.28'\nevm_version='cancun'\n")
            .unwrap();
    assert!(!settings.optimizer.value);
    assert!(!settings.optimizer.explicit);
    assert_eq!(settings.optimizer_runs.value, 200);
    assert!(!settings.optimizer_runs.explicit);
    assert!(!settings.via_ir.value);
    assert_eq!(settings.bytecode_hash.value, "ipfs");
    assert!(settings.cbor_metadata.value);
    assert!(settings.profile_summary().contains("Foundry default"));
    let mut same = settings.clone();
    same.optimizer.explicit = true;
    assert!(settings.matches_required(&same));
    same.optimizer.value = true;
    assert!(!settings.matches_required(&same));
}

#[test]
fn config_rejects_ambiguity_invalid_types_and_unsupported_settings() {
    for value in [
        "[profile.default]\nsolc='0.8.28'\n",
        "[profile.default]\nevm_version='cancun'\n",
        "[profile.default]\nsolc='^0.8.28'\nevm_version='cancun'\n",
        "[profile.default]\nsolc='../solc'\nevm_version='cancun'\n",
        "[profile.default]\nsolc='0.8.28'\nsolc_version='0.8.28'\nevm_version='cancun'\n",
        "[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\noptimizer='true'\n",
        "[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\noptimizer_runs=-1\n",
        "[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\noptimizer_runs='200oops'\n",
        "[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\nbytecode_hash='unknown'\n",
        "[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\nallow_paths=['..']\n",
        "[profile.default\nsolc='0.8.28'",
    ] {
        assert!(
            parse_settings(value.as_bytes()).is_err(),
            "accepted: {value}"
        );
    }
    assert!(parse_settings(&vec![b' '; CONFIG_LIMIT + 1]).is_err());
    assert!(parse_settings(
        b"[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\n[profile.legacy]\nvia_ir=false\n"
    )
    .is_ok());
    assert!(parse_settings(b"[profile.default]\nsolc='0.8.28'\nevm_version='cancun'\nfs_permissions=[{access='read-write',path='./'}]\n").is_ok());
}

#[test]
fn rejects_path_escapes_and_inherited_overrides() {
    for path in [
        "../lib",
        "/lib",
        "C:/other",
        "lib/../../outside",
        "lib\\dependency",
        "lib\0name",
    ] {
        assert!(
            relative_path(path, "fixture").is_err(),
            "accepted: {path:?}"
        );
    }
    for remapping in [
        "x=../outside",
        "x=/outside",
        "x=C:/outside",
        "x=lib/../../outside",
        "=lib",
        "x=",
    ] {
        assert!(
            remapping_target(remapping).is_err(),
            "accepted: {remapping}"
        );
    }
    assert!(relative_path("lib/package/src", "fixture").is_ok());
    assert!(env_override(OsStr::new("FOUNDRY_PROFILE")));
    assert!(env_override(OsStr::new("dapp_remappings")));
    assert!(!env_override(OsStr::new("PATH")));
}

#[test]
fn root_config_must_be_exact_and_sources_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("foundry.toml"), BLACK_TITHE).unwrap();
    fs::create_dir(temp.path().join("nested")).unwrap();
    assert!(preflight(&temp.path().join("nested")).is_err());
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("src/Large.sol"),
        vec![b'a'; SOURCE_LIMIT + 1],
    )
    .unwrap();
    assert!(preflight(temp.path()).unwrap_err().contains("exceeds"));
}

#[test]
fn oversized_build_info_and_multiple_units_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("a.json");
    fs::write(&first, vec![b' '; BUILD_INFO_LIMIT + 1]).unwrap();
    assert!(read_bounded(&first, BUILD_INFO_LIMIT).is_err());
    fs::write(temp.path().join("b.json"), b"{}").unwrap();
    assert!(build_info_file(temp.path()).is_err());
}

#[test]
fn source_and_remapping_changes_change_review_digest() {
    let (temp, first) = fixture();
    fs::write(
        temp.path().join("src/Tithe.sol"),
        "pragma solidity 0.8.28; contract Changed {}\n",
    )
    .unwrap();
    let second = preflight(temp.path()).unwrap();
    assert_ne!(first.digest, second.digest);
    fs::create_dir(temp.path().join("lib")).unwrap();
    fs::write(temp.path().join("remappings.txt"), "demo/=lib/demo/src/\n").unwrap();
    let third = preflight(temp.path()).unwrap();
    assert_ne!(second.digest, third.digest);
    assert!(third
        .settings
        .remappings
        .contains(&"demo/=lib/demo/src/".to_string()));
}

#[test]
fn external_and_unreviewed_relative_imports_are_rejected() {
    let (temp, _) = fixture();
    fs::write(
        temp.path().join("src/Tithe.sol"),
        "pragma solidity 0.8.28; import '../../outside.sol'; contract Tithe {}\n",
    )
    .unwrap();
    assert!(preflight(temp.path()).is_err());
    fs::write(
        temp.path().join("src/Tithe.sol"),
        "pragma solidity 0.8.28; import './Missing.sol'; contract Tithe {}\n",
    )
    .unwrap();
    assert!(preflight(temp.path()).is_err());
}

#[cfg(windows)]
#[test]
fn rejects_directory_reparse_points() {
    let (temp, _) = fixture();
    let outside = tempfile::tempdir().unwrap();
    let link = temp.path().join("external");
    if std::os::windows::fs::symlink_dir(outside.path(), &link).is_err() {
        return;
    }
    assert!(preflight(temp.path()).unwrap_err().contains("reparse"));
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_sources_and_remapping_directories() {
    use std::os::unix::fs::symlink;
    let (temp, _) = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("External.sol"),
        "contract External {}\n",
    )
    .unwrap();
    symlink(outside.path(), temp.path().join("external")).unwrap();
    assert!(preflight(temp.path()).unwrap_err().contains("link"));
}

#[test]
fn local_compiler_search_stays_in_version_cache() {
    let temp = tempfile::tempdir().unwrap();
    let version = "0.8.28";
    fs::create_dir(temp.path().join(version)).unwrap();
    assert!(local_solc_in_cache(temp.path(), version).is_err());
    #[cfg(windows)]
    let file = "solc-0.8.28.exe";
    #[cfg(not(windows))]
    let file = "solc-0.8.28";
    let executable = temp.path().join(version).join(file);
    fs::write(&executable, b"fixture, not executed").unwrap();
    assert_eq!(
        local_solc_in_cache(temp.path(), version).unwrap(),
        fs::canonicalize(executable).unwrap()
    );
    assert!(local_solc_in_cache(temp.path(), "../other").is_err());
}

#[test]
fn command_targets_reviewed_root_and_isolated_directories() {
    let (_, review) = fixture();
    let output = tempfile::tempdir().unwrap();
    let solc = output.path().join("solc-fixture");
    let plan = command_plan(&review, output.path(), &solc);
    assert_eq!(plan.program, "forge");
    assert_eq!(plan.cwd, review.workspace);
    let args = &plan.args;
    let option = |name: &str| -> &OsStr {
        let index = args.iter().position(|item| item == name).unwrap();
        args[index + 1].as_os_str()
    };
    for flag in [
        "build",
        "--build-info",
        "--offline",
        "--no-auto-detect",
        "--extra-output",
        "storageLayout",
        "evm.methodIdentifiers",
        "evm.bytecode.sourceMap",
        "evm.deployedBytecode.sourceMap",
        "evm.deployedBytecode.immutableReferences",
    ] {
        assert!(args.iter().any(|item| item == flag));
    }
    assert_eq!(Path::new(option("--use")), solc);
    assert_eq!(Path::new(option("--root")), review.workspace);
    assert_eq!(Path::new(option("--config-path")), review.config_path);
    assert_eq!(Path::new(option("--out")), output.path().join("out"));
    assert_eq!(
        Path::new(option("--cache-path")),
        output.path().join("cache")
    );
    assert_eq!(
        Path::new(option("--build-info-path")),
        output.path().join("build-info")
    );
}

#[test]
fn output_settings_sources_and_artifact_hashes_are_checked() {
    let (_, review) = fixture();
    let mut info = build_info(&review);
    assert!(settings_warnings(&info, &review.settings).is_empty());
    assert!(source_warnings(&info, &review.source_manifest, &review.workspace).is_empty());
    let (artifacts, warnings) = artifact_summaries(&info, &review.digest).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].source, "src/Tithe.sol");
    assert_eq!(artifacts[0].contract, "Tithe");
    assert_eq!(
        artifacts[0].id,
        artifact_id(&review.digest, "src/Tithe.sol", "Tithe")
    );
    assert_ne!(
        artifacts[0].id,
        artifact_id("another-review", "src/Tithe.sol", "Tithe")
    );
    assert_eq!(artifacts[0].creation_bytecode.as_deref(), Some("0x6001"));
    assert_eq!(artifacts[0].runtime_bytecode.as_deref(), Some("0x6002"));
    assert!(artifacts[0].deployable());
    assert_eq!(artifacts[0].creation_source_map.as_deref(), Some("0:1:0"));
    assert_eq!(artifacts[0].runtime_source_map.as_deref(), Some("1:2:0"));
    assert!(artifacts[0].storage_layout.is_some());
    assert!(artifacts[0].method_identifiers.is_some());
    assert_eq!(artifacts[0].creation_keccak256, Some(hash([0x60, 0x01])));
    assert_eq!(artifacts[0].runtime_keccak256, Some(hash([0x60, 0x02])));
    assert_eq!(
        artifacts[0].immutable_references.as_ref().unwrap()["4"][0]["length"],
        2
    );
    for (pointer, value) in [
        ("/solcVersion", json!("0.8.27")),
        ("/input/settings/optimizer/enabled", json!(false)),
        ("/input/settings/optimizer/runs", json!(201)),
        ("/input/settings/viaIR", json!(false)),
        ("/input/settings/evmVersion", json!("shanghai")),
        ("/input/settings/metadata/bytecodeHash", json!("ipfs")),
        ("/input/settings/metadata/appendCBOR", json!(true)),
    ] {
        let mut altered = info.clone();
        *altered.pointer_mut(pointer).unwrap() = value;
        assert!(
            !settings_warnings(&altered, &review.settings).is_empty(),
            "{pointer}"
        );
        *altered.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(
            !settings_warnings(&altered, &review.settings).is_empty(),
            "missing {pointer}"
        );
    }
    info["input"]["sources"]["src/Tithe.sol"]["content"] = json!("changed");
    assert!(!source_warnings(&info, &review.source_manifest, &review.workspace).is_empty());
    let mut external = build_info(&review);
    external["output"]["contracts"]["outside.sol"] = json!({});
    assert!(!source_warnings(&external, &review.source_manifest, &review.workspace).is_empty());
    let mut unlinked = build_info(&review);
    unlinked["output"]["contracts"]["src/Tithe.sol"]["Tithe"]["evm"]["bytecode"]["object"] =
        json!("60__$abcd$__");
    let (artifacts, warnings) = artifact_summaries(&unlinked, &review.digest).unwrap();
    assert!(!artifacts[0].deployable());
    assert!(artifacts[0].creation_bytecode.is_none());
    assert!(!warnings.is_empty());
    let mut linked_hex = build_info(&review);
    linked_hex["output"]["contracts"]["src/Tithe.sol"]["Tithe"]["evm"]["bytecode"]
        ["linkReferences"] = json!({"L.sol":{"L":[{"start":1,"length":20}]}});
    let (artifacts, warnings) = artifact_summaries(&linked_hex, &review.digest).unwrap();
    assert!(!artifacts[0].deployable());
    assert!(!warnings.is_empty());
    let mut missing_immutable = build_info(&review);
    missing_immutable["output"]["contracts"]["src/Tithe.sol"]["Tithe"]["evm"]["deployedBytecode"]
        .as_object_mut()
        .unwrap()
        .remove("immutableReferences");
    let (artifacts, warnings) = artifact_summaries(&missing_immutable, &review.digest).unwrap();
    assert!(artifacts[0].immutable_references.is_none());
    assert!(!warnings.is_empty());
}

#[test]
fn source_comparison_accepts_forge_line_endings_and_comment_encoding() {
    let reviewed = "pragma solidity 0.8.28;\r\n// café \u{2014} voilà\r\ncontract Tithe {\r\n    string constant S = unicode\"café // not a comment\";\r\n    /* note: \u{2014}\r\n       next line */\r\n}\r\n";
    let compiled = "pragma solidity 0.8.28;\n// caf\\u00e9 \\u2014 voilà\ncontract Tithe {\n    string constant S = unicode\"café // not a comment\";\n    /* note: \\u2014\n       next line */\n}\n";
    assert!(solidity_sources_match(
        reviewed.as_bytes(),
        compiled.as_bytes()
    ));
    assert!(solidity_sources_match(
        compiled.as_bytes(),
        reviewed.as_bytes()
    ));

    let continued_crlf = b"contract C { string s = \"a\\\r\nb\"; }\r\n";
    let continued_lf = b"contract C { string s = \"a\\\nb\"; }\n";
    assert!(solidity_sources_match(continued_crlf, continued_lf));
}

#[test]
fn source_comparison_rejects_changed_code_boundaries_and_literals() {
    for (reviewed, compiled) in [
        ("contract Tithe {}", "contractTithe {}"),
        ("uint x = a + +b;", "uint x = a++b;"),
        ("uint x = a + /* separator */ +b;", "uint x = a++b;"),
        ("uint x = 1 ether;", "uint x = 1ether;"),
        ("uint x = 1 . 2;", "uint x = 1.2;"),
        (
            "string s = unicode\"plain\";",
            "string s = unicode \"plain\";",
        ),
        ("bytes b = hex\"01\";", "bytes b = hex \"01\";"),
        (
            "string s = unicode\"café\";",
            "string s = unicode\"caf\\u00e9\";",
        ),
        ("string s = \"a // b\";", "string s = \"a /* b\";"),
        ("string s = \"a b\";", "string s = \"ab\";"),
        (
            "string s = \"\\\"/*not a comment*/\";",
            "string s = \"\\\"/*changed*/\";",
        ),
    ] {
        assert!(
            !solidity_sources_match(reviewed.as_bytes(), compiled.as_bytes()),
            "accepted {reviewed:?} as {compiled:?}"
        );
        assert!(!solidity_sources_match(
            compiled.as_bytes(),
            reviewed.as_bytes()
        ));
    }
}

#[test]
fn source_comparison_preserves_compiler_significant_comments() {
    for (reviewed, compiled) in [
        (
            "assembly {}",
            "/// @solidity memory-safe-assembly\nassembly {}",
        ),
        (
            "/// @solidity memory-safe-assembly\nassembly {}",
            "/// @solidity memory-safe-assemblx\nassembly {}",
        ),
        (
            "// @solidity memory-safe-assembly\nassembly {}",
            "/// @solidity memory-safe-assembly\nassembly {}",
        ),
        (
            "/* @solidity memory-safe-assembly */ assembly {}",
            "/** @solidity memory-safe-assembly */ assembly {}",
        ),
        (
            "/// @solidity memory-safe-assembly\nassembly {}",
            "/// \\u0040solidity memory-safe-assembly\nassembly {}",
        ),
        (
            "// SPDX-License-Identifier: MIT\ncontract C {}",
            "// SPDX-License-Identifier: GPL-3.0\ncontract C {}",
        ),
    ] {
        assert!(
            !solidity_sources_match(reviewed.as_bytes(), compiled.as_bytes()),
            "accepted {reviewed:?} as {compiled:?}"
        );
    }
}

#[test]
fn source_comparison_fails_closed_for_unreadable_or_malformed_input() {
    for (reviewed, compiled) in [
        (&b"/* unterminated"[..], &b"/* different"[..]),
        (&b"\"unterminated"[..], &b"\"different"[..]),
        (&b"\"escape\\"[..], &b"/* unterminated"[..]),
        (&b"\xff"[..], &b"\xfe"[..]),
        (&b"/* unterminated"[..], &b"contract C {}"[..]),
    ] {
        assert!(!solidity_sources_match(reviewed, compiled));
        assert!(!solidity_sources_match(compiled, reviewed));
    }
}

#[test]
fn source_warnings_verify_fallback_against_reviewed_file() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("C.sol");
    let reviewed = "contract C {\r\n // café\r\n uint x = 1;\r\n}\r\n";
    fs::write(&source, reviewed).unwrap();
    let manifest = vec![SourceManifestEntry {
        path: "C.sol".into(),
        size: reviewed.len() as u64,
        keccak256: hash(reviewed),
    }];
    let info = |content: &str| json!({"input":{"sources":{"C.sol":{"content":content}}}});
    let compiled = "contract C {\n // caf\\u00e9\n uint x = 1;\n}\n";
    assert!(source_warnings(&info(compiled), &manifest, temp.path()).is_empty());
    assert!(!source_warnings(
        &info("contractC {\n // caf\\u00e9\n uint x = 1;\n}\n"),
        &manifest,
        temp.path()
    )
    .is_empty());
    assert!(!source_warnings(&info("/* unterminated"), &manifest, temp.path()).is_empty());

    fs::write(&source, compiled).unwrap();
    let changed = "contract C {\n // caf\\u00e9\n uint x = 1;\n}\n\n";
    assert!(!source_warnings(&info(changed), &manifest, temp.path()).is_empty());
    assert!(!source_warnings(
        &info("/* unterminated"),
        &manifest,
        &temp.path().join("missing")
    )
    .is_empty());
}

#[tokio::test]
async fn trust_gate_does_not_need_a_valid_workspace_or_compiler() {
    let (_temp, review) = fixture();
    let error = build(&review, false, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.contains("trust"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = build(&review, true, cancel).await.unwrap_err();
    assert!(error.contains("cancelled"));
    let mut forged = review.clone();
    forged.source_manifest[0].keccak256 = "0x00".into();
    let error = build(&forged, true, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.contains("changed after review"));
}

#[cfg(windows)]
#[tokio::test]
async fn windows_job_kills_child_when_job_is_dropped() {
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    let job = WindowsJob::new().unwrap();
    let mut command = Command::new("ping.exe");
    command
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    job.assign(&child).unwrap();
    assert!(child.try_wait().unwrap().is_none());
    drop(job);
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("job closure did not terminate the child")
        .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_job_terminate_kills_and_reaps_child() {
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    let job = WindowsJob::new().unwrap();
    let mut command = Command::new("ping.exe");
    command
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    job.assign(&child).unwrap();
    terminate_tree(&mut child, &job).await.unwrap();
    assert!(child.try_wait().unwrap().is_some());
}

#[tokio::test]
async fn bounded_pipe_counts_combined_output() {
    let used = AtomicUsize::new(0);
    let (mut writer, reader) = tokio::io::duplex(32);
    use tokio::io::AsyncWriteExt;
    writer.write_all(b"123456").await.unwrap();
    drop(writer);
    assert_eq!(read_pipe(reader, &used, 8).await.unwrap(), b"123456");
    let (mut writer, reader) = tokio::io::duplex(32);
    writer.write_all(b"789").await.unwrap();
    drop(writer);
    assert_eq!(
        read_pipe(reader, &used, 8).await.unwrap_err(),
        ProcessError::TooLarge
    );
}

#[tokio::test]
async fn process_deadline_reports_timeout_and_cancellation() {
    let cancel = CancellationToken::new();
    let result = process_deadline(std::future::pending::<()>(), &cancel, Duration::ZERO).await;
    assert_eq!(result.unwrap_err(), ProcessError::Timeout);
    cancel.cancel();
    let result = process_deadline(
        std::future::pending::<()>(),
        &cancel,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(result.unwrap_err(), ProcessError::Cancelled);
}
