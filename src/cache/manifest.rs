//! Layer B: per-worktree manifest of `rel_path → ContentHash`.
//!
//! Backed by SQLite in WAL mode. Readers and one writer can operate
//! concurrently; manifests for different worktrees are independent files
//! with no cross-contention.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::cache::types::{ContentHash, ParserVersion};

/// One row in the manifest table.
#[derive(Debug, Clone)]
pub struct ManifestRow {
    pub rel_path: String,
    pub content_hash: ContentHash,
    pub lang: Option<u32>,
    pub size: u64,
    pub mtime_ns: i128,
    pub git_oid: Option<[u8; 20]>,
    pub generation: u64,
}

pub struct Manifest {
    conn: Connection,
    path: PathBuf,
}

impl Manifest {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        // WAL gives us concurrent readers + one writer with no lock escalation.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS manifest (
                rel_path     TEXT PRIMARY KEY,
                content_hash BLOB NOT NULL,
                lang         INTEGER,
                size         INTEGER NOT NULL,
                mtime_ns     INTEGER NOT NULL,
                git_oid      BLOB,
                generation   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS manifest_hash ON manifest(content_hash);
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT
            );
            ",
        )?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get_meta(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn parser_version(&self) -> rusqlite::Result<Option<ParserVersion>> {
        Ok(self
            .get_meta("parser_version")?
            .and_then(|s| s.parse().ok()))
    }

    pub fn set_parser_version(&self, v: ParserVersion) -> rusqlite::Result<()> {
        self.set_meta("parser_version", &v.to_string())
    }

    pub fn generation(&self) -> rusqlite::Result<u64> {
        Ok(self
            .get_meta("generation")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0))
    }

    pub fn bump_generation(&self) -> rusqlite::Result<u64> {
        let g = self.generation()? + 1;
        self.set_meta("generation", &g.to_string())?;
        Ok(g)
    }

    pub fn row_count(&self) -> rusqlite::Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM manifest", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
    }

    pub fn get_row(&self, rel_path: &str) -> rusqlite::Result<Option<ManifestRow>> {
        self.conn
            .query_row(
                "SELECT rel_path, content_hash, lang, size, mtime_ns, git_oid, generation
                 FROM manifest WHERE rel_path = ?1",
                params![rel_path],
                row_from_sql,
            )
            .optional()
    }

    pub fn all_rows(&self) -> rusqlite::Result<Vec<ManifestRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT rel_path, content_hash, lang, size, mtime_ns, git_oid, generation
             FROM manifest",
        )?;
        let rows = stmt
            .query_map([], row_from_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn rows_with_prefix(&self, prefix: &str) -> rusqlite::Result<Vec<ManifestRow>> {
        // GLOB is faster than LIKE for prefix queries and doesn't interpret %.
        let pat = format!("{prefix}*");
        let mut stmt = self.conn.prepare(
            "SELECT rel_path, content_hash, lang, size, mtime_ns, git_oid, generation
             FROM manifest WHERE rel_path GLOB ?1",
        )?;
        let rows = stmt
            .query_map(params![pat], row_from_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Upsert rows in a single transaction.
    pub fn put_rows(&mut self, rows: &[ManifestRow]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO manifest(rel_path,content_hash,lang,size,mtime_ns,git_oid,generation)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(rel_path) DO UPDATE SET
                    content_hash=excluded.content_hash,
                    lang=excluded.lang,
                    size=excluded.size,
                    mtime_ns=excluded.mtime_ns,
                    git_oid=excluded.git_oid,
                    generation=excluded.generation",
            )?;
            for r in rows {
                stmt.execute(params![
                    r.rel_path,
                    r.content_hash.0.as_slice(),
                    r.lang.map(|l| l as i64),
                    r.size as i64,
                    r.mtime_ns as i64,
                    r.git_oid.as_ref().map(|o| o.as_slice()),
                    r.generation as i64,
                ])?;
            }
        }
        tx.commit()
    }

    pub fn delete_row(&mut self, rel_path: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM manifest WHERE rel_path = ?1", params![rel_path])?;
        Ok(())
    }

    pub fn delete_rows(&mut self, rel_paths: &[String]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM manifest WHERE rel_path = ?1")?;
            for p in rel_paths {
                stmt.execute(params![p])?;
            }
        }
        tx.commit()
    }
}

fn row_from_sql(r: &rusqlite::Row) -> rusqlite::Result<ManifestRow> {
    let rel_path: String = r.get(0)?;
    let hash_blob: Vec<u8> = r.get(1)?;
    let mut hash = [0u8; 32];
    if hash_blob.len() == 32 {
        hash.copy_from_slice(&hash_blob);
    }
    let lang: Option<i64> = r.get(2)?;
    let size: i64 = r.get(3)?;
    let mtime_ns: i64 = r.get(4)?;
    let oid_blob: Option<Vec<u8>> = r.get(5)?;
    let git_oid = oid_blob.and_then(|v| {
        if v.len() == 20 {
            let mut o = [0u8; 20];
            o.copy_from_slice(&v);
            Some(o)
        } else {
            None
        }
    });
    let generation: i64 = r.get(6)?;
    Ok(ManifestRow {
        rel_path,
        content_hash: ContentHash(hash),
        lang: lang.map(|l| l as u32),
        size: size as u64,
        mtime_ns: mtime_ns as i128,
        git_oid,
        generation: generation as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::PARSER_VERSION;

    fn row(rel: &str, h: u8) -> ManifestRow {
        ManifestRow {
            rel_path: rel.to_string(),
            content_hash: ContentHash([h; 32]),
            lang: None,
            size: 42,
            mtime_ns: 1000,
            git_oid: None,
            generation: 1,
        }
    }

    #[test]
    fn upsert_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = Manifest::open(&tmp.path().join("m.sqlite")).unwrap();
        m.set_parser_version(PARSER_VERSION).unwrap();
        m.put_rows(&[row("a.rs", 1), row("b.rs", 2)]).unwrap();
        assert_eq!(m.row_count().unwrap(), 2);
        let a = m.get_row("a.rs").unwrap().unwrap();
        assert_eq!(a.content_hash.0[0], 1);
    }

    #[test]
    fn prefix_query() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = Manifest::open(&tmp.path().join("m.sqlite")).unwrap();
        m.put_rows(&[
            row("src/a.rs", 1),
            row("src/b.rs", 2),
            row("tests/c.rs", 3),
        ])
        .unwrap();
        let src = m.rows_with_prefix("src/").unwrap();
        assert_eq!(src.len(), 2);
    }
}
