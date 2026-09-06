//! 存储模块（M0 PoC）：SQLite（rusqlite bundled，含 FTS5）
//! 元数据入库；大对象（图片 DIB）按 sha256 内容寻址存文件目录，DB 只存路径。
//! 支持 FTS5 trigram 分词的中文/英文全文搜索（target: 万条 <50ms）。

use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::clipboard_watcher::ClipEvent;

pub struct Store {
    conn: Connection,
    blob_dir: PathBuf,
}

#[derive(Debug, serde::Serialize)]
pub struct ClipRow {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub text: Option<String>,
    pub image_path: Option<String>,
    pub files_json: Option<String>,
    pub html_path: Option<String>,
    pub hash: String,
    pub size: i64,
    pub pinned: bool,
}

/// 待办条目（M2→M3 增强）：优先级/截止日/标签/引动到顶/完成时间
#[derive(Debug, serde::Serialize)]
pub struct TodoRow {
    pub id: i64,
    pub ts: i64,
    pub done: bool,
    pub kind: String,
    pub title: Option<String>,
    pub content: Option<String>,
    pub text: Option<String>,
    pub image_path: Option<String>,
    pub files_json: Option<String>,
    pub size: i64,
    pub priority: i64,
    pub due_at: Option<i64>,
    pub tags: String,
    pub pinned: bool,
    pub done_at: Option<i64>,
}

/// 待办新建载荷（文本/图片路径/文件列表任一）
#[derive(Debug, serde::Deserialize)]
pub struct TodoInput {
    pub kind: String,
    pub title: Option<String>,
    pub content: Option<String>,
    pub text: Option<String>,
    pub image_path: Option<String>,
    pub files_json: Option<String>,
    pub size: i64,
    pub priority: Option<i64>,
    pub due_at: Option<i64>,
    pub tags: Option<String>,
}

