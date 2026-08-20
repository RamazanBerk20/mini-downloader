use anyhow::Result;
use rusqlite::params;

use crate::model::{now, Package};

use super::Db;

impl Db {
    pub fn insert_package(
        &self,
        name: &str,
        category_id: Option<i64>,
        dir: Option<&str>,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO packages (name, category_id, dir, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, category_id, dir, now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_packages(&self) -> Result<Vec<Package>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, name, category_id, dir, status, created_at FROM packages ORDER BY id DESC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(Package {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    category_id: row.get(2)?,
                    dir: row.get(3)?,
                    status: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Drop a package once its last member is removed (keeps the list free of
    /// empty group headers). No-op while any download still references it.
    pub fn delete_package_if_empty(&self, package_id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM packages WHERE id=?1
             AND NOT EXISTS (SELECT 1 FROM downloads WHERE package_id=?1)",
            params![package_id],
        )?;
        Ok(())
    }
}
