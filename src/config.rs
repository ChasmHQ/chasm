use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Struct untuk .chasm/chasm.toml — format kompatibel dengan foundry.toml
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ChasmConfig {
    #[serde(default)]
    pub remappings: Vec<String>,
}

impl ChasmConfig {
    /// Baca config dari {root_dir}/.chasm/chasm.toml
    /// Jika file tidak ada, return default (empty)
    pub fn load(root_dir: &Path) -> Result<Self> {
        let config_path = root_dir.join(".chasm").join("chasm.toml");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: ChasmConfig = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(ChasmConfig::default())
        }
    }

    /// Tulis config ke {root_dir}/.chasm/chasm.toml (buat direktori jika belum ada)
    pub fn save(&self, root_dir: &Path) -> Result<()> {
        let chasm_dir = root_dir.join(".chasm");
        std::fs::create_dir_all(&chasm_dir)?;
        let config_path = chasm_dir.join("chasm.toml");
        let content = toml::to_string_pretty(self)?;
        std::fs::write(config_path, content)?;
        Ok(())
    }

    /// Tambahkan remapping jika belum ada (hindari duplikat)
    pub fn add_remapping(&mut self, remapping: String) {
        // Cek berdasarkan prefix (bagian sebelum '=')
        let prefix = remapping.split('=').next().unwrap_or("").to_string();
        let already_exists = self
            .remappings
            .iter()
            .any(|r| r.split('=').next().unwrap_or("") == prefix);
        if !already_exists {
            self.remappings.push(remapping);
        }
    }

    /// Ganti semua remapping OZ dengan yang baru (untuk switch versi)
    pub fn replace_oz_remapping(&mut self, new_remapping: String) {
        self.remappings.retain(|r| !r.contains("@openzeppelin"));
        self.remappings.push(new_remapping);
    }
}

/// Scan root_dir untuk instalasi openzeppelin-contracts.
/// Urutan pengecekan:
///   1. {root}/lib/openzeppelin-contracts
///   2. {root}/node_modules/@openzeppelin/contracts
///   3. Rekursif WalkDir max depth 4, cari folder "openzeppelin-contracts"
/// Mengembalikan path ke direktori root OZ (bukan /contracts subdir)
pub fn scan_openzeppelin(root_dir: &Path) -> Option<PathBuf> {
    // 1. Cek lib/openzeppelin-contracts (foundry-style)
    let lib_path = root_dir.join("lib").join("openzeppelin-contracts");
    if lib_path.exists() && lib_path.is_dir() {
        return Some(lib_path);
    }

    // 2. Cek node_modules/@openzeppelin/contracts (npm-style)
    let npm_path = root_dir
        .join("node_modules")
        .join("@openzeppelin")
        .join("contracts");
    if npm_path.exists() && npm_path.is_dir() {
        return Some(npm_path);
    }

    // 3. Rekursif scan max depth 4, cari folder bernama "openzeppelin-contracts"
    // Skip direktori yang besar/tidak relevan untuk menghindari scan lambat
    const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".chasm", "dist", "out", "cache", ".foundry"];

    for entry in WalkDir::new(root_dir)
        .max_depth(4)
        .into_iter()
        .filter_entry(|e| {
            // Skip direktori yang diketahui tidak relevan
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                return !SKIP_DIRS.iter().any(|skip| *skip == name.as_ref());
            }
            true
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir()
            && entry.file_name().to_string_lossy() == "openzeppelin-contracts"
        {
            return Some(entry.path().to_path_buf());
        }
    }

    None
}

/// Generate remapping string from OZ path.
/// Supports two layouts:
///   - Foundry-style: oz_path/contracts/ contains Solidity sources
///   - npm-style: oz_path/ contains Solidity sources directly
///
/// Always produces an absolute path so foundry-compilers passes it correctly
/// to solc regardless of working directory. Relative paths cause solc to ignore
/// the remapping silently.
pub fn generate_remapping(oz_path: &Path, _root_dir: &Path) -> String {
    // Determine source location
    let contracts_subdir = oz_path.join("contracts");
    let source_path = if contracts_subdir.exists() {
        contracts_subdir
    } else {
        oz_path.to_path_buf()
    };

    // Canonicalize to resolve symlinks (e.g. /var -> /private/var on macOS).
    // Fall back to the original path if canonicalize fails (e.g. path doesn't exist yet).
    let source_path = source_path.canonicalize().unwrap_or(source_path);

    // Always use absolute path — foundry-compilers/solc does not reliably resolve
    // relative remapping paths relative to the project root.
    // Strip Windows UNC prefix (\\?\) BEFORE replacing backslashes — if we replace
    // first, "\\?\" becomes "//?/" and the strip_prefix no longer matches.
    let raw = source_path.to_string_lossy();
    let raw = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    let abs = raw.replace('\\', "/");

    // Ensure trailing slash
    let abs = if abs.ends_with('/') { abs } else { format!("{}/", abs) };

    format!("@openzeppelin/contracts/={}", abs)
}


/// Path ke direktori global chasm lib: ~/.chasm/lib/
pub fn global_lib_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".chasm").join("lib")
}

/// Daftar versi OZ yang tersedia secara global di ~/.chasm/lib/
/// Format direktori: openzeppelin-contracts@X.Y.Z
pub fn list_global_oz_versions() -> Vec<String> {
    let lib_dir = global_lib_dir();
    if !lib_dir.exists() {
        return vec![];
    }

    let mut versions = vec![];
    if let Ok(entries) = std::fs::read_dir(&lib_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("openzeppelin-contracts@") && entry.path().is_dir() {
                // Ekstrak versi dari nama folder: "openzeppelin-contracts@4.9.6" → "4.9.6"
                if let Some(version) = name.strip_prefix("openzeppelin-contracts@") {
                    versions.push(version.to_string());
                }
            }
        }
    }
    // Sort semver numerik (bukan lexicographic) agar "10.0.0" > "9.0.0"
    versions.sort_by(|a, b| {
        let parse = |s: &str| -> Vec<u64> {
            s.split('.').map(|n| n.parse().unwrap_or(0)).collect()
        };
        parse(a).cmp(&parse(b))
    });
    versions
}

/// Path ke instalasi OZ global untuk versi tertentu
pub fn global_oz_path(version: &str) -> PathBuf {
    global_lib_dir().join(format!("openzeppelin-contracts@{}", version))
}