impl Store {
    /// 打开（或初始化）数据库。db_path 如 app_data_dir/clipwin.db
    pub fn open(db_path: &std::path::Path, blob_dir: PathBuf) -> rusqlite::Result<Self> {
        std::fs::create_dir_all(&blob_dir).ok();
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS clips(
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                kind TEXT NOT NULL,
                text TEXT,
                image_path TEXT,
                files_json TEXT,
                hash TEXT NOT NULL UNIQUE,
                size INTEGER NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(
                text, content='clips', content_rowid='id', tokenize='trigram'
            );
            CREATE TRIGGER IF NOT EXISTS clips_ai AFTER INSERT ON clips BEGIN
                INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
            END;
            CREATE TRIGGER IF NOT EXISTS clips_ad AFTER DELETE ON clips BEGIN
                INSERT INTO clips_fts(clips_fts, rowid, text)
                VALUES('delete', old.id, old.text);
            END;
            CREATE INDEX IF NOT EXISTS idx_clips_ts ON clips(ts DESC);
            CREATE TABLE IF NOT EXISTS todos(
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                done INTEGER NOT NULL DEFAULT 0,
                kind TEXT NOT NULL,
                text TEXT,
                image_path TEXT,
                files_json TEXT,
                size INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_todos_ts ON todos(ts DESC);
            "
        )?;
        let store = Self { conn, blob_dir };
        store.migrate()?;
        store.migrate_clips()?;
        Ok(store)
    }

    /// 轻量迁移：todos 增列（priority/due_at/tags/pinned/done_at）、settings 表（窗口位置记忆等）
    fn migrate(&self) -> rusqlite::Result<()> {
        let cols: Vec<String> = self
            .conn
            .prepare("PRAGMA table_info(todos)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|x| x.ok())
            .collect();
        for (col, ddl) in [
            ("priority", "ALTER TABLE todos ADD COLUMN priority INTEGER NOT NULL DEFAULT 0"),
            ("due_at", "ALTER TABLE todos ADD COLUMN due_at INTEGER"),
            ("tags", "ALTER TABLE todos ADD COLUMN tags TEXT NOT NULL DEFAULT '[]'"),
            ("pinned", "ALTER TABLE todos ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0"),
            ("done_at", "ALTER TABLE todos ADD COLUMN done_at INTEGER"),
            ("title", "ALTER TABLE todos ADD COLUMN title TEXT"),
            ("content", "ALTER TABLE todos ADD COLUMN content TEXT"),
        ] {
            if !cols.contains(&col.to_string()) {
                self.conn.execute(ddl, [])?;
            }
        }
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings(
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // 存量数据回填：title 为空时取 text 前 50 字符（标题/内容分离后兼容旧条目）
        self.conn.execute(
            "UPDATE todos SET title = COALESCE(NULLIF(TRIM(title), ''),
                CASE WHEN text IS NOT NULL AND TRIM(text) != ''
                     THEN SUBSTR(TRIM(REPLACE(REPLACE(text, CHAR(13), ' '), CHAR(10), ' ')), 1, 50)
                     ELSE '待办' END)
             WHERE title IS NULL OR TRIM(title) = ''",
            [],
        )?;
        Ok(())
    }

    /// M4 迁移：clips 增列（pinned / html_path）
    fn migrate_clips(&self) -> rusqlite::Result<()> {
        let cols: Vec<String> = self
            .conn
            .prepare("PRAGMA table_info(clips)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|x| x.ok())
            .collect();
        for (col, ddl) in [
            ("pinned", "ALTER TABLE clips ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0"),
            ("html_path", "ALTER TABLE clips ADD COLUMN html_path TEXT"),
        ] {
            if !cols.contains(&col.to_string()) {
                self.conn.execute(ddl, [])?;
            }
        }
        Ok(())
    }

    // ---------------- settings 通用 KV（窗口位置记忆等） ----------------

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .unwrap_or(None)
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        self.conn
            .execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .ok();
    }

    /// blob 目录（图片文件存放处）——命令层写入新图片时用
    pub fn blob_dir(&self) -> &std::path::Path {
        &self.blob_dir
    }
}

const TODO_COLS: &str =
    "id, ts, done, kind, title, content, text, image_path, files_json, size, priority, due_at, tags, pinned, done_at";

fn map_todo_row(r: &rusqlite::Row) -> rusqlite::Result<TodoRow> {
    Ok(TodoRow {
        id: r.get(0)?,
        ts: r.get(1)?,
        done: r.get::<_, i64>(2)? != 0,
        kind: r.get(3)?,
        title: r.get(4)?,
        content: r.get(5)?,
        text: r.get(6)?,
        image_path: r.get(7)?,
        files_json: r.get(8)?,
        size: r.get(9)?,
        priority: r.get(10)?,
        due_at: r.get(11)?,
        tags: r.get(12)?,
        pinned: r.get::<_, i64>(13)? != 0,
        done_at: r.get(14)?,
    })
}

/// 本地时区「今日 0 点 ~ 明日 0 点」的毫秒时间戳（今日截止/逾期筛选用）
fn local_day_bounds() -> (i64, i64) {
    use chrono::TimeZone as _;
    let now = chrono::Local::now();
    let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
    let s = chrono::Local
        .from_local_datetime(&start)
        .earliest()
        .map(|t| t.timestamp_millis())
        .unwrap_or(0);
    (s, s + 86_400_000)
}

/// 自动标题：取 text 前 50 字符（空白折叠）；图片/文件给类型描述
fn auto_title(text: &Option<String>, kind: &str) -> Option<String> {
    if let Some(t) = text {
        let folded: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
        let t = folded.trim();
        if !t.is_empty() {
            let cut: String = t.chars().take(50).collect();
            return Some(cut);
        }
    }
    match kind {
        "image" => Some("图片待办".into()),
        "files" => Some("文件待办".into()),
        _ => Some("待办".into()),
    }
}

fn now_ts_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Store {
    /// 插入一条事件（去重：相同 hash 更新时间戳置顶）
    pub fn insert(&mut self, ev: &ClipEvent) -> rusqlite::Result<i64> {
        // 富文本：CF_HTML body 落盘（html_path）；text 同时存在（双格式条目）
        let html_path: Option<String> = ev.html.as_ref().and_then(|raw| {
            let body = parse_cf_html(raw)?;
            let h = hex(&Sha256::digest(body.as_bytes()));
            let p = self.blob_dir.join(format!("{h}.html"));
            if !p.exists() { std::fs::write(&p, body).ok(); }
            Some(p.to_string_lossy().into_owned())
        });
        let (kind, text, image_path, files_json, size): (&str, Option<&str>, Option<String>, Option<String>, i64) =
            match (&ev.text, &ev.dib, &ev.files) {
                (Some(t), _, _) => {
                    let trimmed = t.trim();
                    if trimmed.is_empty() { return Ok(0); }
                    ("text", Some(t), None, None, t.len() as i64)
                }
                (_, Some(dib), _) => {
                    let hash = hex(&Sha256::digest(dib));
                    let path = self.blob_dir.join(format!("{hash}.dib"));
                    if !path.exists() {
                        std::fs::write(&path, dib).ok();
                    }
                    // 同步写一份 PNG（前端 <img> 可直渲染 DIB 不能）
                    let png = self.blob_dir.join(format!("{hash}.png"));
                    if !png.exists() {
                        if let Some(img) = dib_to_image(dib) {
                            let mut buf = Vec::new();
                            if img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png).is_ok() {
                                std::fs::write(&png, &buf).ok();
                            }
                        }
                    }
                    ("image", None, Some(path.to_string_lossy().into_owned()), None, dib.len() as i64)
                }
                (_, _, Some(files)) => {
                    ("files", None, None, Some(serde_json::to_string(files).unwrap_or_default()), files.len() as i64)
                }
                _ => return Ok(0),
            };
        let hash = match (&ev.text, &ev.dib, &html_path) {
            (Some(t), _, hp) => {
                let mut buf = t.as_bytes().to_vec();
                if let Some(p) = hp {
                    if let Ok(extra) = std::fs::read(p) { buf.extend_from_slice(&extra); }
                }
                hex(&Sha256::digest(&buf))
            }
            (_, Some(dib), _) => hex(&Sha256::digest(dib)),
            (_, _, _) => hex(&Sha256::digest(files_json.as_deref().unwrap_or("").as_bytes())),
        };

        // 去重：命中 UNIQUE 冲突则更新 ts 置顶
        let exists: Option<i64> = self
            .conn
            .query_row("SELECT id FROM clips WHERE hash = ?1", params![hash], |r| r.get(0))
            .optional()?;
        if let Some(id) = exists {
            self.conn.execute("UPDATE clips SET ts = ?1 WHERE id = ?2", params![ev.ts, id])?;
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO clips(ts, kind, text, image_path, files_json, hash, size, html_path)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![ev.ts, kind, text, image_path, files_json, hash, size, html_path],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 全文搜索（FTS5 trigram，中文可搜）。query 为空则返回最新 N 条。
    pub fn search(&self, query: &str, limit: i64) -> rusqlite::Result<Vec<ClipRow>> {
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<ClipRow> {
            Ok(ClipRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                kind: r.get(2)?,
                text: r.get(3)?,
                image_path: r.get(4)?,
                files_json: r.get(5)?,
                hash: r.get(6)?,
                size: r.get(7)?,
                html_path: r.get(8)?,
                pinned: r.get::<_, i64>(9)? != 0,
            })
        };
        if query.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT id, ts, kind, text, image_path, files_json, hash, size, html_path, pinned
                 FROM clips ORDER BY pinned DESC, ts DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit], map_row)?;
            Ok(rows.filter_map(|x| x.ok()).collect())
        } else if query.chars().count() < 3 {
            // 短查询（1-2 字符）：FTS5 trigram 最少 3 字符无法命中，LIKE 兜底
            // （万条级全表扫毫秒级；中文两字词是高频场景，必须可搜）
            let esc = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let like = format!("%{esc}%");
            let mut stmt = self.conn.prepare(
                "SELECT id, ts, kind, text, image_path, files_json, hash, size, html_path, pinned
                 FROM clips WHERE text LIKE ?1 ESCAPE '\\' ORDER BY pinned DESC, ts DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![like, limit], map_row)?;
            Ok(rows.filter_map(|x| x.ok()).collect())
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT c.id, c.ts, c.kind, c.text, c.image_path, c.files_json, c.hash, c.size, c.html_path, c.pinned
                 FROM clips_fts f JOIN clips c ON c.id = f.rowid
                 WHERE clips_fts MATCH ?1 ORDER BY c.pinned DESC, rank LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![query, limit], map_row)?;
            Ok(rows.filter_map(|x| x.ok()).collect())
        }
    }

    /// 回贴置顶：更新 ts 为当前时间（列表按 ts DESC 排序，自动浮到最上面）
