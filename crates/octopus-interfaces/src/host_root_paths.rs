use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::InterfaceDescriptor;
use octopus_descriptors::ProcessAuthoritySpec;

pub fn additional_host_root_paths(
    descriptor: &InterfaceDescriptor,
    roots: &BTreeMap<String, PathBuf>,
) -> Result<BTreeMap<String, Vec<PathBuf>>, String> {
    let mut additional = BTreeMap::new();
    for (slot, binding) in descriptor
        .interface_driver_config
        .iter()
        .flat_map(|config| &config.host_slots)
    {
        if binding.additional_paths.is_empty() || !roots.contains_key(slot) {
            continue;
        }
        if roots.get(slot) != Some(&binding.path) {
            return Err(format!(
                "root slot '{slot}' differs from its declared primary path"
            ));
        }
        let mut seen = std::collections::BTreeSet::from([binding.path.clone()]);
        for path in &binding.additional_paths {
            if !path.is_absolute() || !seen.insert(path.clone()) {
                return Err(format!(
                    "root slot '{slot}' contains a relative or repeated path"
                ));
            }
        }
        additional.insert(slot.clone(), binding.additional_paths.clone());
    }
    Ok(additional)
}

pub fn expand_host_root_authority(
    authority: &ProcessAuthoritySpec,
    roots: &BTreeMap<String, PathBuf>,
    additional: &BTreeMap<String, Vec<PathBuf>>,
) -> Result<(ProcessAuthoritySpec, BTreeMap<String, PathBuf>), String> {
    let mut authority = authority.clone();
    let mut expanded = roots.clone();
    for (slot, paths) in additional {
        if !roots.contains_key(slot) {
            return Err(format!(
                "additional paths have no primary root slot '{slot}'"
            ));
        }
        for (index, path) in paths.iter().enumerate() {
            if !path.is_absolute() {
                return Err(format!("additional root for '{slot}' is not absolute"));
            }
            let alias = format!("__root_path_{slot}_{index}");
            if expanded.insert(alias.clone(), path.clone()).is_some() {
                return Err(format!("root path expansion collides with '{alias}'"));
            }
            for slots in [
                &mut authority.read_root_slots,
                &mut authority.write_root_slots,
                &mut authority.executable_root_slots,
                &mut authority.optional_read_root_slots,
                &mut authority.optional_write_root_slots,
                &mut authority.optional_executable_root_slots,
            ] {
                if slots.contains(slot) {
                    slots.insert(alias.clone());
                }
            }
            for paths in [
                &mut authority.read_argument_paths,
                &mut authority.write_argument_paths,
                &mut authority.executable_argument_paths,
            ] {
                for argument in paths {
                    if argument.within_root_slots.contains(slot) {
                        argument.within_root_slots.insert(alias.clone());
                    }
                }
            }
            let subpaths = authority
                .executable_root_subpaths
                .iter()
                .filter(|entry| &entry.root_slot == slot)
                .map(|entry| {
                    let mut copy = entry.clone();
                    copy.root_slot = alias.clone();
                    copy
                })
                .collect::<Vec<_>>();
            authority.executable_root_subpaths.extend(subpaths);
        }
    }
    Ok((authority, expanded))
}
