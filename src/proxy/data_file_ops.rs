//! Mutation helpers for file-based trunk configs.
//!
//! Used by the "Promote to DB" workflow: after a file trunk is copied into
//! `rustpbx_trunks`, the source TOML file needs its section removed so the
//! next reload doesn't reintroduce the file-origin duplicate.
//!
//! Two trunk file shapes are supported (matching the loader in
//! `src/proxy/data.rs`):
//!
//!   1. Legacy `[trunks.<name>]` table-per-trunk layout.
//!   2. Newer `[[trunk]]` array-of-tables with a `name = "<name>"` key.
//!
//! Writes are atomic (`<path>.tmp` + `rename`) and a single `.bak` of the
//! pre-mutation contents is left next to the file. If `<name>` isn't found
//! the function returns Ok — promotion can still succeed because the file
//! simply won't re-emit a phantom row on the next load.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item};

/// Remove the single section/array entry for `trunk_name` from `path`.
/// Idempotent — missing entry is treated as success. Leaves a `.bak` copy.
pub fn remove_trunk_section_from_file(path: &Path, trunk_name: &str) -> Result<()> {
    let original = fs::read_to_string(path)
        .with_context(|| format!("read trunk file {}", path.display()))?;

    let mut doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parse trunk file {} as TOML", path.display()))?;

    let mut removed = false;

    // Shape 1: `[trunks.<name>]`
    if let Some(Item::Table(trunks_table)) = doc.get_mut("trunks") {
        if trunks_table.remove(trunk_name).is_some() {
            removed = true;
        }
        // If the parent `[trunks]` table is now empty, drop it too to avoid
        // a stranded empty section in the file.
        if trunks_table.is_empty() {
            doc.remove("trunks");
        }
    }

    // Shape 2: `[[trunk]]` array of tables, match by `name`.
    if let Some(Item::ArrayOfTables(arr)) = doc.get_mut("trunk") {
        let before = arr.len();
        arr.retain(|t| t.get("name").and_then(|i| i.as_str()) != Some(trunk_name));
        if arr.len() != before {
            removed = true;
        }
        if arr.is_empty() {
            doc.remove("trunk");
        }
    }

    if !removed {
        return Ok(());
    }

    let bak = path.with_extension(format!(
        "{}.bak",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("toml")
    ));
    fs::write(&bak, &original)
        .with_context(|| format!("write backup {}", bak.display()))?;

    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, doc.to_string())
        .with_context(|| format!("write tmp {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn removes_legacy_trunks_section() {
        let f = write_temp(
            r#"[trunks.alpha]
dest = "sip:1.2.3.4:5060"

[trunks.beta]
dest = "sip:5.6.7.8:5060"
"#,
        );
        remove_trunk_section_from_file(f.path(), "alpha").unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert!(!after.contains("alpha"));
        assert!(after.contains("beta"));
    }

    #[test]
    fn removes_arrayoftables_entry() {
        let f = write_temp(
            r#"[[trunk]]
name = "alpha"
kind = "webrtc"

[[trunk]]
name = "beta"
kind = "sip"
"#,
        );
        remove_trunk_section_from_file(f.path(), "alpha").unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert!(!after.contains("\"alpha\""));
        assert!(after.contains("\"beta\""));
    }

    #[test]
    fn missing_name_is_ok_and_leaves_file_untouched() {
        let before = r#"[trunks.alpha]
dest = "sip:1.2.3.4:5060"
"#;
        let f = write_temp(before);
        remove_trunk_section_from_file(f.path(), "nonexistent").unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn empty_parent_table_is_dropped() {
        let f = write_temp(
            r#"[trunks.alpha]
dest = "sip:1.2.3.4:5060"
"#,
        );
        remove_trunk_section_from_file(f.path(), "alpha").unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert!(!after.contains("[trunks"));
    }
}