pub fn touch(&mut self, id: i64) -> rusqlite::Result<()> {
    self.conn.execute("UPDATE clips SET ts = ?1 WHERE id = ?2", params![now_ts_ms(), id])?;
    Ok(())
}

/// 取一条完整记录（粘贴回填用）
    pub fn get(&self, id: i64) -> rusqlite::Result<Option<ClipRow>> {
        self.conn
            .query_row(
                "SELECT id, ts, kind, text, image_path, files_json, hash, size, html_path, pinned FROM clips WHERE id = ?1",
                params![id],
                |r| {
                    Ok(ClipRow {
                        id: r.get(0)?,
                        ts: r.get(1)?,
                        kind: r.get(2)?,
                        text: r.get(3)?,
                        image_path: r.get(4)?,
                        files_json: r.get(5)?,
                        hash: r.get(6)?,
                        size: r.get(7)?,
                        html_path: r.get(8)?,
                        pinned: r.get::<_, i64>(9)? != 0,
                    })
                },
            )
            .optional()
    }

    /// LRU 清理：保留最新 keep 条，超出部分删除（含 blob 文件）
    pub fn trim(&mut self, keep: i64) -> rusqlite::Result<usize> {
        let victims: Vec<(i64, Option<String>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, image_path FROM clips WHERE pinned = 0 ORDER BY ts DESC LIMIT -1 OFFSET ?1",
            )?;
            let rows = stmt.query_map(params![keep], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.filter_map(|x| x.ok()).collect()
        };
        for (id, path) in &victims {
            self.conn.execute("DELETE FROM clips WHERE id = ?1", params![id])?;
            if let Some(p) = path {
                let hash = std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned());
                // blob 引用计数：还有其他行引用同一文件则不删
                if let Some(hash) = hash {
                    let n: i64 = self.conn.query_row(
                        "SELECT COUNT(*) FROM clips WHERE image_path LIKE ?1",
                        params![format!("%{hash}%")],
                        |r| r.get(0),
                    ).unwrap_or(0);
                    if n == 0 {
                        std::fs::remove_file(p).ok();
                    }
                }
            }
        }
        Ok(victims.len())
    }

    pub fn count(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
    }

    /// 收藏/取消收藏
    pub fn clip_toggle_pin(&self, id: i64) -> rusqlite::Result<bool> {
        self.conn.execute("UPDATE clips SET pinned = 1 - pinned WHERE id = ?1", params![id])?;
        self.conn.query_row("SELECT pinned FROM clips WHERE id = ?1", params![id], |r| r.get::<_, i64>(0))
            .map(|v| v != 0)
    }

    /// 删除单条（blob 文件一并清理；无人引用才删）
    pub fn clip_delete(&self, id: i64) -> rusqlite::Result<()> {
        if let Some(p) = self.conn.query_row(
            "SELECT image_path FROM clips WHERE id = ?1", params![id], |r| r.get::<_, Option<String>>(0),
        ).optional()?.flatten() {
            self.unlink_blob_if_unreferenced(&p);
        }
        if let Some(p) = self.conn.query_row(
            "SELECT html_path FROM clips WHERE id = ?1", params![id], |r| r.get::<_, Option<String>>(0),
        ).optional()?.flatten() {
            self.unlink_blob_if_unreferenced(&p);
        }
        self.conn.execute("DELETE FROM clips WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// blob 文件引用计数（clips+todos 两表）：仅剩当前引用时物理删除
    fn unlink_blob_if_unreferenced(&self, path: &str) {
        if let Some(hash) = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()) {
            let n: i64 = self.conn.query_row(
                "SELECT (SELECT COUNT(*) FROM clips WHERE image_path LIKE ?1 OR html_path LIKE ?1)
                      + (SELECT COUNT(*) FROM todos WHERE image_path LIKE ?1)",
                params![format!("%{hash}%")], |r| r.get(0),
            ).unwrap_or(1);
            if n <= 1 {
                std::fs::remove_file(path).ok();
            }
        }
    }

    /// 清空全部历史（保护收藏；blob 清理仅删未引用文件）
    pub fn clips_clear(&mut self) -> rusqlite::Result<usize> {
        let victims: Vec<(i64, Option<String>, Option<String>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, image_path, html_path FROM clips WHERE pinned = 0",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.filter_map(|x| x.ok()).collect()
        };
        // 先清 DB 引用再删文件（引用计数归零路径）
        self.conn.execute("DELETE FROM clips WHERE pinned = 0", [])?;
        for (_, img, html) in &victims {
            if let Some(p) = img { std::fs::remove_file(p).ok(); }
            if let Some(p) = html { std::fs::remove_file(p).ok(); }
        }
        Ok(victims.len())
    }

    // ---------------- 待办 CRUD（M2） ----------------

    pub fn todos_add(&mut self, input: &TodoInput) -> rusqlite::Result<i64> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // 默认标题：显式 title > text 前 50 字 > kind 描述
        let title = input.title.clone().or_else(|| auto_title(&input.text, &input.kind));
        self.conn.execute(
            "INSERT INTO todos(ts, kind, title, content, text, image_path, files_json, size, priority, due_at, tags)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                ts, input.kind, title, input.content, input.text, input.image_path, input.files_json, input.size,
                input.priority.unwrap_or(0), input.due_at, input.tags.as_deref().unwrap_or("[]")
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 待办列表（M3：pinned→done→priority→due_at→ts 多级排序 + 筛选）
    /// filter: open=未完成（默认） all=全部 today=今日截止 overdue=逾期 done=已完成
    /// tag: 非空则只留包含该标签的条目（tags 为 JSON 数组文本，用 LIKE 匹配）
    pub fn todos_list(&self, filter: &str, tag: &str, include_done: bool) -> rusqlite::Result<Vec<TodoRow>> {
        let mut conds: Vec<String> = Vec::new();
        match filter {
            "all" => {}
            "done" => conds.push("done = 1".into()),
            "today" => {
                let (start, end) = local_day_bounds();
                conds.push(format!("due_at IS NOT NULL AND due_at >= {start} AND due_at < {end}"));
            }
            "overdue" => {
                let (start, _) = local_day_bounds();
                conds.push(format!("done = 0 AND due_at IS NOT NULL AND due_at < {start}"));
            }
            _ => {
                // open：默认只看未完成；include_done=true 为兼容旧「全部含已完成」入口
                if !include_done {
                    conds.push("done = 0".into());
                }
            }
        }
        if !tag.is_empty() {
            // tags 存 JSON 数组文本：["工作","急"]——按完整成员精确匹配
            let esc: String = tag.chars().filter(|c| *c != '"' && *c != '\'').collect();
            let pat = format!("%\"{}\"%", esc);
            conds.push(format!("tags LIKE '{}'", pat));
        }
        let mut sql = format!("SELECT {TODO_COLS} FROM todos");
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" ORDER BY pinned DESC, done ASC, priority DESC, due_at IS NULL, due_at ASC, ts DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_todo_row)?;
        Ok(rows.filter_map(|x| x.ok()).collect())
    }

    /// 待办统计（chips 计数）：open/today/overdue/done + 标签聚合
    pub fn todos_stats(&self) -> rusqlite::Result<serde_json::Value> {
        let (start, end) = local_day_bounds();
        let n = |sql: String| -> i64 { self.conn.query_row(&sql, [], |r| r.get(0)).unwrap_or(0) };
        let open = n("SELECT COUNT(*) FROM todos WHERE done = 0".into());
        let today = n(format!("SELECT COUNT(*) FROM todos WHERE done = 0 AND due_at IS NOT NULL AND due_at >= {start} AND due_at < {end}"));
        let overdue = n(format!("SELECT COUNT(*) FROM todos WHERE done = 0 AND due_at IS NOT NULL AND due_at < {start}"));
        let done = n("SELECT COUNT(*) FROM todos WHERE done = 1".into());
        let mut stmt = self.conn.prepare("SELECT tags FROM todos WHERE done = 0")?;
        let rows: Vec<String> = stmt.query_map([], |r| r.get(0))?.filter_map(|x| x.ok()).collect();
        let mut tags: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        for t in rows {
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&t) {
                for tag in arr {
                    *tags.entry(tag).or_insert(0) += 1;
                }
            }
        }
        Ok(serde_json::json!({ "open": open, "today": today, "overdue": overdue, "done": done, "tags": tags }))
    }

    pub fn todos_toggle(&mut self, id: i64) -> rusqlite::Result<bool> {
        // 完成时间戳：勾上写 done_at，取消清空
        self.conn.execute(
            "UPDATE todos SET done = 1 - done, done_at = CASE WHEN 1 - done = 1 THEN ?2 ELSE NULL END WHERE id = ?1",
            params![id, now_ts_ms()],
        )?;
        self.conn
            .query_row("SELECT done FROM todos WHERE id = ?1", params![id], |r| r.get::<_, i64>(0))
            .map(|v| v != 0)
    }

    /// 编辑弹窗保存：标题 + 富文本内容（HTML）+ 纯文本回写
    pub fn todos_update_full(
        &mut self, id: i64, title: &str, content_html: Option<&str>, text: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE todos SET title = ?2, content = ?3, text = COALESCE(?4, text), size = COALESCE(?5, size) WHERE id = ?1",
            params![id, title, content_html, text, text.map(|t| t.len() as i64)],
        )?;
        Ok(())
    }

    pub fn todos_set_priority(&mut self, id: i64, priority: i64) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE todos SET priority = ?2 WHERE id = ?1", params![id, priority])?;
        Ok(())
    }

    /// due: None = 清除截止日
    pub fn todos_set_due(&mut self, id: i64, due: Option<i64>) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE todos SET due_at = ?2 WHERE id = ?1", params![id, due])?;
        Ok(())
    }

    pub fn todos_toggle_pin(&mut self, id: i64) -> rusqlite::Result<bool> {
        self.conn.execute("UPDATE todos SET pinned = 1 - pinned WHERE id = ?1", params![id])?;
        self.conn
            .query_row("SELECT pinned FROM todos WHERE id = ?1", params![id], |r| r.get::<_, i64>(0))
            .map(|v| v != 0)
    }

    pub fn todos_set_tags(&mut self, id: i64, tags: &str) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE todos SET tags = ?2 WHERE id = ?1", params![id, tags])?;
        Ok(())
    }

    pub fn todos_delete(&mut self, id: i64) -> rusqlite::Result<()> {
        // blob 文件引用计数：clips 里还有引用则不删物理文件
        if let Some(p) = self.conn.query_row(
            "SELECT image_path FROM todos WHERE id = ?1", params![id], |r| r.get::<_, Option<String>>(0),
        ).optional()?.flatten() {
            if let Some(hash) = std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned()) {
                let n: i64 = self.conn.query_row(
                    "SELECT (SELECT COUNT(*) FROM clips WHERE image_path LIKE ?1) + (SELECT COUNT(*) FROM todos WHERE image_path LIKE ?1)",
                    params![format!("%{hash}%")], |r| r.get(0),
                ).unwrap_or(0);
                if n <= 1 {
                    std::fs::remove_file(&p).ok();
                }
            }
        }
        self.conn.execute("DELETE FROM todos WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn todos_get(&self, id: i64) -> rusqlite::Result<Option<TodoRow>> {
        self.conn
            .query_row(
                &format!("SELECT {TODO_COLS} FROM todos WHERE id = ?1"),
                params![id],
                map_todo_row,
            )
            .optional()
    }

    /// 预留：待办 Tab 角标显示未完成数（前端接入后移除 allow）
    #[allow(dead_code)]
    pub fn todos_count_open(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM todos WHERE done = 0", [], |r| r.get(0))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// DIB → PNG 字节（待办粘贴路径用）
pub fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let img = dib_to_image(dib)?;
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png).ok()?;
    Some(buf)
}

