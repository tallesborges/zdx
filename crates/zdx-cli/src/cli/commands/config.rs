//! Config command handlers.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{DocumentMut, Item, Table, value};
use zdx_engine::config::{self, Config};

pub fn path() {
    println!("{}", config::paths::config_path().display());
}

pub fn init() -> Result<()> {
    let config_path = config::paths::config_path();
    config::Config::init(&config_path)
        .with_context(|| format!("init config at {}", config_path.display()))?;
    println!("Created config at {}", config_path.display());
    Ok(())
}

pub fn generate() -> Result<()> {
    let toml = config::Config::generate()?;
    print!("{toml}");
    Ok(())
}

fn resolve_target_path(local: bool) -> Result<PathBuf> {
    if local {
        let cwd = std::env::current_dir().context("determine current directory")?;
        config::paths::workspace_config_path_for(&cwd)
            .ok_or_else(|| anyhow!("current directory is not inside a workspace project"))
    } else {
        Ok(config::paths::config_path())
    }
}

pub fn get(key: Option<&str>, json: bool) -> Result<()> {
    let config = config::Config::load().context("load configuration")?;
    let toml_val = toml::Value::try_from(&config).context("serialize configuration to toml")?;

    if let Some(raw_key) = key {
        let parts = parse_key_path(raw_key)?;
        let mut curr = &toml_val;
        for part in &parts {
            if let Some(tbl) = curr.as_table() {
                if let Some(v) = tbl.get(part) {
                    curr = v;
                } else {
                    bail!("key '{raw_key}' not found in configuration");
                }
            } else if let Some(arr) = curr.as_array() {
                if let Ok(idx) = part.parse::<usize>() {
                    if let Some(v) = arr.get(idx) {
                        curr = v;
                    } else {
                        bail!("index {idx} out of bounds in key '{raw_key}'");
                    }
                } else {
                    bail!("expected array index at '{part}' in '{raw_key}'");
                }
            } else {
                bail!("cannot traverse into non-table/non-array at '{part}' in '{raw_key}'");
            }
        }

        if json {
            let json_val = serde_json::to_value(curr)?;
            println!("{}", serde_json::to_string_pretty(&json_val)?);
        } else {
            match curr {
                toml::Value::String(s) => println!("{s}"),
                toml::Value::Integer(i) => println!("{i}"),
                toml::Value::Float(f) => println!("{f}"),
                toml::Value::Boolean(b) => println!("{b}"),
                toml::Value::Datetime(d) => println!("{d}"),
                toml::Value::Array(_) => {
                    let mut wrapper = toml::value::Table::new();
                    let field_name = parts.last().map_or("item", String::as_str);
                    wrapper.insert(field_name.to_string(), curr.clone());
                    let s = toml::to_string_pretty(&wrapper)?;
                    if let Some(stripped) = s.strip_prefix(&format!("{field_name} = ")) {
                        print!("{stripped}");
                    } else if let Some(stripped) = s.strip_prefix(&format!("{field_name} =")) {
                        print!("{stripped}");
                    } else {
                        print!("{s}");
                    }
                }
                toml::Value::Table(_) => {
                    let s = toml::to_string_pretty(curr)?;
                    print!("{s}");
                }
            }
        }
    } else if json {
        let json_val = serde_json::to_value(&config)?;
        println!("{}", serde_json::to_string_pretty(&json_val)?);
    } else {
        let s = toml::to_string_pretty(&toml_val)?;
        print!("{s}");
    }

    Ok(())
}

fn read_or_create_doc(path: &Path) -> Result<DocumentMut> {
    if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;
        content
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse config from {}", path.display()))
    } else {
        Ok(DocumentMut::new())
    }
}

fn write_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    // Secure, atomic write via unique named temp file in the same directory
    let mut temp = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        let _ = fs::set_permissions(temp.path(), perms);
    }

    temp.as_file_mut()
        .write_all(doc.to_string().as_bytes())
        .with_context(|| format!("write to temp config {}", temp.path().display()))?;
    temp.as_file_mut().flush()?;

    temp.persist(path)
        .with_context(|| format!("persist temp config to {}", path.display()))?;
    Ok(())
}

fn parse_cli_value(val_str: &str, force_string: bool) -> toml_edit::Item {
    if force_string {
        return value(val_str);
    }
    if val_str.eq_ignore_ascii_case("true") {
        return value(true);
    }
    if val_str.eq_ignore_ascii_case("false") {
        return value(false);
    }
    if let Ok(num) = val_str.parse::<i64>() {
        return value(num);
    }
    if let Ok(num) = val_str.parse::<f64>()
        && num.is_finite()
        && (val_str.contains('.') || val_str.contains('e') || val_str.contains('E'))
    {
        return value(num);
    }
    value(val_str)
}

