//! zestroku-core — LANE B: inventory, access-tier gate, reconciliation.
//! The daemon's brain: policy, inventory, and persistence.
#![forbid(unsafe_code)]

pub mod discovery;
pub mod doctor;
pub mod mutation;
pub mod policy;
pub mod ssdp_io;
pub mod store;
pub mod targets;
pub mod tier;
pub mod version;

use std::collections::BTreeMap;
use std::sync::Arc;

use zestroku_proto::{ActiveApp, Command, InstalledApp, MediaPlayerState};
use zestroku_types::{AccessTier, Device, Error, Result};

pub const DEVICE_STATE_POLL_INTERVAL_SECS: i64 = 30;
pub const DEVICE_STATE_FRESHNESS_SECS: i64 = 60;
pub const DEVICE_STATE_BACKOFF_INITIAL_SECS: i64 = 5;
pub const DEVICE_STATE_BACKOFF_MAX_SECS: i64 = 300;

/// I/O stays at the protocol edge; core owns policy and state.
/// The tier gate: refuses out-of-tier commands BEFORE any I/O.
pub fn check_tier(cmd: &Command, device: &Device) -> Result<()> {
    match (cmd.tier, device.access_tier) {
        (AccessTier::Permissive, AccessTier::Open) => Err(Error::TierRefused {
            cmd: cmd.path.clone(),
            mode: device.access_tier,
        }),
        _ => Ok(()),
    }
}

