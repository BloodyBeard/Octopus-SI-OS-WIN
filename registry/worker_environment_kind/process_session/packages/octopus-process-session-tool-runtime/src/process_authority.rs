pub use octopus_verifiers::process_authority::*;

#[cfg(all(test, target_os = "macos"))]
use std::collections::BTreeMap;
#[cfg(all(test, target_os = "macos"))]
use octopus_descriptors::ProcessAuthoritySpec;
#[cfg(all(test, target_os = "macos"))]
#[path = "../tests/support/process_sandbox_mode.rs"]
mod sandbox_mode_tests;
