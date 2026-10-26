//! Merge-only, idempotent writers for agent configuration files.
//!
//! Nothing here replaces a configuration file wholesale: JSON documents are
//! parsed, the `ukma` server entry is merged in (or removed), and every other
//! key is preserved. TOML documents get one delimited block that can be
//! removed again byte-for-byte.
//!
//! Honest boundaries:
//! - a rewritten JSON document has its object keys sorted (`serde_json`'s
//!   default map), so the diff of an existing file can be larger than the
//!   change itself;
//! - writes are plain `fs::write`; there is no cross-process locking.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::agents::ConfigFormat;
use super::{io_error, SetupError};

/// The start marker of the managed TOML block.
pub const TOML_BEGIN: &str = "# >>> ukma (managed by `ukma install`; remove with `ukma uninstall`)";
/// The end marker of the managed TOML block.
pub const TOML_END: &str = "# <<< ukma";

/// The result of planning (and, unless it is a dry run, applying) one write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The file the plan is about.
    pub path: PathBuf,
    /// Whether the file's content changed.
    pub changed: bool,
    /// Whether the file did not exist before.
    pub created: bool,
    /// A short human note (for example why a removal was skipped).
    pub detail: String,
}

impl Plan {
    fn unchanged(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            changed: false,
            created: false,
            detail: String::new(),
        }
    }
}

fn read_object(path: &Path) -> Result<Option<Map<String, Value>>, SetupError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|error| io_error("read", path, &error))?;
    if text.trim().is_empty() {
        return Ok(Some(Map::new()));
    }
    let value: Value = serde_json::from_str(&text).map_err(|error| SetupError::Config {
        path: path.to_path_buf(),
        detail: format!("invalid JSON: {error}"),
    })?;
    match value {
        Value::Object(map) => Ok(Some(map)),
        _ => Err(SetupError::Config {
            path: path.to_path_buf(),
            detail: "the document is not a JSON object".into(),
        }),
    }
}

fn write_text(path: &Path, text: &str) -> Result<(), SetupError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| io_error("create", parent, &error))?;
    }
    fs::write(path, text).map_err(|error| io_error("write", path, &error))
}

fn write_object(path: &Path, map: &Map<String, Value>, dry_run: bool) -> Result<(), SetupError> {
    if dry_run {
        return Ok(());
    }
    let text = serde_json::to_string_pretty(&Value::Object(map.clone())).map_err(|error| {
        SetupError::Config {
            path: path.to_path_buf(),
            detail: format!("cannot serialize the document: {error}"),
        }
    })?;
    write_text(path, &format!("{text}\n"))
}

/// Merge the `ukma` entry into a JSON document. Returns whether it changed.
fn merge_entry(
    map: &mut Map<String, Value>,
    key: &str,
    entry: &Value,
    path: &Path,
) -> Result<bool, SetupError> {
    match map.get_mut(key) {
        Some(Value::Object(servers)) => {
            let before = Value::Object(servers.clone());
            servers.insert("ukma".into(), entry.clone());
            Ok(Value::Object(servers.clone()) != before)
        }
        Some(_) => Err(SetupError::Config {
            path: path.to_path_buf(),
            detail: format!("`{key}` exists but is not a JSON object"),
        }),
        None => {
            let mut servers = Map::new();
            servers.insert("ukma".into(), entry.clone());
            map.insert(key.into(), Value::Object(servers));
            Ok(true)
        }
    }
}

/// Merge a UKMA server entry into a JSON configuration file.
pub fn install_json(
    path: &Path,
    format: ConfigFormat,
    entry: &Value,
    dry_run: bool,
) -> Result<Plan, SetupError> {
    let key = format.container().ok_or_else(|| {
        SetupError::Unsupported(format!("{} is not a JSON configuration", path.display()))
    })?;
    let existing = read_object(path)?;
    let created = existing.is_none();
    let mut map = existing.unwrap_or_default();
    let changed = merge_entry(&mut map, key, entry, path)?;
    if changed {
        write_object(path, &map, dry_run)?;
    }
    Ok(Plan {
        path: path.to_path_buf(),
        changed,
        created,
        detail: String::new(),
    })
}

/// Remove the UKMA server entry from a JSON configuration file.
pub fn remove_json(path: &Path, format: ConfigFormat, dry_run: bool) -> Result<Plan, SetupError> {
    let key = format.container().ok_or_else(|| {
        SetupError::Unsupported(format!("{} is not a JSON configuration", path.display()))
    })?;
    let Some(mut map) = read_object(path)? else {
        return Ok(Plan::unchanged(path));
    };
    let mut changed = false;
    if let Some(Value::Object(servers)) = map.get_mut(key) {
        changed = servers.remove("ukma").is_some();
        if changed && servers.is_empty() {
            map.remove(key);
        }
    }
    if changed {
        write_object(path, &map, dry_run)?;
    }
    Ok(Plan {
        path: path.to_path_buf(),
        changed,
        created: false,
        detail: String::new(),
    })
}

