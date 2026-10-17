//! Owner policy for the mutation choke point (`o0r`).
//!
//! `policy.toml` lives beside the store DB (`$ZESTROKU_DB` sibling, else
//! `~/.local/state/zestroku/policy.toml`). Missing file means the documented
//! defaults below; an unparsable file or out-of-range value is fail-closed
//! ([`Policy::load`] returns `Err` and callers must refuse mutations).
//!
//! Enforcement lives in [`crate::mutation`]; this module is pure policy data:
//! verb classification, allowlists, rate budgets, and quiet hours. Rate
//! *state* lives in the store (`policy_hits` table, migration v7) so limits
//! bind every surface and process through the single choke.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zestroku_types::Error;

/// Documented default: at most 8 keypresses per rolling second, per client.
pub const DEFAULT_KEYPRESS_PER_SEC: u32 = 8;
/// Documented default: at most 10 launches per rolling 60 seconds, per client.
pub const DEFAULT_LAUNCH_PER_MIN: u32 = 10;
/// Verbs the choke point classifies from the ECP command path.
pub const VERB_KEYPRESS: &str = "keypress";
pub const VERB_LAUNCH: &str = "launch";
pub const VERB_INSTALL: &str = "install";
/// Window seconds for launch accounting.
pub const LAUNCH_WINDOW_SECS: i64 = 60;
/// Keypress window is one second.
pub const KEYPRESS_WINDOW_SECS: i64 = 1;
/// Rate rows older than this are pruned on record.
pub const RATE_RETENTION_SECS: i64 = 3_600;

/// Quiet-hours window, UTC hours `[0, 24)`. During the window every mutation
/// needs a token minted inside the window (fresh explicit confirmation);
/// pre-window approvals are refused. Hours are UTC by decision: the choke
/// point takes no local-timezone dependency; operators translate local hours
/// to UTC when writing `policy.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietHours {
    /// Inclusive start hour.
    pub start: u8,
    /// Exclusive end hour; may wrap past midnight.
    pub end: u8,
}

impl QuietHours {
    /// True when `hour` (UTC, 0..24) falls in the window.
    pub fn active_at(&self, hour: u8) -> bool {
        if self.start == self.end {
            return false;
        }
        if self.start < self.end {
            (self.start..self.end).contains(&hour)
        } else {
            hour >= self.start || hour < self.end
        }
    }
}

/// Parsed owner policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Max keypress mutations per rolling second, per client.
    pub keypress_per_sec: u32,
    /// Max launch mutations per rolling 60 seconds, per client.
    pub launch_per_min: u32,
    /// Per-client allowed verbs. A client absent from the map, or a verb
    /// absent from its list, is denied mutations (reads never consult this:
    /// unknown clients keep read-only tools).
    pub allow: HashMap<String, Vec<String>>,
    /// Optional quiet-hours window.
    pub quiet: Option<QuietHours>,
}

impl Default for Policy {
    /// Documented defaults for a missing `policy.toml`: the three known
    /// clients (`cli`, `http`, `mcp`) may use every verb within the default
    /// budgets; no quiet hours.
    fn default() -> Self {
        let verbs = vec![
            VERB_KEYPRESS.to_string(),
            VERB_LAUNCH.to_string(),
            VERB_INSTALL.to_string(),
        ];
        let mut allow = HashMap::new();
        for client in ["cli", "http", "mcp"] {
            allow.insert(client.to_string(), verbs.clone());
        }
        Self {
            keypress_per_sec: DEFAULT_KEYPRESS_PER_SEC,
            launch_per_min: DEFAULT_LAUNCH_PER_MIN,
            allow,
            quiet: None,
        }
    }
}