/// DIB（含 BITMAPINFOHEADER，剪贴板 CF_DIB/CF_DIBV5 载荷）→ image::DynamicImage
/// 支持 24/32bpp BI_RGB 与 32bpp BI_BITFIELDS（常见 ALPHA 掩码）；1/4/8bpp 调色板不常见，暂不支持
fn dib_to_image(dib: &[u8]) -> Option<image::DynamicImage> {
    use image::{DynamicImage, RgbaImage};
    if dib.len() < 40 { return None; }
    let hdr_size = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
    if dib.len() < hdr_size + 16 { return None; }
    let width = i32::from_le_bytes(dib[4..8].try_into().ok()?) as u32;
    let height_raw = i32::from_le_bytes(dib[8..12].try_into().ok()?);
    if width == 0 || height_raw == 0 { return None; }
    let flip = height_raw > 0; // 正值=自底向上，需翻转
    let height = height_raw.unsigned_abs();
    let bpp = u16::from_le_bytes(dib[14..16].try_into().ok()?) as u32;
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    if compression != 0 && compression != 3 { return None; } // BI_RGB / BI_BITFIELDS
    // 像素起点：头长度 + BI_BITFIELDS 掩码表 12 字节（DIBV5 的 124 头已含在 hdr_size）
    let px_off = if compression == 3 { hdr_size + 12 } else { hdr_size };
    let (bpr, has_alpha) = match bpp {
        32 => ((width as usize * 4 + 3) & !3, true),
        24 => (((width as usize * 3) + 3) & !3, false),
        _ => return None,
    };
    if dib.len() < px_off + bpr * height as usize { return None; }
    let mut out = RgbaImage::new(width, height);
    for y in 0..height as usize {
        let src_y = if flip { height as usize - 1 - y } else { y };
        let row = &dib[px_off + src_y * bpr..];
        for x in 0..width as usize {
            let p = &row[x * (bpp as usize / 8)..];
            let (b, g, r, a) = match bpp {
                32 => (p[0], p[1], p[2], if has_alpha { p[3] } else { 255 }),
                _ => (p[0], p[1], p[2], 255),
            };
            out.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, a]));
        }
    }
    Some(DynamicImage::ImageRgba8(out))
}