/// Render a TOML string literal: a literal string when possible (so Windows
/// paths keep their backslashes), else an escaped basic string.
fn toml_string(text: &str) -> String {
    if !text.contains(['\'', '\n', '\r']) {
        return format!("'{text}'");
    }
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{escaped}\"")
}

fn toml_block(binary: &Path, args: &[String]) -> String {
    let rendered: Vec<String> = args.iter().map(|arg| toml_string(arg)).collect();
    format!(
        "{TOML_BEGIN}\n[mcp_servers.ukma]\ncommand = {}\nargs = [{}]\n{TOML_END}\n",
        toml_string(&binary.display().to_string()),
        rendered.join(", ")
    )
}

/// The byte range of the managed block, when the document has one. An
/// unterminated block extends to the end of the document.
fn block_range(text: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        match start {
            None if trimmed == TOML_BEGIN => start = Some(offset),
            Some(begin) if trimmed == TOML_END => return Some((begin, offset + line.len())),
            _ => {}
        }
        offset += line.len();
    }
    start.map(|begin| (begin, text.len()))
}

/// Append (or refresh) the managed `[mcp_servers.ukma]` block of a TOML file.
pub fn install_toml(
    path: &Path,
    binary: &Path,
    args: &[String],
    dry_run: bool,
) -> Result<Plan, SetupError> {
    let created = !path.exists();
    let existing = if created {
        String::new()
    } else {
        fs::read_to_string(path).map_err(|error| io_error("read", path, &error))?
    };
    let block = toml_block(binary, args);
    let after = match block_range(&existing) {
        Some((begin, end)) => format!("{}{block}{}", &existing[..begin], &existing[end..]),
        None => {
            let mut text = existing.clone();
            if !text.is_empty() {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push('\n');
            }
            text.push_str(&block);
            text
        }
    };
    let changed = after != existing;
    if changed {
        if !dry_run {
            write_text(path, &after)?;
        }
        Ok(Plan {
            path: path.to_path_buf(),
            changed: true,
            created,
            detail: String::new(),
        })
    } else {
        Ok(Plan {
            path: path.to_path_buf(),
            changed: false,
            created,
            detail: String::new(),
        })
    }
}

/// Remove the managed block from a TOML file.
pub fn remove_toml(path: &Path, dry_run: bool) -> Result<Plan, SetupError> {
    if !path.exists() {
        return Ok(Plan::unchanged(path));
    }
    let existing = fs::read_to_string(path).map_err(|error| io_error("read", path, &error))?;
    let Some((begin, end)) = block_range(&existing) else {
        return Ok(Plan::unchanged(path));
    };
    let mut after = format!("{}{}", &existing[..begin], &existing[end..]);
    while after.ends_with("\n\n") {
        after.pop();
    }
    if !dry_run {
        write_text(path, &after)?;
    }
    Ok(Plan {
        path: path.to_path_buf(),
        changed: true,
        created: false,
        detail: String::new(),
    })
}

