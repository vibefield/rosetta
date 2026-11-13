//! Which projection members the canonical rows name, read in bounded pages.
//!
//! Both walks below visit `message` in windows of [`PAGE`] row ids. Each
//! window is one short read statement, so neither keeps a read transaction
//! open long enough to hold back WAL checkpoints while a live writer
//! commits, and rows whose JSON does not contain `/api/assets/` are skipped
//! before any JSON is parsed:
//! - [`image_work`] is the background rendition sweep's work list. It reads
//!   only attachment arrays (`json_each`), never a whole document tree.
//! - [`referenced_names`] is image GC's reference set. It must find a URL
//!   anywhere (legacy rendition maps, tool and workflow resources), so it
//!   walks the trees of the rows the prefilter keeps.
//!
//! Every writer serializes these columns with serde_json, which never
//! escapes `/`, so the `instr` prefilter cannot miss a reference.

use std::collections::{BTreeMap, BTreeSet};

use rosetta_core::assets::IMAGE_ASSET_URL_PREFIX;
use rosetta_core::timeline::Dimensions;
use rusqlite::Connection;

use super::renditions::image_base;
use crate::StoreError;

/// Message row ids per read statement.
const PAGE: i64 = 4096;

/// One image base the sweep renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageWork {
    /// The base member name, `<digest>.<ext>`.
    pub name: String,
    /// The attachment's recorded dimensions (oriented, fitted to Full).
    pub dimensions: Option<Dimensions>,
}

// One branch per attachment array: authored and tool attachments, and a
// collaboration event's. Legacy WebP bundles carry their own renditions and
// are not work.
const WORK_SQL: &str = "
SELECT s.updated_at, json_extract(a.value, '$.source.asset.url'),
       json_extract(a.value, '$.dimensions.width'), json_extract(a.value, '$.dimensions.height')
  FROM message m, session s, json_each(m.detail, '$.attachments') a
 WHERE m.id > ?1 AND m.id <= ?2 AND instr(m.detail, '/api/assets/') > 0 AND s.id = m.session_id
   AND json_extract(a.value, '$.source.type') = 'asset'
   AND json_type(a.value, '$.source.asset.image_variants') IS NULL
UNION ALL
SELECT s.updated_at, json_extract(a.value, '$.source.asset.url'),
       json_extract(a.value, '$.dimensions.width'), json_extract(a.value, '$.dimensions.height')
  FROM message m, session s, json_each(m.detail, '$.event.attachments') a
 WHERE m.id > ?1 AND m.id <= ?2 AND instr(m.detail, '/api/assets/') > 0 AND s.id = m.session_id
   AND json_extract(a.value, '$.source.type') = 'asset'
   AND json_type(a.value, '$.source.asset.image_variants') IS NULL
UNION ALL
SELECT s.updated_at, json_extract(a.value, '$.source.asset.url'),
       json_extract(a.value, '$.dimensions.width'), json_extract(a.value, '$.dimensions.height')
  FROM message m, session s, json_each(m.tool_detail, '$.attachments') a
 WHERE m.id > ?1 AND m.id <= ?2 AND instr(m.tool_detail, '/api/assets/') > 0 AND s.id = m.session_id
   AND json_extract(a.value, '$.source.type') = 'asset'
   AND json_type(a.value, '$.source.asset.image_variants') IS NULL";

const MESSAGE_URLS_SQL: &str = "
SELECT j.value FROM message m, json_tree(m.detail) j
 WHERE m.id > ?1 AND m.id <= ?2 AND instr(m.detail, '/api/assets/') > 0
   AND j.key = 'url' AND j.type = 'text'
UNION ALL
SELECT j.value FROM message m, json_tree(m.tool_detail) j
 WHERE m.id > ?1 AND m.id <= ?2 AND instr(m.tool_detail, '/api/assets/') > 0
   AND j.key = 'url' AND j.type = 'text'";

