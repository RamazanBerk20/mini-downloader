use serde::Serialize;

pub const EV_TICK: &str = "downloads:tick";
pub const EV_STATE: &str = "downloads:state";
pub const EV_COMPLETE: &str = "downloads:complete";
pub const EV_ERROR: &str = "downloads:error";
pub const EV_RECONCILED: &str = "downloads:reconciled";

/// One live-progress row. Batched into a `downloads:tick` array per poll.
#[derive(Serialize, Clone)]
pub struct Tick {
    pub gid: String,
    pub name: String,
    pub completed: i64,
    pub total: i64,
    pub dl_speed: i64,
    pub ul_speed: i64,
    pub connections: i64,
    pub num_seeders: i64,
    pub status: String,
}