/// Typed, scrubbed state from one completed daemon poll.
///
/// This deliberately excludes serial number, UDN, MAC, device ID, and raw XML:
/// copying an ECP response here must not turn device identifiers into inventory.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStateSnapshot {
    pub device: Device,
    pub observed_tier: tier::ObservedTier,
    pub active_app: ActiveApp,
    pub apps: AppsSnapshot,
    pub media_player: MediaPlayerSnapshot,
    pub tv_active_channel: Option<bool>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppsSnapshot {
    Available(Vec<InstalledApp>),
    TierRefused,
    EndpointAbsent,
    UnknownForbidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPlayerSnapshot {
    Available(MediaPlayerState),
    TierRefused,
    EndpointAbsent,
    UnknownForbidden,
}

#[derive(Debug, Clone)]
pub struct InventoryStateView {
    pub snapshot: Arc<DeviceStateSnapshot>,
    pub is_fresh: bool,
    pub consecutive_failures: u8,
    pub next_poll_at: i64,
}

#[derive(Debug, Default)]
struct DevicePollSchedule {
    snapshot: Option<Arc<DeviceStateSnapshot>>,
    consecutive_failures: u8,
    next_poll_at: i64,
    /// A poll must keep this generation to publish its snapshot.
    generation: u64,
    /// Mutations invalidate the cached view until a later poll completes.
    invalidated: bool,
}
/// Daemon inventory: discovered devices plus typed, bounded-freshness state.
#[derive(Debug, Default)]
pub struct Inventory {
    devices: BTreeMap<String, Device>,
    poll_state: BTreeMap<String, DevicePollSchedule>,
}

impl Inventory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&mut self, device: Device) {
        self.devices.insert(device.host.clone(), device);
    }

    pub fn get(&self, host: &str) -> Option<&Device> {
        self.devices.get(host)
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn poll_due(&self, device_id: &str, now: i64) -> bool {
        self.poll_generation_if_due(device_id, now).is_some()
    }

    /// Returns the generation a due poll must present when recording state.
    pub fn poll_generation_if_due(&self, device_id: &str, now: i64) -> Option<u64> {
        match self.poll_state.get(device_id) {
            Some(schedule) if now < schedule.next_poll_at => None,
            Some(schedule) => Some(schedule.generation),
            None => Some(0),
        }
    }

    /// Marks cached state stale while retaining its scheduled refresh time.
    pub fn invalidate_state(&mut self, device_id: &str) {
        let schedule = self.poll_state.entry(device_id.to_owned()).or_default();
        schedule.generation = schedule.generation.wrapping_add(1);
        schedule.invalidated = true;
    }

    pub fn record_state(&mut self, snapshot: DeviceStateSnapshot) {
        let id = tier::device_id(&snapshot.device.host, snapshot.device.port);
        self.devices
            .insert(snapshot.device.host.clone(), snapshot.device.clone());
        let next_poll_at = snapshot
            .observed_at
            .saturating_add(DEVICE_STATE_POLL_INTERVAL_SECS);
        let schedule = self.poll_state.entry(id).or_default();
        schedule.next_poll_at = next_poll_at;
        schedule.consecutive_failures = 0;
        schedule.invalidated = false;
        schedule.snapshot = Some(Arc::new(snapshot));
    }

    /// Rejects a poll result if a mutation invalidated it while it was in flight.
    pub fn record_state_for_poll(
        &mut self,
        snapshot: DeviceStateSnapshot,
        generation: u64,
    ) -> bool {
        let id = tier::device_id(&snapshot.device.host, snapshot.device.port);
        if self
            .poll_state
            .get(&id)
            .map_or(0, |schedule| schedule.generation)
            != generation
        {
            return false;
        }
        self.record_state(snapshot);
        true
    }

    pub fn record_poll_failure(&mut self, device_id: &str, now: i64) -> i64 {
        let schedule = self.poll_state.entry(device_id.to_owned()).or_default();
        schedule.consecutive_failures = schedule.consecutive_failures.saturating_add(1);
        let shift = u32::from(schedule.consecutive_failures.saturating_sub(1).min(6));
        let multiplier = 1_i64.checked_shl(shift).unwrap_or(64);
        let delay = DEVICE_STATE_BACKOFF_INITIAL_SECS
            .saturating_mul(multiplier)
            .min(DEVICE_STATE_BACKOFF_MAX_SECS);
        schedule.next_poll_at = now.saturating_add(delay);
        schedule.next_poll_at
    }

    /// Ignores failures from polls made obsolete by a mutation.
    pub fn record_poll_failure_for_poll(
        &mut self,
        device_id: &str,
        now: i64,
        generation: u64,
    ) -> Option<i64> {
        if self
            .poll_state
            .get(device_id)
            .map_or(0, |schedule| schedule.generation)
            != generation
        {
            return None;
        }
        Some(self.record_poll_failure(device_id, now))
    }

    pub fn state_at(&self, device_id: &str, now: i64) -> Option<InventoryStateView> {
        let schedule = self.poll_state.get(device_id)?;
        let snapshot = Arc::clone(schedule.snapshot.as_ref()?);
        let age = now.saturating_sub(snapshot.observed_at);
        Some(InventoryStateView {
            snapshot,
            is_fresh: !schedule.invalidated && (0..=DEVICE_STATE_FRESHNESS_SECS).contains(&age),
            consecutive_failures: schedule.consecutive_failures,
            next_poll_at: schedule.next_poll_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zestroku_proto::query;

    fn device(tier: AccessTier) -> Device {
        Device {
            host: "192.0.2.1".into(),
            port: 8060,
            model_name: "Roku Express 4K+".into(),
            model_number: "3941X2".into(),
            software_version: "15.2.4".into(),
            is_tv: false,
            access_tier: tier,
        }
    }

    #[test]
    fn tier_gate_refuses_permissive_command_on_limited_device() {
        let d = device(AccessTier::Open);
        let cmd = Command {
            method: zestroku_proto::Method::Post,
            path: "/keypress/Home".into(),
            tier: AccessTier::Permissive,
        };
        let err = check_tier(&cmd, &d).unwrap_err();
        assert!(err.to_string().contains("403"));
    }

    #[test]
    fn tier_gate_allows_open_command_on_limited_device() {
        let d = device(AccessTier::Open);
        let cmd = query("device-info");
        check_tier(&cmd, &d).unwrap();
    }

    #[test]
    fn inventory_upsert_and_get() {
        let mut inv = Inventory::new();
        assert!(inv.is_empty());
        inv.upsert(device(AccessTier::Open));
        assert_eq!(inv.len(), 1);
        assert!(inv.get("192.0.2.1").is_some());
        assert!(inv.get("other").is_none());
    }
    fn state_snapshot(device: Device, observed_at: i64) -> DeviceStateSnapshot {
        DeviceStateSnapshot {
            device,
            observed_tier: tier::ObservedTier::Limited,
            active_app: zestroku_proto::ActiveApp {
                app_id: None,
                name: "Roku".into(),
            },
            apps: AppsSnapshot::TierRefused,
            media_player: MediaPlayerSnapshot::TierRefused,
            tv_active_channel: Some(true),
            observed_at,
        }
    }

    #[test]
    fn inventory_state_uses_the_documented_freshness_bound() {
        let mut inventory = Inventory::new();
        let id = tier::device_id("192.0.2.1", 8060);
        assert!(inventory.poll_due(&id, 100));

        inventory.record_state(state_snapshot(device(AccessTier::Open), 100));
        assert!(!inventory.poll_due(&id, 129));
        assert!(inventory.poll_due(&id, 130));
        assert!(inventory.state_at(&id, 160).unwrap().is_fresh);
        assert!(!inventory.state_at(&id, 161).unwrap().is_fresh);
    }

    #[test]
    fn invalidation_stales_snapshot_and_discards_an_inflight_poll() {
        let mut inventory = Inventory::new();
        let id = tier::device_id("192.0.2.1", 8060);
        let first_generation = inventory
            .poll_generation_if_due(&id, 100)
            .expect("initial poll is due");
        assert!(inventory.record_state_for_poll(
            state_snapshot(device(AccessTier::Open), 100),
            first_generation,
        ));
        assert!(inventory.state_at(&id, 100).unwrap().is_fresh);
        assert!(!inventory.poll_due(&id, 101));

        inventory.invalidate_state(&id);
        assert!(!inventory.state_at(&id, 101).unwrap().is_fresh);
        assert!(!inventory.poll_due(&id, 101));
        assert!(
            !inventory.record_state_for_poll(
                state_snapshot(device(AccessTier::Open), 101),
                first_generation,
            ),
            "a poll started before mutation cannot republish stale state"
        );
        assert_eq!(
            inventory.record_poll_failure_for_poll(&id, 101, first_generation),
            None
        );
        assert!(!inventory.poll_due(&id, 129));
        assert!(inventory.poll_due(&id, 130));

        let next_generation = inventory
            .poll_generation_if_due(&id, 130)
            .expect("the next scheduled poll is due");
        assert_ne!(next_generation, first_generation);
        assert!(inventory.record_state_for_poll(
            state_snapshot(device(AccessTier::Open), 131),
            next_generation,
        ));
        assert!(inventory.state_at(&id, 131).unwrap().is_fresh);
    }

    #[test]
    fn inventory_poll_backoff_is_per_device_and_retains_last_snapshot() {
        let mut inventory = Inventory::new();
        let id = tier::device_id("192.0.2.1", 8060);
        let other_id = tier::device_id("192.0.2.2", 8060);
        inventory.record_state(state_snapshot(device(AccessTier::Open), 100));

        assert_eq!(inventory.record_poll_failure(&id, 130), 135);
        assert!(!inventory.poll_due(&id, 134));
        assert!(inventory.poll_due(&id, 135));
        assert!(inventory.poll_due(&other_id, 130));
        assert_eq!(inventory.record_poll_failure(&id, 135), 145);
        assert_eq!(inventory.record_poll_failure(&id, 145), 165);
        assert_eq!(inventory.record_poll_failure(&id, 165), 205);
        assert_eq!(inventory.record_poll_failure(&id, 205), 285);
        assert_eq!(inventory.record_poll_failure(&id, 285), 445);
        assert_eq!(inventory.record_poll_failure(&id, 445), 745);
        assert_eq!(inventory.record_poll_failure(&id, 745), 1_045);
        assert_eq!(
            inventory.state_at(&id, 1_000).unwrap().snapshot.observed_at,
            100
        );
        assert!(!inventory.state_at(&id, 1_000).unwrap().is_fresh);
    }
}