const WORKFLOW_URLS_SQL: &str = "
SELECT j.value FROM workflow_run w, json_tree(w.detail) j
 WHERE instr(w.detail, '/api/assets/') > 0 AND j.key = 'url' AND j.type = 'text'";

/// Distinct image bases without a rendition map that attachments name, the
/// most recently updated session's first (then by name). Workflow and
/// canonical-document resources are left to their first view. `stop` is
/// polled before every page; `None` once it returns true, so a stopping
/// caller waits for at most one page, never for the whole projection.
pub fn image_work(
    conn: &Connection,
    stop: &dyn Fn() -> bool,
) -> Result<Option<Vec<ImageWork>>, StoreError> {
    let mut found: BTreeMap<String, (i64, Option<Dimensions>)> = BTreeMap::new();
    let mut statement = conn.prepare(WORK_SQL)?;
    let finished = for_each_page(conn, stop, |low, high| {
        let rows = statement.query_map([low, high], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?;
        for row in rows {
            let (updated, url, width, height) = row?;
            let Some(name) = url.as_deref().and_then(member_name) else {
                continue;
            };
            if image_base(name).is_none() {
                continue;
            }
            let dimensions = width
                .zip(height)
                .and_then(|(w, h)| Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)))
                .map(|(width, height)| Dimensions { width, height });
            let entry = found
                .entry(name.to_owned())
                .or_insert((updated, dimensions));
            entry.0 = entry.0.max(updated);
            entry.1 = entry.1.or(dimensions);
        }
        Ok(())
    })?;
    if !finished {
        return Ok(None);
    }
    let mut work: Vec<_> = found.into_iter().collect();
    work.sort_by(|(a, (a_at, _)), (b, (b_at, _))| b_at.cmp(a_at).then_with(|| a.cmp(b)));
    Ok(Some(
        work.into_iter()
            .map(|(name, (_, dimensions))| ImageWork { name, dimensions })
            .collect(),
    ))
}

/// Every member name a URL in `message.detail`, `message.tool_detail` or
/// `workflow_run.detail` points at, rendition-map leaves included.
/// Canonical documents (`.json` members) can name more; GC resolves them.
pub fn referenced_names(conn: &Connection) -> Result<BTreeSet<String>, StoreError> {
    let mut live = BTreeSet::new();
    let mut insert = |url: String| {
        if let Some(name) = member_name(&url) {
            live.insert(name.to_owned());
        }
    };
    let mut statement = conn.prepare(MESSAGE_URLS_SQL)?;
    // GC needs the whole set: it never stops part way.
    for_each_page(conn, &|| false, |low, high| {
        for url in statement.query_map([low, high], |row| row.get::<_, String>(0))? {
            insert(url?);
        }
        Ok(())
    })?;
    for url in conn
        .prepare(WORKFLOW_URLS_SQL)?
        .query_map([], |row| row.get::<_, String>(0))?
    {
        insert(url?);
    }
    Ok(live)
}

fn member_name(url: &str) -> Option<&str> {
    url.strip_prefix(IMAGE_ASSET_URL_PREFIX)
}

