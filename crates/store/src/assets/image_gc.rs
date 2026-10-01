//! Reclaim obsolete image representations only after atomic ledger rebuild.
use crate::StoreError;
use rusqlite::Connection;
use std::{collections::BTreeSet, path::Path};

pub fn prune_unreferenced_images(conn: &Connection, projection: &Path) -> Result<u64, StoreError> {
    let root = super::asset_root(projection);
    if !root.exists() {
        return Ok(0);
    }
    if !std::fs::symlink_metadata(&root)?.file_type().is_dir() {
        return Err(StoreError::AssetInvalid {
            message: "image cache root is not a regular directory".into(),
        });
    }
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some((digest, extension)) = name.split_once('.') else {
            continue;
        };
        if valid_digest(digest) && matches!(extension, "png" | "jpg" | "gif" | "webp") {
            candidates.push(name.to_owned());
        }
    }
    // Text-only archives must not pay for a full JSON reference scan.
    if candidates.is_empty() {
        return Ok(0);
    }
    let mut live = BTreeSet::new();
    // Authored/collaboration attachments, structured tools and workflow resources
    // live in these typed details. json_tree also visits their rendition references.
    let mut statement = conn.prepare("SELECT j.value FROM message, json_tree(message.detail) AS j WHERE j.key='url' AND j.type='text'
        UNION SELECT j.value FROM message, json_tree(message.tool_detail) AS j WHERE j.key='url' AND j.type='text'
        UNION SELECT j.value FROM workflow_run, json_tree(workflow_run.detail) AS j WHERE j.key='url' AND j.type='text'")?;
    for url in statement.query_map([], |row| row.get::<_, String>(0))? {
        if let Some(name) = url?.strip_prefix(rosetta_core::assets::IMAGE_ASSET_URL_PREFIX) {
            live.insert(name.to_owned());
        }
    }
    // Externalized canonical documents can themselves own image resources.
    // Resolve their committed references before deleting anything; a missing or
    // corrupt document defers the whole cleanup, never its image resources.
    let mut pending: Vec<String> = live
        .iter()
        .filter(|name| name.ends_with(".json"))
        .cloned()
        .collect();
    let mut visited = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(digest) = name
            .strip_suffix(".json")
            .filter(|value| valid_digest(value))
        else {
            continue;
        };
        let path = root.join(&name);
        if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(StoreError::AssetInvalid {
                message: "canonical document is not a regular member".into(),
            });
        }
        let bytes = std::fs::read(path)?;
        if blake3::hash(&bytes).to_hex().as_str() != digest {
            return Err(StoreError::AssetInvalid {
                message: "canonical document failed its content hash".into(),
            });
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| StoreError::AssetInvalid {
                message: format!("invalid canonical document: {error}"),
            })?;
        collect_urls(&value, &mut live, &mut pending);
    }
    let mut removed = 0;
    for name in candidates {
        if live.contains(&name) {
            continue;
        }
        std::fs::remove_file(root.join(name))?;
        removed += 1;
    }
    Ok(removed)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn collect_urls(value: &serde_json::Value, live: &mut BTreeSet<String>, pending: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(name) = object
                .get("url")
                .and_then(|url| url.as_str())
                .and_then(|url| url.strip_prefix(rosetta_core::assets::IMAGE_ASSET_URL_PREFIX))
                && live.insert(name.to_owned())
                && name.ends_with(".json")
            {
                pending.push(name.to_owned());
            }
            for value in object.values() {
                collect_urls(value, live, pending);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_urls(value, live, pending);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_unreferenced_image_members_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db.sqlite");
        let root = super::super::asset_root(&db);
        std::fs::create_dir_all(&root).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE message(detail TEXT, tool_detail TEXT); CREATE TABLE workflow_run(detail TEXT)")
            .unwrap();
        let full = format!("{}.webp", "a".repeat(64));
        let thumb = format!("{}.webp", "b".repeat(64));
        let old = format!("{}.png", "c".repeat(64));
        let text = format!("{}.txt", "d".repeat(64));
        let workflow = format!("{}.webp", "e".repeat(64));
        for name in [&full, &thumb, &old, &text, &workflow] {
            std::fs::write(root.join(name), b"fixture").unwrap();
        }
        let document =
            serde_json::json!({"resources": [{"asset": {"url": format!("/api/assets/{thumb}")}}]})
                .to_string();
        let document_name = format!("{}.json", blake3::hash(document.as_bytes()).to_hex());
        std::fs::write(root.join(&document_name), document).unwrap();
        let detail = serde_json::json!({"assets": [{"url": format!("/api/assets/{full}")}, {"url": format!("/api/assets/{document_name}")} ]});
        conn.execute(
            "INSERT INTO message(detail) VALUES (?)",
            [detail.to_string()],
        )
        .unwrap();
        let workflow_detail = serde_json::json!({"resources": [{"type": "asset", "asset": {"url": format!("/api/assets/{workflow}")}}]});
        conn.execute(
            "INSERT INTO workflow_run(detail) VALUES (?)",
            [workflow_detail.to_string()],
        )
        .unwrap();
        assert_eq!(prune_unreferenced_images(&conn, &db).unwrap(), 1);
        assert!(root.join(workflow).exists());
        assert!(root.join(full).exists());
        assert!(root.join(thumb).exists());
        assert!(root.join(text).exists());
        assert!(!root.join(old).exists());
    }
}
