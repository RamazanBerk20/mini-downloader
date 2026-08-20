use anyhow::Result;
use rusqlite::params;

use crate::model::Schedule;

use super::Db;

impl Db {
    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, name, action, days_mask, at_minute, speed_limit, enabled FROM schedules ORDER BY at_minute",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(Schedule {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    action: row.get(2)?,
                    days_mask: row.get(3)?,
                    at_minute: row.get(4)?,
                    speed_limit: row.get(5)?,
                    enabled: row.get::<_, i64>(6)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_schedule(
        &self,
        id: Option<i64>,
        name: Option<&str>,
        action: &str,
        days_mask: i64,
        at_minute: i64,
        speed_limit: Option<i64>,
        enabled: bool,
    ) -> Result<i64> {
        let conn = self.lock();
        match id {
            Some(id) => {
                conn.execute(
                    "UPDATE schedules SET name=?1, action=?2, days_mask=?3, at_minute=?4, speed_limit=?5, enabled=?6 WHERE id=?7",
                    params![name, action, days_mask, at_minute, speed_limit, enabled as i64, id],
                )?;
                Ok(id)
            }
            None => {
                conn.execute(
                    "INSERT INTO schedules (name, action, days_mask, at_minute, speed_limit, enabled) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![name, action, days_mask, at_minute, speed_limit, enabled as i64],
                )?;
                Ok(conn.last_insert_rowid())
            }
        }
    }

    pub fn delete_schedule(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM schedules WHERE id=?1", params![id])?;
        Ok(())
    }
}