fn parse_key_path(raw: &str) -> Result<Vec<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("empty config key");
    }
    let mut parts = Vec::new();
    for part in trimmed.split('.') {
        if part.is_empty() {
            bail!("invalid key '{raw}': empty segment or misplaced dot");
        }
        parts.push(part.to_string());
    }
    Ok(parts)
}

enum MutContainer<'a> {
    Item(&'a mut Item),
    Table(&'a mut Table),
}

pub fn set(key: &str, val_str: &str, local: bool, force_string: bool) -> Result<()> {
    let path = resolve_target_path(local)?;
    let mut doc = read_or_create_doc(&path)?;
    let parts = parse_key_path(key)?;

    let parsed_val = parse_cli_value(val_str, force_string);

    let mut curr = MutContainer::Item(doc.as_item_mut());
    for (i, part) in parts[..parts.len() - 1].iter().enumerate() {
        let next_part = parts.get(i + 1);
        let next_is_idx = next_part.and_then(|p| p.parse::<usize>().ok());

        let is_table = match &curr {
            MutContainer::Item(Item::Table(tbl)) => {
                tbl.contains_key(part) || (part.parse::<usize>().is_err() && next_is_idx.is_none())
            }
            MutContainer::Table(tbl) => {
                tbl.contains_key(part) || (part.parse::<usize>().is_err() && next_is_idx.is_none())
            }
            MutContainer::Item(_) => false,
        };

        if is_table {
            curr = match curr {
                MutContainer::Item(Item::Table(tbl)) | MutContainer::Table(tbl) => {
                    if !tbl.contains_key(part) {
                        tbl[part] = Item::Table(Table::new());
                    }
                    MutContainer::Item(&mut tbl[part])
                }
                MutContainer::Item(_) => bail!("'{part}' is not a table in path '{key}'"),
            };
        } else if let Ok(idx) = part.parse::<usize>() {
            curr = match curr {
                MutContainer::Item(Item::ArrayOfTables(arr)) => {
                    if idx == arr.len() {
                        arr.push(Table::new());
                    }
                    let tbl = arr
                        .get_mut(idx)
                        .ok_or_else(|| anyhow!("array index {idx} out of bounds in '{key}'"))?;
                    MutContainer::Table(tbl)
                }
                MutContainer::Item(Item::Value(toml_edit::Value::Array(_))) => {
                    bail!("cannot traverse into non-table array element {idx} in '{key}'");
                }
                _ => bail!("cannot index '{part}' into non-array in '{key}'"),
            };
        } else if next_is_idx.is_some() {
            // Container doesn't exist yet and next is an index: create ArrayOfTables
            curr = match curr {
                MutContainer::Item(Item::Table(tbl)) | MutContainer::Table(tbl) => {
                    if !tbl.contains_key(part) {
                        tbl[part] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
                    }
                    MutContainer::Item(&mut tbl[part])
                }
                MutContainer::Item(_) => bail!("'{part}' is not a table in path '{key}'"),
            };
        } else {
            bail!("'{part}' is not accessible in path '{key}'");
        }
    }

    let last_part = &parts[parts.len() - 1];
    apply_leaf_value(curr, last_part, parsed_val, key)?;

    // Validate the modified document deserializes as a valid Config if it's the global config
    let doc_str = doc.to_string();
    if !local {
        toml::from_str::<Config>(&doc_str)
            .with_context(|| format!("modified config would be invalid: key '{key}'"))?;
    }

    write_doc(&path, &doc)?;
    println!("Set {key} in {}", path.display());
    Ok(())
}

fn apply_leaf_value(
    curr: MutContainer<'_>,
    last_part: &str,
    parsed_val: Item,
    key: &str,
) -> Result<()> {
    let is_table_target = match &curr {
        MutContainer::Item(Item::Table(tbl)) => {
            tbl.contains_key(last_part) || last_part.parse::<usize>().is_err()
        }
        MutContainer::Table(tbl) => {
            tbl.contains_key(last_part) || last_part.parse::<usize>().is_err()
        }
        MutContainer::Item(_) => false,
    };

    if is_table_target {
        match curr {
            MutContainer::Item(Item::Table(tbl)) | MutContainer::Table(tbl) => {
                tbl[last_part] = parsed_val;
            }
            MutContainer::Item(_) => bail!("cannot set key '{last_part}' on non-table"),
        }
    } else if let Ok(idx) = last_part.parse::<usize>() {
        match curr {
            MutContainer::Item(Item::ArrayOfTables(_)) => {
                bail!("cannot directly replace table in array of tables at index {idx}");
            }
            MutContainer::Item(Item::Value(toml_edit::Value::Array(arr))) => {
                if let Item::Value(v) = parsed_val {
                    match idx.cmp(&arr.len()) {
                        std::cmp::Ordering::Less => {
                            arr.replace(idx, v);
                        }
                        std::cmp::Ordering::Equal => {
                            arr.push(v);
                        }
                        std::cmp::Ordering::Greater => {
                            bail!("array index {idx} out of bounds in '{key}'");
                        }
                    }
                } else {
                    bail!("cannot set non-value into array");
                }
            }
            _ => bail!("cannot set array index '{last_part}' on non-array"),
        }
    } else {
        match curr {
            MutContainer::Item(Item::Table(tbl)) | MutContainer::Table(tbl) => {
                tbl[last_part] = parsed_val;
            }
            MutContainer::Item(_) => bail!("cannot set key '{last_part}' on non-table"),
        }
    }
    Ok(())
}

pub fn unset(key: &str, local: bool) -> Result<()> {
    let path = resolve_target_path(local)?;
    if !path.exists() {
        bail!("config file does not exist at {}", path.display());
    }

    let mut doc = read_or_create_doc(&path)?;
    let parts = parse_key_path(key)?;

    let mut curr = MutContainer::Item(doc.as_item_mut());
    for part in &parts[..parts.len() - 1] {
        let is_table = match &curr {
            MutContainer::Item(Item::Table(tbl)) => {
                tbl.contains_key(part) || part.parse::<usize>().is_err()
            }
            MutContainer::Table(tbl) => tbl.contains_key(part) || part.parse::<usize>().is_err(),
            MutContainer::Item(_) => false,
        };

        if is_table {
            curr = match curr {
                MutContainer::Item(Item::Table(tbl)) | MutContainer::Table(tbl) => {
                    let item = tbl
                        .get_mut(part)
                        .ok_or_else(|| anyhow!("key '{key}' not found in {}", path.display()))?;
                    MutContainer::Item(item)
                }
                MutContainer::Item(_) => bail!("'{part}' is not a table in path '{key}'"),
            };
        } else if let Ok(idx) = part.parse::<usize>() {
            curr = match curr {
                MutContainer::Item(Item::ArrayOfTables(arr)) => {
                    let tbl = arr
                        .get_mut(idx)
                        .ok_or_else(|| anyhow!("array index {idx} out of bounds in '{key}'"))?;
                    MutContainer::Table(tbl)
                }
                MutContainer::Item(Item::Value(toml_edit::Value::Array(_))) => {
                    bail!("cannot traverse into non-table array element {idx} in '{key}'");
                }
                _ => bail!("cannot index '{part}' into non-array in '{key}'"),
            };
        } else {
            bail!("'{part}' is not accessible in path '{key}'");
        }
    }

    let last_part = &parts[parts.len() - 1];
    let is_table_target = match &curr {
        MutContainer::Item(Item::Table(tbl)) => {
            tbl.contains_key(last_part) || last_part.parse::<usize>().is_err()
        }
        MutContainer::Table(tbl) => {
            tbl.contains_key(last_part) || last_part.parse::<usize>().is_err()
        }
        MutContainer::Item(_) => false,
    };

    if is_table_target {
        match curr {
            MutContainer::Item(Item::Table(tbl)) | MutContainer::Table(tbl) => {
                if tbl.remove(last_part).is_none() {
                    bail!("key '{key}' not found in {}", path.display());
                }
            }
            MutContainer::Item(_) => bail!("cannot unset key on non-table"),
        }
    } else if let Ok(idx) = last_part.parse::<usize>() {
        match curr {
            MutContainer::Item(Item::ArrayOfTables(arr)) => {
                if idx < arr.len() {
                    arr.remove(idx);
                } else {
                    bail!("index {idx} out of bounds in '{key}'");
                }
            }
            MutContainer::Item(Item::Value(toml_edit::Value::Array(arr))) => {
                if idx < arr.len() {
                    arr.remove(idx);
                } else {
                    bail!("index {idx} out of bounds in '{key}'");
                }
            }
            _ => bail!("cannot unset array index on non-array"),
        }
    } else {
        match curr {
            MutContainer::Item(Item::Table(tbl)) | MutContainer::Table(tbl) => {
                if tbl.remove(last_part).is_none() {
                    bail!("key '{key}' not found in {}", path.display());
                }
            }
            MutContainer::Item(_) => bail!("cannot unset key on non-table"),
        }
    }

    let doc_str = doc.to_string();
    if !local {
        toml::from_str::<Config>(&doc_str)
            .with_context(|| format!("modified config would be invalid after unsetting '{key}'"))?;
    }

    write_doc(&path, &doc)?;
    println!("Unset {key} from {}", path.display());
    Ok(())
}
