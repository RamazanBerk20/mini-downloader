use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use super::Db;

impl Db {
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT value FROM settings WHERE key=?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// Persist a complete settings draft in one SQLite transaction.
    pub fn set_settings(&self, values: &[(String, String)]) -> Result<()> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=?2",
            )?;
            for (key, value) in values {
                statement.execute(params![key, value])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}