impl Policy {
    /// Classify the mutation verb from the ECP command path.
    /// Unknown paths yield `None` and are denied by [`Policy::allows`].
    pub fn verb_of(path: &str) -> Option<&'static str> {
        if path.starts_with("/keypress/") {
            Some(VERB_KEYPRESS)
        } else if path.starts_with("/launch/") {
            Some(VERB_LAUNCH)
        } else if path == "/plugin_install" {
            Some(VERB_INSTALL)
        } else {
            None
        }
    }

    /// True when `client` may perform `verb`.
    pub fn allows(&self, client: &str, verb: &str) -> bool {
        self.allow
            .get(client)
            .is_some_and(|verbs| verbs.iter().any(|v| v == verb))
    }

    /// `(budget, window_secs)` for `verb`; installs are unbudgeted
    /// (confirmation-gated sideloads are rare by construction).
    pub fn budget(&self, verb: &str) -> Option<(u32, i64)> {
        match verb {
            VERB_KEYPRESS => Some((self.keypress_per_sec, KEYPRESS_WINDOW_SECS)),
            VERB_LAUNCH => Some((self.launch_per_min, LAUNCH_WINDOW_SECS)),
            _ => None,
        }
    }

    /// Data-dir convention, mirroring `store_db::db_path`: `$ZESTROKU_DB`
    /// sibling, else `~/.local/state/zestroku/policy.toml`.
    pub fn default_path() -> PathBuf {
        let db = std::env::var_os("ZESTROKU_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let mut p = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
                p.push(".local/state/zestroku/actions.db");
                p
            });
        path_for_db(&db)
    }

    /// `policy.toml` beside the given store DB path.
    pub fn path_for_db(db_path: &Path) -> PathBuf {
        path_for_db(db_path)
    }

    /// Load from `path`: missing file yields [`Policy::default`]; any parse
    /// failure or out-of-range value is fail-closed `Err`.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(Error::Malformed(format!("policy.toml unreadable: {error}"))),
        };
        Self::parse(&text)
    }

    /// Load from [`Policy::default_path`].
    pub fn load_default() -> Result<Self, Error> {
        Self::load(&Self::default_path())
    }

    /// Parse supported TOML fields, refusing unknown restrictions instead of ignoring them.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let bad = |m: String| Error::Malformed(format!("policy.toml: {m}"));
        let doc: toml::Table = text.parse().map_err(|e| bad(format!("{e}")))?;
        if let Some(name) = doc.keys().find(|name| {
            !matches!(
                name.as_str(),
                "keypress_per_sec" | "launch_per_min" | "allow" | "quiet"
            )
        }) {
            return Err(Error::Malformed(format!(
                "policy.toml: unknown field {name:?}; expected keypress_per_sec, launch_per_min, allow or quiet"
            )));
        }
        let mut policy = Self::default();
        if let Some(value) = doc.get("keypress_per_sec") {
            policy.keypress_per_sec = value
                .as_integer()
                .filter(|v| *v > 0)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| {
                    bad("`keypress_per_sec` must be a positive integer within u32".into())
                })?;
        }
        if let Some(value) = doc.get("launch_per_min") {
            policy.launch_per_min = value
                .as_integer()
                .filter(|v| *v > 0)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| {
                    bad("`launch_per_min` must be a positive integer within u32".into())
                })?;
        }
        if let Some(allow) = doc.get("allow") {
            let table = allow
                .as_table()
                .ok_or_else(|| bad("`allow` must be a table of client = [verbs]".into()))?;
            let mut map = HashMap::new();
            for (client, verbs) in table {
                let verbs = verbs
                    .as_array()
                    .ok_or_else(|| bad(format!("`allow.{client}` must be an array")))?;
                let mut list = Vec::new();
                for verb in verbs {
                    let verb = verb
                        .as_str()
                        .ok_or_else(|| bad(format!("`allow.{client}` verbs must be strings")))?;
                    if !matches!(verb, "keypress" | "launch" | "install") {
                        return Err(bad(format!("`allow.{client}` unknown verb {verb:?}")));
                    }
                    list.push(verb.to_string());
                }
                map.insert(client.clone(), list);
            }
            policy.allow = map;
        }
        if let Some(quiet) = doc.get("quiet") {
            let table = quiet
                .as_table()
                .ok_or_else(|| bad("`quiet` must be a table with start/end hours".into()))?;
            if let Some(name) = table
                .keys()
                .find(|name| !matches!(name.as_str(), "start" | "end"))
            {
                return Err(Error::Malformed(format!(
                    "policy.toml: unknown quiet field {name:?}; expected start or end"
                )));
            }
            let hour = |name: &str| {
                table
                    .get(name)
                    .and_then(toml::Value::as_integer)
                    .filter(|h| (0..24).contains(h))
                    .ok_or_else(|| bad(format!("`quiet.{name}` must be an hour 0..24")))
                    .map(|h| h as u8)
            };
            policy.quiet = Some(QuietHours {
                start: hour("start")?,
                end: hour("end")?,
            });
        }
        Ok(policy)
    }
}

