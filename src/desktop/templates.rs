//! "New contract from template": writes a starter `.sol` file built on
//! OpenZeppelin v5 into the workspace. The project compiles it like any
//! other source; an unmapped OpenZeppelin is reported by the library manager.
use std::path::{Path, PathBuf};

/// (id, label) of each template, in the order the sidebar lists them.
pub const TEMPLATES: [(&str, &str); 3] = [("erc20", "ERC20 token"), ("erc721", "ERC721 NFT"), ("ownable", "Ownable contract")];

/// A Solidity identifier that is also a safe file name.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64
}

/// Token symbol from the capitals of the name, e.g. "MyToken" -> "MT".
fn symbol(name: &str) -> String {
    let capitals: String = name.chars().filter(char::is_ascii_uppercase).take(5).collect();
    if capitals.len() >= 2 {
        capitals
    } else {
        name.chars().filter(char::is_ascii_alphanumeric).take(3).collect::<String>().to_ascii_uppercase()
    }
}

pub fn source(kind: &str, name: &str) -> Option<String> {
    let symbol = symbol(name);
    let body = match kind {
        "erc20" => format!(
            r#"import {{ERC20}} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {{Ownable}} from "@openzeppelin/contracts/access/Ownable.sol";

contract {name} is ERC20, Ownable {{
    constructor(address initialOwner) ERC20("{name}", "{symbol}") Ownable(initialOwner) {{
        _mint(initialOwner, 1_000_000 * 10 ** decimals());
    }}

    function mint(address to, uint256 amount) external onlyOwner {{
        _mint(to, amount);
    }}
}}
"#
        ),
        "erc721" => format!(
            r#"import {{ERC721}} from "@openzeppelin/contracts/token/ERC721/ERC721.sol";
import {{Ownable}} from "@openzeppelin/contracts/access/Ownable.sol";

contract {name} is ERC721, Ownable {{
    uint256 private _nextTokenId;

    constructor(address initialOwner) ERC721("{name}", "{symbol}") Ownable(initialOwner) {{}}

    function safeMint(address to) external onlyOwner returns (uint256 tokenId) {{
        tokenId = _nextTokenId++;
        _safeMint(to, tokenId);
    }}
}}
"#
        ),
        "ownable" => format!(
            r#"import {{Ownable}} from "@openzeppelin/contracts/access/Ownable.sol";

contract {name} is Ownable {{
    uint256 public value;

    event ValueChanged(uint256 value);

    constructor(address initialOwner) Ownable(initialOwner) {{}}

    function setValue(uint256 newValue) external onlyOwner {{
        value = newValue;
        emit ValueChanged(newValue);
    }}
}}
"#
        ),
        _ => return None,
    };
    Some(format!("// SPDX-License-Identifier: MIT\npragma solidity ^0.8.20;\n\n{body}"))
}

/// Writes `<name>.sol` into `src/` when the project has one, else the
/// workspace root. Never overwrites a file.
pub fn create(workspace: &Path, kind: &str, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if !valid_name(name) {
        return Err("Name the contract with letters, digits and _ (e.g. MyToken).".into());
    }
    if !TEMPLATES.iter().any(|(id, _)| *id == kind) {
        return Err(format!("Unknown template. Use one of: {}.", TEMPLATES.map(|(id, label)| format!("{id} ({label})")).join(", ")));
    }
    let text = source(kind, name).ok_or("Unknown template.")?;
    let folder = if workspace.join("src").is_dir() { workspace.join("src") } else { workspace.to_path_buf() };
    let path = folder.join(format!("{name}.sol"));
    if path.exists() {
        return Err(format!("{} already exists. Pick another name.", path.file_name().unwrap_or_default().to_string_lossy()));
    }
    std::fs::write(&path, text).map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_write_once_with_a_valid_name() {
        let workspace = tempfile::tempdir().unwrap();
        for (kind, _) in TEMPLATES {
            let name = format!("My{}", kind.to_ascii_uppercase());
            let path = create(workspace.path(), kind, &name).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains(&format!("contract {name} is")), "{text}");
            assert!(text.contains("@openzeppelin/contracts/access/Ownable.sol"));
            assert!(create(workspace.path(), kind, &name).unwrap_err().contains("already exists"));
        }
        assert!(create(workspace.path(), "erc20", "1Bad").is_err());
        assert!(create(workspace.path(), "erc20", "../Escape").is_err());
        assert!(create(workspace.path(), "nope", "Fine").is_err());
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        assert!(create(workspace.path(), "erc20", "InSrc").unwrap().starts_with(workspace.path().join("src")));
        assert_eq!(symbol("MyToken"), "MT");
        assert_eq!(symbol("token"), "TOK");
    }
}