/// 解析 CF_HTML（"HTML Format"）：头几行是 "Version:0.9\r\nStartHtml:xxx\r\nEndHtml:yyy\r\nStartFragment:..\r\nEndFragment:.."
/// 取 StartHtml..EndHtml 之间字节（完整 <html> 文档），失败返回 None
pub fn parse_cf_html(raw: &[u8]) -> Option<String> {
    let header = String::from_utf8_lossy(&raw[..raw.len().min(512)]).into_owned();
    // 按行取值：找到 "Key:" 后截到行尾（\r\n），再 parse——否则后续行会混进数字导致解析失败
    let field = |key: &str| -> Option<usize> {
        let i = header.find(key)?;
        let rest = &header[i + key.len()..];
        let line = rest.split(['\r', '\n']).next()?;
        line.trim().parse::<usize>().ok()
    };
    let start = field("StartHtml:")?;
    let end = field("EndHtml:")?;
    if end <= start || end > raw.len() {
        // 头解析失败：整体当 HTML（多数应用仍可粘贴）
        let s = String::from_utf8_lossy(raw);
        return if s.contains('<') { Some(s.into_owned()) } else { None };
    }
    let mut s = String::from_utf8_lossy(&raw[start..end]).into_owned();
    // 规范化：确保是完整文档（无 <html> 包裹则补壳，目标应用兼容性更好）
    if !s.to_lowercase().contains("<html") {
        s = format!("<html><body>{}</body></html>", s);
    }
    Some(s)
}