/// The exact text of a rules file.
pub fn rules_text(prefix: &str, body: &str) -> String {
    let mut text = String::with_capacity(prefix.len() + body.len() + 1);
    text.push_str(prefix);
    text.push_str(body);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Write an agent's rules file.
pub fn write_rules(
    path: &Path,
    prefix: &str,
    body: &str,
    dry_run: bool,
) -> Result<Plan, SetupError> {
    let text = rules_text(prefix, body);
    let existing = if path.exists() {
        Some(fs::read_to_string(path).map_err(|error| io_error("read", path, &error))?)
    } else {
        None
    };
    let changed = existing.as_deref() != Some(text.as_str());
    if changed && !dry_run {
        write_text(path, &text)?;
    }
    Ok(Plan {
        path: path.to_path_buf(),
        changed,
        created: existing.is_none(),
        detail: String::new(),
    })
}

/// Remove an agent's rules file — but only when it is still exactly the file
/// this installer writes, so a user-edited rules file is never deleted.
pub fn remove_rules(
    path: &Path,
    prefix: &str,
    body: &str,
    dry_run: bool,
) -> Result<Plan, SetupError> {
    if !path.exists() {
        return Ok(Plan::unchanged(path));
    }
    let existing = fs::read_to_string(path).map_err(|error| io_error("read", path, &error))?;
    if existing != rules_text(prefix, body) {
        return Ok(Plan {
            path: path.to_path_buf(),
            changed: false,
            created: false,
            detail: "modified by the user; left in place".into(),
        });
    }
    if !dry_run {
        fs::remove_file(path).map_err(|error| io_error("remove", path, &error))?;
    }
    Ok(Plan {
        path: path.to_path_buf(),
        changed: true,
        created: false,
        detail: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document() -> Map<String, Value> {
        let value = json!({
            "numStartups": 7,
            "mcpServers": {
                "other": { "command": "other-bin" }
            }
        });
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    #[test]
    fn the_merge_adds_the_entry_and_keeps_everything_else() {
        let path = Path::new("config.json");
        let mut map = document();
        let entry = json!({"command": "ukma", "args": ["--mcp"]});
        assert!(merge_entry(&mut map, "mcpServers", &entry, path).expect("merge"));
        assert_eq!(map["numStartups"], json!(7));
        assert_eq!(map["mcpServers"]["other"]["command"], "other-bin");
        assert_eq!(map["mcpServers"]["ukma"]["command"], "ukma");
    }

    #[test]
    fn the_merge_is_idempotent() {
        let path = Path::new("config.json");
        let mut map = document();
        let entry = json!({"command": "ukma", "args": ["--mcp"]});
        assert!(merge_entry(&mut map, "mcpServers", &entry, path).expect("first merge"));
        assert!(
            !merge_entry(&mut map, "mcpServers", &entry, path).expect("second merge"),
            "the second merge reports no change"
        );
    }

    #[test]
    fn the_merge_creates_the_container_when_it_is_absent() {
        let path = Path::new("config.json");
        let mut map = Map::new();
        let entry = json!({"command": "ukma", "args": []});
        assert!(merge_entry(&mut map, "servers", &entry, path).expect("merge"));
        assert_eq!(map["servers"]["ukma"]["command"], "ukma");
    }

    #[test]
    fn the_merge_refuses_a_container_of_the_wrong_type() {
        let path = Path::new("config.json");
        let mut map = Map::new();
        map.insert("mcpServers".into(), json!("not an object"));
        let entry = json!({"command": "ukma"});
        let error = merge_entry(&mut map, "mcpServers", &entry, path)
            .expect_err("a non-object container is a configuration error");
        assert!(matches!(error, SetupError::Config { .. }));
    }

    #[test]
    fn toml_literals_keep_windows_paths_intact() {
        assert_eq!(toml_string(r"C:\bin\ukma.exe"), r"'C:\bin\ukma.exe'");
        assert_eq!(toml_string("--mcp"), "'--mcp'");
        // A path with a single quote falls back to an escaped basic string.
        assert_eq!(
            toml_string(r"C:\o'brien\ukma.exe"),
            r#""C:\\o'brien\\ukma.exe""#
        );
    }

    #[test]
    fn the_block_range_covers_the_markers() {
        let text = format!("a = 1\n{TOML_BEGIN}\n[mcp_servers.ukma]\n{TOML_END}\nb = 2\n");
        let (begin, end) = block_range(&text).expect("a managed block");
        assert_eq!(
            &text[begin..end],
            format!("{TOML_BEGIN}\n[mcp_servers.ukma]\n{TOML_END}\n")
        );
    }

    #[test]
    fn an_unterminated_block_extends_to_the_end() {
        let text = format!("a = 1\n{TOML_BEGIN}\n[mcp_servers.ukma]\n");
        let (begin, end) = block_range(&text).expect("a managed block");
        assert_eq!(end, text.len());
        assert_eq!(
            &text[begin..end],
            format!("{TOML_BEGIN}\n[mcp_servers.ukma]\n")
        );
    }

    #[test]
    fn a_document_without_markers_has_no_range() {
        assert_eq!(block_range("a = 1\n"), None);
        assert_eq!(block_range(""), None);
    }

    #[test]
    fn the_toml_block_round_trips() {
        let args = vec![
            "--mcp".to_string(),
            "--index-cwd".to_string(),
            ".".to_string(),
        ];
        let block = toml_block(Path::new("/usr/local/bin/ukma"), &args);
        assert!(block.starts_with(TOML_BEGIN));
        assert!(block.contains("[mcp_servers.ukma]"));
        assert!(block.contains("command = '/usr/local/bin/ukma'"));
        assert!(block.contains("args = ['--mcp', '--index-cwd', '.']"));
        assert!(block.ends_with(&format!("{TOML_END}\n")));
    }

    #[test]
    fn the_rules_text_is_newline_terminated() {
        assert_eq!(rules_text("", "body"), "body\n");
        assert_eq!(rules_text("---\n", "body\n"), "---\nbody\n");
    }
}