/// `policy.toml` beside the given store DB path.
pub fn path_for_db(db_path: &Path) -> PathBuf {
    let mut path = db_path.to_path_buf();
    path.set_file_name("policy.toml");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verb_of_classifies_known_paths() {
        assert_eq!(Policy::verb_of("/keypress/Home"), Some("keypress"));
        assert_eq!(Policy::verb_of("/launch/12"), Some("launch"));
        assert_eq!(Policy::verb_of("/plugin_install"), Some("install"));
        assert_eq!(Policy::verb_of("/keypress"), None);
        assert_eq!(Policy::verb_of("/query/active-app"), None);
        assert_eq!(Policy::verb_of(""), None);
    }

    #[test]
    fn default_allows_known_clients_all_verbs() {
        let policy = Policy::default();
        for client in ["cli", "http", "mcp"] {
            for verb in ["keypress", "launch", "install"] {
                assert!(policy.allows(client, verb), "{client}/{verb}");
            }
        }
        assert!(!policy.allows(" stranger ", "keypress"));
        assert!(!policy.allows("cli", "reboot"));
        assert!(policy.quiet.is_none());
    }

    #[test]
    fn parse_overrides_and_rejects_garbage() {
        let policy = Policy::parse(
            "keypress_per_sec = 3\nlaunch_per_min = 2\n[allow]\ncli = [\"keypress\"]\n[quiet]\nstart = 22\nend = 7\n",
        )
        .expect("valid policy parses");
        assert_eq!(policy.keypress_per_sec, 3);
        assert_eq!(policy.launch_per_min, 2);
        assert!(policy.allows("cli", "keypress"));
        assert!(!policy.allows("cli", "launch"));
        assert!(!policy.allows("http", "keypress"));
        let quiet = policy.quiet.expect("quiet parsed");
        assert!(quiet.active_at(23));
        assert!(quiet.active_at(6));
        assert!(!quiet.active_at(7));
        assert!(!quiet.active_at(21));
        for (text, field) in [
            ("[quiet_hours]\nenabled = true\n", "quiet_hours"),
            (
                "launch_per_min = 2\nlaunch_per_minute = 1\n",
                "launch_per_minute",
            ),
            ("[quiet]\nstart = 22\nend = 7\nend_hour = 6\n", "end_hour"),
            ("[quiet]\nstart = 22\nend = 7\nenabled = false\n", "enabled"),
        ] {
            let error = Policy::parse(text).expect_err("unsupported restriction must fail closed");
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
        assert!(Policy::parse("keypress_per_sec = 0\n").is_err());
        assert!(Policy::parse("keypress_per_sec = 5000000000\n").is_err());
        assert!(Policy::parse("[allow]\ncli = \"keypress\"\n").is_err());
        assert!(Policy::parse("[allow]\ncli = [\"reboot\"]\n").is_err());
        assert!(Policy::parse("[quiet]\nstart = 25\nend = 7\n").is_err());
        assert!(Policy::parse("not = [toml\n").is_err());
    }

    #[test]
    fn quiet_hours_wrap_and_degenerate() {
        let day = QuietHours { start: 9, end: 17 };
        assert!(day.active_at(9));
        assert!(!day.active_at(17));
        assert!(!day.active_at(8));
        let empty = QuietHours { start: 7, end: 7 };
        assert!(!empty.active_at(7));
    }

    #[test]
    fn budgets_cover_budgeted_verbs_only() {
        let policy = Policy::default();
        assert_eq!(policy.budget("keypress"), Some((8, 1)));
        assert_eq!(policy.budget("launch"), Some((10, 60)));
        assert_eq!(policy.budget("install"), None);
    }
}
