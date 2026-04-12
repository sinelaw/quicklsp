//! Global registry: tracks which worktrees exist for each repo.
//!
//! Used by manifest subsumption to answer "do I already have a manifest for
//! a parent or child of the path the user is opening?".

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::cache::identity::{RepoId, WorktreeKey};

pub struct Registry {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct RegisteredWorktree {
    pub repo_id: RepoId,
    pub worktree_key: WorktreeKey,
    pub working_dir: PathBuf,
    pub last_seen: i64,
}

impl Registry {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS worktrees (
                repo_id      BLOB NOT NULL,
                worktree_key BLOB NOT NULL PRIMARY KEY,
                working_dir  TEXT NOT NULL,
                last_seen    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS worktrees_by_repo ON worktrees(repo_id);
            CREATE INDEX IF NOT EXISTS worktrees_by_dir  ON worktrees(working_dir);
            ",
        )?;
        Ok(Self { conn })
    }

    pub fn upsert(
        &self,
        repo_id: &RepoId,
        worktree_key: &WorktreeKey,
        working_dir: &Path,
    ) -> rusqlite::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO worktrees(repo_id,worktree_key,working_dir,last_seen)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(worktree_key) DO UPDATE SET
                working_dir=excluded.working_dir,
                last_seen=excluded.last_seen",
            params![
                repo_id.0.as_slice(),
                worktree_key.0.as_slice(),
                working_dir.to_string_lossy(),
                now,
            ],
        )?;
        Ok(())
    }

    pub fn worktrees_for_repo(
        &self,
        repo_id: &RepoId,
    ) -> rusqlite::Result<Vec<RegisteredWorktree>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo_id, worktree_key, working_dir, last_seen
             FROM worktrees WHERE repo_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![repo_id.0.as_slice()], registered_from_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn registered_from_sql(r: &rusqlite::Row) -> rusqlite::Result<RegisteredWorktree> {
    let repo_blob: Vec<u8> = r.get(0)?;
    let wtk_blob: Vec<u8> = r.get(1)?;
    let dir: String = r.get(2)?;
    let last_seen: i64 = r.get(3)?;
    let mut rid = [0u8; 32];
    if repo_blob.len() == 32 {
        rid.copy_from_slice(&repo_blob);
    }
    let mut wtk = [0u8; 32];
    if wtk_blob.len() == 32 {
        wtk.copy_from_slice(&wtk_blob);
    }
    Ok(RegisteredWorktree {
        repo_id: RepoId(rid),
        worktree_key: WorktreeKey(wtk),
        working_dir: PathBuf::from(dir),
        last_seen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Registry::open(&tmp.path().join("registry.sqlite")).unwrap();
        let repo = RepoId([1; 32]);
        let wk1 = WorktreeKey([2; 32]);
        let wk2 = WorktreeKey([3; 32]);
        reg.upsert(&repo, &wk1, Path::new("/tmp/wt1")).unwrap();
        reg.upsert(&repo, &wk2, Path::new("/tmp/wt2")).unwrap();
        let list = reg.worktrees_for_repo(&repo).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn different_repos_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Registry::open(&tmp.path().join("registry.sqlite")).unwrap();
        let r1 = RepoId([1; 32]);
        let r2 = RepoId([2; 32]);
        reg.upsert(&r1, &WorktreeKey([10; 32]), Path::new("/a")).unwrap();
        reg.upsert(&r2, &WorktreeKey([20; 32]), Path::new("/b")).unwrap();
        assert_eq!(reg.worktrees_for_repo(&r1).unwrap().len(), 1);
        assert_eq!(reg.worktrees_for_repo(&r2).unwrap().len(), 1);
    }
}