/// Call `page(low, high)` for consecutive `(low, high]` windows of message
/// ids, up to the largest id present when the walk began. False when `stop`
/// ended the walk before a page.
fn for_each_page(
    conn: &Connection,
    stop: &dyn Fn() -> bool,
    mut page: impl FnMut(i64, i64) -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    let (first, last): (Option<i64>, Option<i64>) =
        conn.query_row("SELECT min(id), max(id) FROM message", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let (Some(first), Some(last)) = (first, last) else {
        return Ok(true);
    };
    let mut low = first.saturating_sub(1);
    while low < last {
        if stop() {
            return Ok(false);
        }
        let high = low.saturating_add(PAGE).min(last);
        page(low, high)?;
        low = high;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn schema() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session(id TEXT PRIMARY KEY, updated_at INTEGER NOT NULL);
             CREATE TABLE message(id INTEGER PRIMARY KEY, session_id TEXT, detail TEXT, tool_detail TEXT);
             CREATE TABLE workflow_run(detail TEXT);",
        )
        .unwrap();
        conn
    }

    fn name(seed: char, extension: &str) -> String {
        format!("{}.{extension}", seed.to_string().repeat(64))
    }

    fn attachment(name: &str, dimensions: Option<(u32, u32)>) -> Value {
        json!({
            "id": name, "kind": "image", "label": null, "name": null,
            "media_type": "image/png", "byte_len": 10,
            "source": {"type": "asset", "asset": {
                "url": format!("/api/assets/{name}"), "digest": &name[..64],
                "media_type": "image/png", "byte_len": 10}},
            "dimensions": dimensions.map(|(width, height)| json!({"width": width, "height": height})),
            "message_ranges": [],
        })
    }

    fn bundle(full: &str, leaf: &str) -> Value {
        let mut value = attachment(full, Some((10, 10)));
        value["source"]["asset"]["image_variants"] =
            json!({"rail": {"url": format!("/api/assets/{leaf}")}});
        value
    }

    fn insert(
        conn: &Connection,
        id: i64,
        session: &str,
        detail: Option<Value>,
        tool: Option<Value>,
    ) {
        conn.execute(
            "INSERT INTO message(id, session_id, detail, tool_detail) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                id,
                session,
                detail.map(|v| v.to_string()),
                tool.map(|v| v.to_string())
            ],
        )
        .unwrap();
    }

    #[test]
    fn work_is_distinct_bases_without_renditions_newest_session_first() {
        let conn = schema();
        conn.execute_batch("INSERT INTO session VALUES ('old', 10), ('new', 20)")
            .unwrap();
        let (a, b, c, d) = (
            name('a', "png"),
            name('b', "jpg"),
            name('c', "gif"),
            name('d', "ico"),
        );
        let (legacy, leaf) = (name('e', "webp"), name('f', "webp"));
        insert(
            &conn,
            1,
            "old",
            Some(
                json!({"type": "prose", "attachments": [attachment(&a, Some((40, 30))), bundle(&legacy, &leaf)]}),
            ),
            None,
        );
        insert(
            &conn,
            2,
            "old",
            None,
            Some(
                json!({"type": "file_read", "attachments": [attachment(&b, None),
                attachment(&name('g', "txt"), None)]}),
            ),
        );
        // Far past the first page, and in the newer session: `a` again, a
        // collaboration report's image, and a tool image.
        insert(
            &conn,
            PAGE * 2 + 7,
            "new",
            Some(
                json!({"type": "collaboration", "event": {"attachments": [attachment(&c, Some((2000, 10)))]}}),
            ),
            Some(json!({"type": "file_read", "attachments": [attachment(&d, Some((16, 16)))]})),
        );
        insert(
            &conn,
            PAGE * 2 + 8,
            "new",
            Some(json!({"type": "prose", "attachments": [attachment(&a, Some((40, 30)))]})),
            None,
        );
        insert(
            &conn,
            3,
            "old",
            Some(json!({"type": "prose", "text_member": null})),
            None,
        );
        let work = image_work(&conn, &|| false).unwrap().unwrap();
        let names: Vec<_> = work.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, [&a, &c, &d, &b]);
        assert_eq!(
            work[0].dimensions,
            Some(Dimensions {
                width: 40,
                height: 30
            })
        );
        assert_eq!(work[3].dimensions, None);
    }

    #[test]
    fn a_stop_ends_the_work_walk_between_pages() {
        let conn = schema();
        conn.execute_batch("INSERT INTO session VALUES ('s', 1)")
            .unwrap();
        let image = name('a', "png");
        for id in [1, PAGE * 2 + 7] {
            insert(
                &conn,
                id,
                "s",
                Some(json!({"type": "prose", "attachments": [attachment(&image, None)]})),
                None,
            );
        }
        // Three pages: the stop is asked before each one.
        let asked = std::cell::Cell::new(0);
        let count = || {
            asked.set(asked.get() + 1);
            false
        };
        assert_eq!(image_work(&conn, &count).unwrap().unwrap().len(), 1);
        assert_eq!(asked.get(), 3);
        // Stopping after the first page reads no further and returns no list.
        asked.set(0);
        let after_one = || {
            asked.set(asked.get() + 1);
            asked.get() > 1
        };
        assert_eq!(image_work(&conn, &after_one).unwrap(), None);
        assert_eq!(asked.get(), 2);
    }

    fn video_attachment(name: &str, media_type: &str, dimensions: Option<(u32, u32)>) -> Value {
        let mut value = attachment(name, dimensions);
        value["kind"] = json!("video");
        value["media_type"] = json!(media_type);
        value["source"]["asset"]["media_type"] = json!(media_type);
        value
    }

    /// A video member is a reference like any other, and never rendition
    /// work: the sweep is handed image bases only, so it makes no WebP for a
    /// video wherever a row attaches one.
    #[test]
    fn video_members_are_references_and_never_rendition_work() {
        let conn = schema();
        conn.execute_batch("INSERT INTO session VALUES ('s', 1)")
            .unwrap();
        let (mp4, mov, image) = (name('a', "mp4"), name('b', "mov"), name('c', "png"));
        // A tool row (the sent file), an authored row and a collaboration
        // event, with and without recorded dimensions.
        insert(
            &conn,
            1,
            "s",
            Some(json!({"type": "prose", "attachments": [
                video_attachment(&mov, "video/quicktime", None),
                attachment(&image, Some((40, 30)))]})),
            Some(json!({"type": "other", "attachments": [
                video_attachment(&mp4, "video/mp4", Some((1920, 1080)))]})),
        );
        insert(
            &conn,
            2,
            "s",
            Some(json!({"type": "collaboration", "event": {"attachments": [
                video_attachment(&mp4, "video/mp4", None),
                video_attachment(&mov, "video/quicktime", Some((640, 480)))]}})),
            None,
        );
        let work = image_work(&conn, &|| false).unwrap().unwrap();
        assert_eq!(
            work,
            [ImageWork {
                name: image.clone(),
                dimensions: Some(Dimensions {
                    width: 40,
                    height: 30
                }),
            }]
        );
        assert_eq!(
            referenced_names(&conn).unwrap(),
            [mp4, mov, image].into_iter().collect()
        );
    }

    #[test]
    fn references_cover_every_url_including_rendition_leaves_and_workflows() {
        let conn = schema();
        conn.execute_batch("INSERT INTO session VALUES ('s', 1)")
            .unwrap();
        let (full, leaf, tool, run) = (
            name('a', "webp"),
            name('b', "webp"),
            name('c', "png"),
            name('d', "png"),
        );
        insert(
            &conn,
            PAGE + 1,
            "s",
            Some(json!({"type": "prose", "attachments": [bundle(&full, &leaf)]})),
            Some(
                json!({"type": "web_fetch", "resource": {"asset": {"url": format!("/api/assets/{tool}")}}}),
            ),
        );
        insert(
            &conn,
            1,
            "s",
            Some(json!({"type": "prose", "url": "https://example.com/"})),
            None,
        );
        conn.execute(
            "INSERT INTO workflow_run(detail) VALUES (?1)",
            [
                json!({"resources": [{"asset": {"url": format!("/api/assets/{run}")}}]})
                    .to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            referenced_names(&conn).unwrap(),
            [full, leaf, tool, run].into_iter().collect()
        );
        let empty = schema();
        assert!(referenced_names(&empty).unwrap().is_empty());
        assert!(image_work(&empty, &|| true).unwrap().unwrap().is_empty());
    }
}
