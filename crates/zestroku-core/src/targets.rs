//! Pure local target resolution for device aliases and smart targets.
//!
//! Exact aliases win, followed by exact stored device identifiers, unique
//! model-class words (`tv`, `stick`), and unique device-name prefixes. The
//! resolver never guesses: ambiguous matches return stable candidate IDs.

use crate::store::{AliasRow, DeviceRow};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetResolutionError {
    #[error("TARGET_NOT_FOUND: {target}")]
    NotFound { target: String },
    #[error("AMBIGUOUS_NAME: {target} matches {candidates}")]
    AmbiguousName { target: String, candidates: String },
}

/// Resolve a user-supplied host or label against the current device inventory.
/// This is pure; callers decide whether not-found labels remain raw hosts.
pub fn resolve_target(
    target: &str,
    devices: &[DeviceRow],
    aliases: &[AliasRow],
) -> Result<DeviceRow, TargetResolutionError> {
    let not_found = || TargetResolutionError::NotFound {
        target: target.to_owned(),
    };
    if target.is_empty() {
        return Err(not_found());
    }

    if let Some(alias) = aliases.iter().find(|alias| alias.alias == target) {
        return devices
            .iter()
            .find(|device| device.id == alias.device_id)
            .cloned()
            .ok_or_else(not_found);
    }

    let exact: Vec<_> = devices
        .iter()
        .filter(|device| device.id == target || device.ip == target)
        .collect();
    if !exact.is_empty() {
        return select_unique(target, exact);
    }

    if target.eq_ignore_ascii_case("tv") || target.eq_ignore_ascii_case("stick") {
        let matches: Vec<_> = devices
            .iter()
            .filter(|device| model_class_matches(device, target))
            .collect();
        if !matches.is_empty() {
            return select_unique(target, matches);
        }
    }

    let prefix = target.to_ascii_lowercase();
    let matches: Vec<_> = devices
        .iter()
        .filter(|device| device.name.to_ascii_lowercase().starts_with(&prefix))
        .collect();
    if matches.is_empty() {
        Err(not_found())
    } else {
        select_unique(target, matches)
    }
}

fn model_class_matches(device: &DeviceRow, class: &str) -> bool {
    let model = device.model.to_ascii_lowercase();
    let name = device.name.to_ascii_lowercase();
    let model_num = device.model_num.to_ascii_lowercase();
    let is_tv = model.split_whitespace().any(|word| word == "tv")
        || name.split_whitespace().any(|word| word == "tv")
        || model_num.starts_with('l');
    match class.to_ascii_lowercase().as_str() {
        "tv" => is_tv,
        "stick" => {
            !is_tv
                && (model.contains("stick")
                    || name.contains("stick")
                    || model_num.starts_with("38"))
        }
        _ => false,
    }
}

fn select_unique(
    target: &str,
    matches: Vec<&DeviceRow>,
) -> Result<DeviceRow, TargetResolutionError> {
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }
    let mut candidates: Vec<_> = matches
        .into_iter()
        .map(|device| {
            if device.name.is_empty() {
                device.id.clone()
            } else {
                format!("{} ({})", device.name, device.id)
            }
        })
        .collect();
    candidates.sort();
    candidates.dedup();
    Err(TargetResolutionError::AmbiguousName {
        target: target.to_owned(),
        candidates: candidates.join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str, model: &str, model_num: &str) -> DeviceRow {
        DeviceRow {
            id: id.into(),
            name: name.into(),
            ip: id.split(':').next().unwrap_or(id).into(),
            model: model.into(),
            model_num: model_num.into(),
            firmware: "15.0".into(),
            wifi_driver: String::new(),
            access_tier: "permissive".into(),
            last_seen: 1,
        }
    }

    #[test]
    fn exact_alias_precedes_a_model_class_match() {
        let devices = vec![
            row("192.0.2.10:8060", "Living Room", "Roku Express", "3941X2"),
            row("192.0.2.20:8060", "Bedroom TV", "Roku TV", "L816X"),
        ];
        let aliases = vec![AliasRow {
            alias: "tv".into(),
            device_id: devices[0].id.clone(),
        }];
        assert_eq!(
            resolve_target("tv", &devices, &aliases).unwrap().id,
            "192.0.2.10:8060"
        );
    }

    #[test]
    fn unique_class_and_name_prefix_resolve_to_canonical_id() {
        let devices = vec![
            row(
                "192.0.2.10:8060",
                "Living Room Roku",
                "Roku Express",
                "3941X2",
            ),
            row("192.0.2.20:8060", "Bedroom Television", "Roku TV", "L816X"),
            row(
                "192.0.2.30:8060",
                "Office Stick",
                "Streaming Stick",
                "3820X",
            ),
        ];
        assert_eq!(
            resolve_target("tv", &devices, &[]).unwrap().id,
            "192.0.2.20:8060"
        );
        assert_eq!(
            resolve_target("stick", &devices, &[]).unwrap().id,
            "192.0.2.30:8060"
        );
        assert_eq!(
            resolve_target("living r", &devices, &[]).unwrap().id,
            "192.0.2.10:8060"
        );
    }

    #[test]
    fn ambiguous_class_and_prefix_return_sorted_candidates() {
        let devices = vec![
            row("192.0.2.20:8060", "Living Bedroom TV", "Roku TV", "L816X"),
            row("192.0.2.10:8060", "Living Room TV", "Roku TV", "L815X"),
        ];
        let class_error = resolve_target("tv", &devices, &[]).unwrap_err();
        assert_eq!(
            class_error.to_string(),
            "AMBIGUOUS_NAME: tv matches Living Bedroom TV (192.0.2.20:8060), Living Room TV (192.0.2.10:8060)"
        );
        let prefix_error = resolve_target("living", &devices, &[]).unwrap_err();
        assert!(prefix_error.to_string().contains("AMBIGUOUS_NAME"));
    }
}
