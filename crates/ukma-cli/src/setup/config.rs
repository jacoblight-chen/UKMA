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
    if changed && map.is_empty() {
        // The file held nothing but the managed entry; remove it rather
        // than leaving an empty `{}` document behind.
        if !dry_run {
            fs::remove_file(path).map_err(|error| io_error("remove", path, &error))?;
        }
        return Ok(Plan {
            path: path.to_path_buf(),
            changed: true,
            created: false,
            detail: if dry_run {
                "the file is now empty; it would be removed".to_string()
            } else {
                "the file is now empty; it was removed".to_string()
            },
        });
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

// ------------------------------------------------------------------- YAML ---

/// A YAML file split into lines, keeping the file's own line breaks.
struct YamlText {
    lines: Vec<String>,
    eol: &'static str,
}

impl YamlText {
    fn parse(text: &str) -> Self {
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect();
        while lines.last().map(String::is_empty).unwrap_or(false) {
            lines.pop();
        }
        Self { lines, eol }
    }

    fn render(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let mut text = self.lines.join(self.eol);
        text.push_str(self.eol);
        text
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// `key` or `key: value` at exactly `indent` spaces; `None` when the line is
/// blank, a comment, or indented differently.
fn yaml_key_at(line: &str, indent: usize) -> Option<(&str, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    if line.len() - trimmed.len() != indent {
        return None;
    }
    let (key, rest) = trimmed.split_once(':')?;
    let key = key.trim_end();
    if key.is_empty() || key.contains(' ') {
        return None;
    }
    Some((key, rest.trim()))
}

/// The index range of the block `container:` introduces (children only).
///
/// A block YAML container owns every following line that is blank or indented;
/// the range ends at the first non-blank line at column 0.
fn yaml_block_range(lines: &[String], container: usize) -> (usize, usize) {
    let mut end = container + 1;
    for (offset, line) in lines.iter().enumerate().skip(container + 1) {
        if line.trim().is_empty() || indent_of(line) > 0 {
            end = offset + 1;
        } else {
            break;
        }
    }
    (container + 1, end)
}

/// One line of the managed entry, as `(indent, text)`.
fn yaml_entry_lines(binary: &Path, args: &[String]) -> Vec<String> {
    let mut lines = vec![
        "  ukma:".to_string(),
        format!(
            "    command: {}",
            yaml_scalar(&binary.display().to_string())
        ),
        "    args:".to_string(),
    ];
    for arg in args {
        lines.push(format!("      - {}", yaml_scalar(arg)));
    }
    lines
}

/// A single-quoted YAML scalar: single quotes are the one form that needs no
/// escaping beyond doubling the quote itself, so a Windows path stays literal.
fn yaml_scalar(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The key names a nested entry line declares (indent `base`).
fn yaml_entry_keys(lines: &[String], base: usize) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| yaml_key_at(line, base).map(|(key, _)| key.to_string()))
        .collect()
}

/// Merge a `ukma:` entry into the `mcp_servers:` map of a YAML file.
///
/// The file's own key order, comments and every other server are preserved: only
/// the `ukma:` entry is added or replaced. A `mcp_servers:` written as a one-line
/// mapping is refused — rewriting it would reformat the user's file.
pub fn install_yaml(
    path: &Path,
    container: &str,
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
    let text = YamlText::parse(&existing);
    let entry = yaml_entry_lines(binary, args);

    let container_index = text
        .lines
        .iter()
        .position(|line| matches!(yaml_key_at(line, 0), Some((key, _)) if key == container));
    let after = match container_index {
        None => {
            let mut lines = text.lines.clone();
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("{container}:"));
            lines.extend(entry);
            YamlText {
                lines,
                eol: text.eol,
            }
            .render()
        }
        Some(index) => {
            let (_, value) = yaml_key_at(&text.lines[index], 0).expect("the key was just matched");
            if !value.is_empty() {
                return Err(SetupError::Config {
                    path: path.to_path_buf(),
                    detail: format!(
                        "`{container}` is a one-line mapping (line {}); add the ukma entry \
                         to it yourself, or write `{container}` as a block and re-run",
                        index + 1
                    ),
                });
            }
            let (start, end) = yaml_block_range(&text.lines, index);
            let existing_entry = (start..end).find(|offset| {
                matches!(yaml_key_at(&text.lines[*offset], 2), Some((key, _)) if key == "ukma")
            });
            match existing_entry {
                Some(begin) => {
                    // Replace only the two keys we own; an entry carrying
                    // anything else (timeouts, env, enabled) is the user's.
                    let entry_end = ((begin + 1)..end)
                        .find(|offset| {
                            !text.lines[*offset].trim().is_empty()
                                && indent_of(&text.lines[*offset]) <= 2
                        })
                        .unwrap_or(end);
                    let keys = yaml_entry_keys(&text.lines[begin..entry_end], 4);
                    if keys.iter().any(|key| key != "command" && key != "args") {
                        return Ok(Plan {
                            path: path.to_path_buf(),
                            changed: false,
                            created: false,
                            detail: "the existing ukma entry sets extra keys; left as written"
                                .into(),
                        });
                    }
                    let mut lines = text.lines.clone();
                    lines.splice(begin..entry_end, entry);
                    YamlText {
                        lines,
                        eol: text.eol,
                    }
                    .render()
                }
                None => {
                    let mut lines = text.lines.clone();
                    lines.splice(start..start, entry);
                    YamlText {
                        lines,
                        eol: text.eol,
                    }
                    .render()
                }
            }
        }
    };
    let changed = after != existing;
    if changed && !dry_run {
        write_text(path, &after)?;
    }
    Ok(Plan {
        path: path.to_path_buf(),
        changed,
        created,
        detail: String::new(),
    })
}

/// Remove the `ukma:` entry from a YAML file's `mcp_servers:` map.
pub fn remove_yaml(path: &Path, container: &str, dry_run: bool) -> Result<Plan, SetupError> {
    if !path.exists() {
        return Ok(Plan::unchanged(path));
    }
    let existing = fs::read_to_string(path).map_err(|error| io_error("read", path, &error))?;
    let mut text = YamlText::parse(&existing);
    let Some(index) = text
        .lines
        .iter()
        .position(|line| matches!(yaml_key_at(line, 0), Some((key, _)) if key == container))
    else {
        return Ok(Plan::unchanged(path));
    };
    if !yaml_key_at(&text.lines[index], 0)
        .map(|(_, value)| value.is_empty())
        .unwrap_or(false)
    {
        return Ok(Plan {
            path: path.to_path_buf(),
            changed: false,
            created: false,
            detail: format!("`{container}` is a one-line mapping; left untouched"),
        });
    }
    let (start, end) = yaml_block_range(&text.lines, index);
    let Some(begin) = (start..end).find(
        |offset| matches!(yaml_key_at(&text.lines[*offset], 2), Some((key, _)) if key == "ukma"),
    ) else {
        return Ok(Plan::unchanged(path));
    };
    let entry_end = ((begin + 1)..end)
        .find(|offset| {
            !text.lines[*offset].trim().is_empty() && indent_of(&text.lines[*offset]) <= 2
        })
        .unwrap_or(end);
    let keys = yaml_entry_keys(&text.lines[begin..entry_end], 4);
    if keys.iter().any(|key| key != "command" && key != "args") {
        return Ok(Plan {
            path: path.to_path_buf(),
            changed: false,
            created: false,
            detail: "the ukma entry sets extra keys; left in place".into(),
        });
    }
    text.lines.drain(begin..entry_end);
    // An empty `mcp_servers:` key with no other server is dead weight.
    let (start, end) = yaml_block_range(&text.lines, index);
    let has_children = text.lines[start..end]
        .iter()
        .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'));
    if !has_children {
        text.lines.remove(index);
        while text.lines.last().map(String::is_empty).unwrap_or(false) {
            text.lines.pop();
        }
    }
    let after = text.render();
    if after != existing && !dry_run {
        write_text(path, &after)?;
    }
    Ok(Plan {
        path: path.to_path_buf(),
        changed: after != existing,
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

    // ---------------------------------------------------------------- YAML ---

    fn scratch(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ukma-config-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch");
        path
    }

    fn yaml_args() -> Vec<String> {
        vec!["--mcp".to_string(), "--index-cwd".to_string(), ".".into()]
    }

    #[test]
    fn a_json_removal_of_the_only_entry_removes_the_file() {
        let dir = scratch("json-remove-empty");
        let path = dir.join("mcp.json");
        fs::write(
            &path,
            r#"{"servers": {"ukma": {"command": "ukma", "args": ["--mcp"]}}}"#,
        )
        .expect("seed");
        let removed = remove_json(&path, ConfigFormat::JsonVsCodeServers, false).expect("remove");
        assert!(removed.changed);
        assert!(
            !path.exists(),
            "an emptied file is removed, not left as `{{}}`"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_json_remoal_keeps_a_file_with_user_entries() {
        let dir = scratch("json-remove-keep");
        let path = dir.join("mcp.json");
        let seed = r#"{"servers": {"other": {"command": "other"}, "ukma": {"command": "ukma"}}}"#;
        fs::write(&path, seed).expect("seed");
        let removed = remove_json(&path, ConfigFormat::JsonVsCodeServers, false).expect("remove");
        assert!(removed.changed);
        assert!(path.exists(), "a file with user entries is kept");
        let value: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(value["servers"]["other"]["command"], "other");
        assert!(value["servers"].get("ukma").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_json_removal_is_a_no_op_when_the_entry_is_absent() {
        let dir = scratch("json-remove-none");
        let path = dir.join("mcp.json");
        fs::write(&path, r#"{"servers": {}}"#).expect("seed");
        let removed = remove_json(&path, ConfigFormat::JsonVsCodeServers, false).expect("remove");
        assert!(!removed.changed);
        assert!(path.exists(), "an absent entry does not remove the file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_yaml_install_creates_the_container_and_is_idempotent() {
        let dir = scratch("yaml-create");
        let path = dir.join("config.yaml");
        let plan = install_yaml(
            &path,
            "mcp_servers",
            Path::new("/usr/local/bin/ukma"),
            &yaml_args(),
            false,
        )
        .expect("install");
        assert!(plan.changed && plan.created);

        let text = fs::read_to_string(&path).expect("read");
        assert_eq!(
            text,
            "mcp_servers:\n  ukma:\n    command: '/usr/local/bin/ukma'\n    args:\n      - '--mcp'\n      - '--index-cwd'\n      - '.'\n"
        );

        let again = install_yaml(
            &path,
            "mcp_servers",
            Path::new("/usr/local/bin/ukma"),
            &yaml_args(),
            false,
        )
        .expect("install twice");
        assert!(!again.changed, "a second install changes nothing");
        assert_eq!(fs::read_to_string(&path).expect("read"), text);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_yaml_install_preserves_every_other_server_and_key() {
        let dir = scratch("yaml-preserve");
        let path = dir.join("config.yaml");
        fs::write(
            &path,
            "# Hermes config\nmodel: hermes-3\nmcp_servers:\n  other:\n    command: other-bin\n    args: ['serve']\nplatform_toolsets:\n  cli: []\n",
        )
        .expect("seed");

        let plan = install_yaml(
            &path,
            "mcp_servers",
            Path::new("/opt/ukma"),
            &yaml_args(),
            false,
        )
        .expect("install");
        assert!(plan.changed);
        let text = fs::read_to_string(&path).expect("read");
        assert!(
            text.starts_with("# Hermes config\nmodel: hermes-3\n"),
            "{text}"
        );
        assert!(
            text.contains("  other:\n    command: other-bin\n    args: ['serve']\n"),
            "{text}"
        );
        assert!(
            text.contains("  ukma:\n    command: '/opt/ukma'\n"),
            "{text}"
        );
        assert!(text.ends_with("platform_toolsets:\n  cli: []\n"), "{text}");

        // Removing takes the entry and nothing else.
        let removed = remove_yaml(&path, "mcp_servers", false).expect("remove");
        assert!(removed.changed);
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "# Hermes config\nmodel: hermes-3\nmcp_servers:\n  other:\n    command: other-bin\n    args: ['serve']\nplatform_toolsets:\n  cli: []\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_yaml_install_appends_a_missing_container() {
        let dir = scratch("yaml-append");
        let path = dir.join("config.yaml");
        fs::write(&path, "model: hermes-3\n").expect("seed");
        install_yaml(
            &path,
            "mcp_servers",
            Path::new("/opt/ukma"),
            &yaml_args(),
            false,
        )
        .expect("install");
        let text = fs::read_to_string(&path).expect("read");
        assert_eq!(
            text,
            "model: hermes-3\n\nmcp_servers:\n  ukma:\n    command: '/opt/ukma'\n    args:\n      - '--mcp'\n      - '--index-cwd'\n      - '.'\n"
        );

        // The entry is the only child, so removing it takes the container too.
        assert!(
            remove_yaml(&path, "mcp_servers", false)
                .expect("remove")
                .changed
        );
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "model: hermes-3\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_one_line_mapping_is_refused_rather_than_reformatted() {
        let dir = scratch("yaml-inline");
        let path = dir.join("config.yaml");
        let seed = "mcp_servers: {other: {command: other-bin}}\n";
        fs::write(&path, seed).expect("seed");

        let error = install_yaml(
            &path,
            "mcp_servers",
            Path::new("/opt/ukma"),
            &yaml_args(),
            false,
        )
        .expect_err("an inline mapping is refused");
        assert!(matches!(error, SetupError::Config { .. }), "{error}");
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            seed,
            "the file is untouched"
        );
        let unchanged = remove_yaml(&path, "mcp_servers", false).expect("remove");
        assert!(!unchanged.changed);
        assert!(unchanged.detail.contains("one-line mapping"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_user_entry_with_extra_keys_is_left_alone() {
        let dir = scratch("yaml-extra");
        let path = dir.join("config.yaml");
        let seed = "mcp_servers:\n  ukma:\n    command: custom\n    args: ['x']\n    timeout: 90\n";
        fs::write(&path, seed).expect("seed");

        let plan = install_yaml(
            &path,
            "mcp_servers",
            Path::new("/opt/ukma"),
            &yaml_args(),
            false,
        )
        .expect("install");
        assert!(!plan.changed);
        assert!(plan.detail.contains("left as written"), "{}", plan.detail);
        assert_eq!(fs::read_to_string(&path).expect("read"), seed);

        let removed = remove_yaml(&path, "mcp_servers", false).expect("remove");
        assert!(!removed.changed);
        assert!(removed.detail.contains("left in place"));
        assert_eq!(fs::read_to_string(&path).expect("read"), seed);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dry_run_yaml_install_writes_nothing() {
        let dir = scratch("yaml-dry");
        let path = dir.join("config.yaml");
        let plan = install_yaml(
            &path,
            "mcp_servers",
            Path::new("/opt/ukma"),
            &yaml_args(),
            true,
        )
        .expect("dry run");
        assert!(plan.changed && plan.created);
        assert!(!path.exists(), "a dry run must not create the file");
        let _ = fs::remove_dir_all(&dir);
    }
}
