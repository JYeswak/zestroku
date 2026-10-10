//! zestroku-core — LANE B: inventory, access-tier gate, reconciliation.
//! The daemon's brain: policy, inventory, and persistence.
#![forbid(unsafe_code)]

pub mod discovery;
pub mod mutation;
pub mod ssdp_io;
pub mod store;

use std::collections::BTreeMap;

use zestroku_proto::Command;
use zestroku_types::{AccessTier, Device, Error, Result};

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

/// LAN inventory: devices keyed by host.
#[derive(Debug, Default)]
pub struct Inventory {
    devices: BTreeMap<String, Device>,
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
}
