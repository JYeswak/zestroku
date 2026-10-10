//! Append-only numbered migrations. Never edit or reorder an applied entry;
//! add a new one with the next version.

/// One schema step.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "devices_apps_capabilities_access_tier",
        sql: "CREATE TABLE devices (\
              id TEXT PRIMARY KEY, name TEXT NOT NULL, ip TEXT NOT NULL, \
              model TEXT NOT NULL, model_num TEXT NOT NULL, firmware TEXT NOT NULL, \
              wifi_driver TEXT NOT NULL, access_tier TEXT NOT NULL, last_seen INTEGER NOT NULL);\
              CREATE TABLE apps (\
              device_id TEXT NOT NULL, app_id TEXT NOT NULL, name TEXT NOT NULL, \
              version TEXT NOT NULL, fetched_at INTEGER NOT NULL, \
              PRIMARY KEY (device_id, app_id));\
              CREATE TABLE capabilities (\
              device_id TEXT NOT NULL, cap TEXT NOT NULL, PRIMARY KEY (device_id, cap));\
              CREATE TABLE access_tier (\
              device_id TEXT PRIMARY KEY, tier TEXT NOT NULL, probe_at INTEGER NOT NULL, \
              probe_map TEXT NOT NULL)",
    },
    Migration {
        version: 2,
        name: "actions",
        sql: "CREATE TABLE actions (\
              id INTEGER PRIMARY KEY, at INTEGER NOT NULL, client TEXT NOT NULL, \
              surface TEXT NOT NULL, intent TEXT NOT NULL, decision TEXT NOT NULL, \
              result TEXT NOT NULL, before_state TEXT NOT NULL)",
    },
    Migration {
        version: 3,
        name: "diff_results",
        sql: "CREATE TABLE diff_results (\
              id INTEGER PRIMARY KEY, left_device TEXT NOT NULL, right_device TEXT NOT NULL, \
              surface TEXT NOT NULL, field TEXT NOT NULL, left_value TEXT NOT NULL, \
              right_value TEXT NOT NULL, ran_at INTEGER NOT NULL)",
    },
];

/// A migration list is valid when versions are 1..=N with no gaps or repeats.
pub fn is_contiguous(list: &[Migration]) -> bool {
    list.iter()
        .zip(1i64..)
        .all(|(m, expected)| m.version == expected)
}
