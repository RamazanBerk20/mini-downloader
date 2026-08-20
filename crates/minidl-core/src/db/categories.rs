use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::model::Category;

use super::schema::DEFAULT_CATEGORIES;
use super::Db;

impl Db {
    pub fn list_categories(&self) -> Result<Vec<Category>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, name, dir, rules, priority FROM categories ORDER BY priority, name",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(Category {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    dir: row.get(2)?,
                    rules: row.get(3)?,
                    priority: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn upsert_category(
        &self,
        name: &str,
        dir: &str,
        rules: &str,
        priority: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        // `RETURNING id` gives the row's id on both insert and update paths.
        let id = conn.query_row(
            "INSERT INTO categories (name, dir, rules, priority) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET dir=?2, rules=?3, priority=?4
             RETURNING id",
            params![name, dir, rules, priority],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub fn move_category(&self, id: i64, direction: i32) -> Result<()> {
        if direction == 0 {
            return Ok(());
        }

        let mut conn = self.lock();
        let mut ids = {
            let mut statement =
                conn.prepare("SELECT id FROM categories ORDER BY priority, name")?;
            let rows = statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
            return Ok(());
        };
        let target = if direction < 0 {
            index.checked_sub(1)
        } else {
            index.checked_add(1).filter(|target| *target < ids.len())
        };
        let Some(target) = target else {
            return Ok(());
        };
        ids.swap(index, target);

        let transaction = conn.transaction()?;
        for (priority, category_id) in ids.into_iter().enumerate() {
            transaction.execute(
                "UPDATE categories SET priority=?1 WHERE id=?2",
                params![priority as i64, category_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_category(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM categories WHERE id=?1", params![id])?;
        Ok(())
    }

    /// Re-add any missing built-in categories (the restore-defaults action).
    pub fn seed_default_categories(&self) -> Result<()> {
        let conn = self.lock();
        for (name, dir, rules) in DEFAULT_CATEGORIES {
            conn.execute(
                "INSERT OR IGNORE INTO categories (name, dir, rules) VALUES (?1, ?2, ?3)",
                params![name, dir, rules],
            )?;
        }
        Ok(())
    }

    /// Reset a category's folder to its built-in default marker.
    pub fn reset_category_dir(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        let name: Option<String> = conn
            .query_row(
                "SELECT name FROM categories WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(name) = name {
            if let Some(default_dir) = DEFAULT_CATEGORIES
                .iter()
                .find(|(default_name, _, _)| *default_name == name)
                .map(|(_, dir, _)| *dir)
            {
                conn.execute(
                    "UPDATE categories SET dir=?1 WHERE id=?2",
                    params![default_dir, id],
                )?;
            }
        }
        Ok(())
    }
}
