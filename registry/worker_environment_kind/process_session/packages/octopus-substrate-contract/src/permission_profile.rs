use std::io;
use std::num::NonZeroUsize;
use std::path::Path;

use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_pathuri::PathUri;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::ser::Serializer;
use ts_rs::TS;

use crate::permissions::FileSystemAccessMode;
use crate::permissions::FileSystemPath;
use crate::permissions::FileSystemSandboxEntry;
use crate::permissions::FileSystemSandboxKind;
use crate::permissions::FileSystemSandboxPolicy;
use crate::permissions::FileSystemSpecialPath;
use crate::permissions::NetworkSandboxPolicy;
use crate::sandbox_policy::SandboxPolicy;

#[derive(
    Debug, Clone, Copy, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermissions {
    #[default]
    UseDefault,
    RequireEscalated,
    WithAdditionalPermissions,
}

impl SandboxPermissions {
    pub fn requires_escalated_permissions(self) -> bool {
        matches!(self, SandboxPermissions::RequireEscalated)
    }

    pub fn requests_sandbox_override(self) -> bool {
        !matches!(self, SandboxPermissions::UseDefault)
    }

    pub fn uses_additional_permissions(self) -> bool {
        matches!(self, SandboxPermissions::WithAdditionalPermissions)
    }
}

#[derive(Debug, Clone, Eq, Hash, PartialEq, JsonSchema, TS)]
pub struct FileSystemPermissions<PathType = AbsolutePathBuf> {
    pub entries: Vec<FileSystemSandboxEntry<PathType>>,
    pub glob_scan_max_depth: Option<NonZeroUsize>,
}

impl From<FileSystemPermissions<AbsolutePathBuf>> for FileSystemPermissions<PathUri> {
    fn from(value: FileSystemPermissions<AbsolutePathBuf>) -> Self {
        FileSystemPermissions {
            entries: value
                .entries
                .into_iter()
                .map(FileSystemSandboxEntry::<PathUri>::from)
                .collect(),
            glob_scan_max_depth: value.glob_scan_max_depth,
        }
    }
}

impl TryFrom<FileSystemPermissions<PathUri>> for FileSystemPermissions<AbsolutePathBuf> {
    type Error = io::Error;

    fn try_from(value: FileSystemPermissions<PathUri>) -> Result<Self, Self::Error> {
        Ok(FileSystemPermissions {
            entries: value
                .entries
                .into_iter()
                .map(FileSystemSandboxEntry::<AbsolutePathBuf>::try_from)
                .collect::<io::Result<_>>()?,
            glob_scan_max_depth: value.glob_scan_max_depth,
        })
    }
}

impl<PathType> Default for FileSystemPermissions<PathType> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            glob_scan_max_depth: None,
        }
    }
}

pub type LegacyReadWriteRoots<PathType = AbsolutePathBuf> =
    (Option<Vec<PathType>>, Option<Vec<PathType>>);
impl<PathType> FileSystemPermissions<PathType> {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn from_read_write_roots(
        read: Option<Vec<PathType>>,
        write: Option<Vec<PathType>>,
    ) -> Self {
        let mut entries = Vec::new();
        if let Some(read) = read {
            entries.extend(read.into_iter().map(|path| FileSystemSandboxEntry {
                path: FileSystemPath::Path { path },
                access: FileSystemAccessMode::Read,
            }));
        }
        if let Some(write) = write {
            entries.extend(write.into_iter().map(|path| FileSystemSandboxEntry {
                path: FileSystemPath::Path { path },
                access: FileSystemAccessMode::Write,
            }));
        }
        Self {
            entries,
            glob_scan_max_depth: None,
        }
    }

    pub fn legacy_read_write_roots(&self) -> Option<LegacyReadWriteRoots<PathType>>
    where
        PathType: Clone,
    {
        self.as_legacy_permissions()
            .map(|legacy| (legacy.read, legacy.write))
    }

    fn as_legacy_permissions(&self) -> Option<LegacyFileSystemPermissions<PathType>>
    where
        PathType: Clone,
    {
        if self.glob_scan_max_depth.is_some() {
            return None;
        }

        let mut read = Vec::new();
        let mut write = Vec::new();

        for entry in &self.entries {
            let FileSystemPath::Path { path } = &entry.path else {
                return None;
            };
            match entry.access {
                FileSystemAccessMode::Read => read.push(path.clone()),
                FileSystemAccessMode::Write => write.push(path.clone()),
                FileSystemAccessMode::Deny => return None,
            }
        }

        Some(LegacyFileSystemPermissions {
            read: (!read.is_empty()).then_some(read),
            write: (!write.is_empty()).then_some(write),
        })
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(bound(deserialize = "PathType: Deserialize<'de>"))]
struct LegacyFileSystemPermissions<PathType = AbsolutePathBuf> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read: Option<Vec<PathType>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    write: Option<Vec<PathType>>,
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(bound(deserialize = "PathType: Deserialize<'de>"))]
struct CanonicalFileSystemPermissions<PathType = AbsolutePathBuf> {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<FileSystemSandboxEntry<PathType>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    glob_scan_max_depth: Option<NonZeroUsize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum FileSystemPermissionsDe<PathType = AbsolutePathBuf> {
    Canonical(CanonicalFileSystemPermissions<PathType>),
    Legacy(LegacyFileSystemPermissions<PathType>),
}

impl<PathType> Serialize for FileSystemPermissions<PathType>
where
    PathType: Clone + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(legacy) = self.as_legacy_permissions() {
            legacy.serialize(serializer)
        } else {
            CanonicalFileSystemPermissions {
                entries: self.entries.clone(),
                glob_scan_max_depth: self.glob_scan_max_depth,
            }
            .serialize(serializer)
        }
    }
}

impl<'de, PathType> Deserialize<'de> for FileSystemPermissions<PathType>
where
    PathType: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match FileSystemPermissionsDe::deserialize(deserializer)? {
            FileSystemPermissionsDe::Canonical(CanonicalFileSystemPermissions {
                entries,
                glob_scan_max_depth,
            }) => Ok(Self {
                entries,
                glob_scan_max_depth,
            }),
            FileSystemPermissionsDe::Legacy(LegacyFileSystemPermissions { read, write }) => {
                Ok(Self::from_read_write_roots(read, write))
            }
        }
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct NetworkPermissions {
    pub enabled: Option<bool>,
}

impl NetworkPermissions {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AdditionalPermissionProfile {
    pub network: Option<NetworkPermissions>,
    pub file_system: Option<FileSystemPermissions>,
}

impl AdditionalPermissionProfile {
    pub fn is_empty(&self) -> bool {
        self.network.is_none() && self.file_system.is_none()
    }
}

#[derive(
    Debug, Clone, Copy, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEnforcement {
    #[default]
    Managed,
    Disabled,
    External,
}

impl SandboxEnforcement {
    pub fn from_legacy_sandbox_policy(sandbox_policy: &SandboxPolicy) -> Self {
        match sandbox_policy {
            SandboxPolicy::DangerFullAccess => Self::Disabled,
            SandboxPolicy::ExternalSandbox { .. } => Self::External,
            SandboxPolicy::ReadOnly { .. } | SandboxPolicy::WorkspaceWrite { .. } => Self::Managed,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum ManagedFileSystemPermissions<PathType = AbsolutePathBuf> {
    #[serde(rename_all = "snake_case")]
    #[ts(rename_all = "snake_case")]
    Restricted {
        entries: Vec<FileSystemSandboxEntry<PathType>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        glob_scan_max_depth: Option<NonZeroUsize>,
    },
    Unrestricted,
}

impl From<ManagedFileSystemPermissions<AbsolutePathBuf>> for ManagedFileSystemPermissions<PathUri> {
    fn from(value: ManagedFileSystemPermissions<AbsolutePathBuf>) -> Self {
        match value {
            ManagedFileSystemPermissions::Restricted {
                entries,
                glob_scan_max_depth,
            } => ManagedFileSystemPermissions::Restricted {
                entries: entries
                    .into_iter()
                    .map(FileSystemSandboxEntry::<PathUri>::from)
                    .collect(),
                glob_scan_max_depth,
            },
            ManagedFileSystemPermissions::Unrestricted => {
                ManagedFileSystemPermissions::Unrestricted
            }
        }
    }
}

impl TryFrom<ManagedFileSystemPermissions<PathUri>>
    for ManagedFileSystemPermissions<AbsolutePathBuf>
{
    type Error = io::Error;

    fn try_from(value: ManagedFileSystemPermissions<PathUri>) -> Result<Self, Self::Error> {
        Ok(match value {
            ManagedFileSystemPermissions::Restricted {
                entries,
                glob_scan_max_depth,
            } => ManagedFileSystemPermissions::Restricted {
                entries: entries
                    .into_iter()
                    .map(FileSystemSandboxEntry::<AbsolutePathBuf>::try_from)
                    .collect::<io::Result<_>>()?,
                glob_scan_max_depth,
            },
            ManagedFileSystemPermissions::Unrestricted => {
                ManagedFileSystemPermissions::Unrestricted
            }
        })
    }
}

impl ManagedFileSystemPermissions {
    fn from_sandbox_policy(file_system_sandbox_policy: &FileSystemSandboxPolicy) -> Self {
        match file_system_sandbox_policy.kind {
            FileSystemSandboxKind::Restricted => Self::Restricted {
                entries: file_system_sandbox_policy.entries.clone(),
                glob_scan_max_depth: file_system_sandbox_policy
                    .glob_scan_max_depth
                    .and_then(NonZeroUsize::new),
            },
            FileSystemSandboxKind::Unrestricted => Self::Unrestricted,
            FileSystemSandboxKind::ExternalSandbox => unreachable!(
                "external filesystem policies are represented by PermissionProfile::External"
            ),
        }
    }

    pub fn to_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        match self {
            Self::Restricted {
                entries,
                glob_scan_max_depth,
            } => FileSystemSandboxPolicy {
                kind: FileSystemSandboxKind::Restricted,
                glob_scan_max_depth: glob_scan_max_depth.map(usize::from),
                entries: entries.clone(),
            },
            Self::Unrestricted => FileSystemSandboxPolicy::unrestricted(),
        }
    }
}

pub const BUILT_IN_PERMISSION_PROFILE_READ_ONLY: &str = ":read-only";

pub const BUILT_IN_PERMISSION_PROFILE_WORKSPACE: &str = ":workspace";

pub const BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS: &str = ":danger-full-access";

#[derive(Debug, Clone, Eq, PartialEq, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum PermissionProfile<PathType = AbsolutePathBuf> {
    #[serde(rename_all = "snake_case")]
    #[ts(rename_all = "snake_case")]
    Managed {
        file_system: ManagedFileSystemPermissions<PathType>,
        network: NetworkSandboxPolicy,
    },
    Disabled,
    #[serde(rename_all = "snake_case")]
    #[ts(rename_all = "snake_case")]
    External {
        network: NetworkSandboxPolicy,
    },
}

impl From<PermissionProfile<AbsolutePathBuf>> for PermissionProfile<PathUri> {
    fn from(value: PermissionProfile<AbsolutePathBuf>) -> Self {
        match value {
            PermissionProfile::Managed {
                file_system,
                network,
            } => PermissionProfile::Managed {
                file_system: file_system.into(),
                network,
            },
            PermissionProfile::Disabled => PermissionProfile::Disabled,
            PermissionProfile::External { network } => PermissionProfile::External { network },
        }
    }
}

impl TryFrom<PermissionProfile<PathUri>> for PermissionProfile<AbsolutePathBuf> {
    type Error = io::Error;

    fn try_from(value: PermissionProfile<PathUri>) -> Result<Self, Self::Error> {
        Ok(match value {
            PermissionProfile::Managed {
                file_system,
                network,
            } => PermissionProfile::Managed {
                file_system: file_system.try_into()?,
                network,
            },
            PermissionProfile::Disabled => PermissionProfile::Disabled,
            PermissionProfile::External { network } => PermissionProfile::External { network },
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct ActivePermissionProfile {
    pub id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub extends: Option<String>,
}

impl ActivePermissionProfile {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            extends: None,
        }
    }

    pub fn read_only() -> Self {
        Self::new(BUILT_IN_PERMISSION_PROFILE_READ_ONLY)
    }
}

impl<PathType> Default for PermissionProfile<PathType> {
    fn default() -> Self {
        Self::Managed {
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: Vec::new(),
                glob_scan_max_depth: None,
            },
            network: NetworkSandboxPolicy::Restricted,
        }
    }
}

impl PermissionProfile {
    pub fn read_only() -> Self {
        let file_system = FileSystemSandboxPolicy::read_only();
        Self::Managed {
            file_system: ManagedFileSystemPermissions::from_sandbox_policy(&file_system),
            network: NetworkSandboxPolicy::Restricted,
        }
    }

    pub fn workspace_write() -> Self {
        Self::workspace_write_with(&[], NetworkSandboxPolicy::Restricted, false, false)
    }

    pub fn workspace_write_with(
        writable_roots: &[AbsolutePathBuf],
        network: NetworkSandboxPolicy,
        exclude_tmpdir_env_var: bool,
        exclude_slash_tmp: bool,
    ) -> Self {
        let file_system = FileSystemSandboxPolicy::workspace_write(
            writable_roots,
            exclude_tmpdir_env_var,
            exclude_slash_tmp,
        );
        Self::Managed {
            file_system: ManagedFileSystemPermissions::from_sandbox_policy(&file_system),
            network,
        }
    }

    pub fn materialize_project_roots_with_workspace_roots(
        self,
        workspace_roots: &[AbsolutePathBuf],
    ) -> Self {
        match self {
            Self::Managed {
                file_system,
                network,
            } => {
                let file_system = file_system
                    .to_sandbox_policy()
                    .materialize_project_roots_with_workspace_roots(workspace_roots);
                Self::Managed {
                    file_system: ManagedFileSystemPermissions::from_sandbox_policy(&file_system),
                    network,
                }
            }
            Self::Disabled => Self::Disabled,
            Self::External { network } => Self::External { network },
        }
    }

    pub fn from_runtime_permissions(
        file_system_sandbox_policy: &FileSystemSandboxPolicy,
        network_sandbox_policy: NetworkSandboxPolicy,
    ) -> Self {
        let enforcement = match file_system_sandbox_policy.kind {
            FileSystemSandboxKind::Restricted | FileSystemSandboxKind::Unrestricted => {
                SandboxEnforcement::Managed
            }
            FileSystemSandboxKind::ExternalSandbox => SandboxEnforcement::External,
        };
        Self::from_runtime_permissions_with_enforcement(
            enforcement,
            file_system_sandbox_policy,
            network_sandbox_policy,
        )
    }

    pub fn from_runtime_permissions_with_enforcement(
        enforcement: SandboxEnforcement,
        file_system_sandbox_policy: &FileSystemSandboxPolicy,
        network_sandbox_policy: NetworkSandboxPolicy,
    ) -> Self {
        match file_system_sandbox_policy.kind {
            FileSystemSandboxKind::ExternalSandbox => Self::External {
                network: network_sandbox_policy,
            },
            FileSystemSandboxKind::Unrestricted if enforcement == SandboxEnforcement::Disabled => {
                Self::Disabled
            }
            FileSystemSandboxKind::Restricted | FileSystemSandboxKind::Unrestricted => {
                Self::Managed {
                    file_system: ManagedFileSystemPermissions::from_sandbox_policy(
                        file_system_sandbox_policy,
                    ),
                    network: network_sandbox_policy,
                }
            }
        }
    }

    pub fn from_legacy_sandbox_policy(sandbox_policy: &SandboxPolicy) -> Self {
        Self::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(sandbox_policy),
            &FileSystemSandboxPolicy::from(sandbox_policy),
            NetworkSandboxPolicy::from(sandbox_policy),
        )
    }

    pub fn from_legacy_sandbox_policy_for_cwd(sandbox_policy: &SandboxPolicy, cwd: &Path) -> Self {
        Self::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(sandbox_policy),
            &FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(sandbox_policy, cwd),
            NetworkSandboxPolicy::from(sandbox_policy),
        )
    }

    pub fn enforcement(&self) -> SandboxEnforcement {
        match self {
            Self::Managed { .. } => SandboxEnforcement::Managed,
            Self::Disabled => SandboxEnforcement::Disabled,
            Self::External { .. } => SandboxEnforcement::External,
        }
    }

    pub fn file_system_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        match self {
            Self::Managed { file_system, .. } => file_system.to_sandbox_policy(),
            Self::Disabled => FileSystemSandboxPolicy::unrestricted(),
            Self::External { .. } => FileSystemSandboxPolicy::external_sandbox(),
        }
    }

    pub fn network_sandbox_policy(&self) -> NetworkSandboxPolicy {
        match self {
            Self::Managed { network, .. } | Self::External { network } => *network,
            Self::Disabled => NetworkSandboxPolicy::Enabled,
        }
    }

    pub fn to_legacy_sandbox_policy(&self, cwd: &Path) -> io::Result<SandboxPolicy> {
        match self {
            Self::Managed {
                file_system,
                network,
            } => file_system
                .to_sandbox_policy()
                .to_legacy_sandbox_policy(*network, cwd),
            Self::Disabled => Ok(SandboxPolicy::DangerFullAccess),
            Self::External { network } => Ok(SandboxPolicy::ExternalSandbox {
                network_access: if network.is_enabled() {
                    crate::sandbox_policy::NetworkAccess::Enabled
                } else {
                    crate::sandbox_policy::NetworkAccess::Restricted
                },
            }),
        }
    }

    pub fn to_runtime_permissions(&self) -> (FileSystemSandboxPolicy, NetworkSandboxPolicy) {
        (
            self.file_system_sandbox_policy(),
            self.network_sandbox_policy(),
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TaggedPermissionProfile<PathType = AbsolutePathBuf> {
    #[serde(rename_all = "snake_case")]
    Managed {
        file_system: ManagedFileSystemPermissions<PathType>,
        network: NetworkSandboxPolicy,
    },
    Disabled,
    #[serde(rename_all = "snake_case")]
    External {
        network: NetworkSandboxPolicy,
    },
}

impl<PathType> From<TaggedPermissionProfile<PathType>> for PermissionProfile<PathType> {
    fn from(value: TaggedPermissionProfile<PathType>) -> Self {
        match value {
            TaggedPermissionProfile::Managed {
                file_system,
                network,
            } => Self::Managed {
                file_system,
                network,
            },
            TaggedPermissionProfile::Disabled => Self::Disabled,
            TaggedPermissionProfile::External { network } => Self::External { network },
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPermissionProfile<PathType = AbsolutePathBuf> {
    network: Option<NetworkPermissions>,
    file_system: Option<FileSystemPermissions<PathType>>,
}

impl<PathType> From<LegacyPermissionProfile<PathType>> for PermissionProfile<PathType> {
    fn from(value: LegacyPermissionProfile<PathType>) -> Self {
        let file_system = value.file_system.map_or_else(
            || ManagedFileSystemPermissions::Restricted {
                entries: Vec::new(),
                glob_scan_max_depth: None,
            },
            |permissions| ManagedFileSystemPermissions::Restricted {
                entries: permissions.entries,
                glob_scan_max_depth: permissions.glob_scan_max_depth,
            },
        );
        let network_sandbox_policy = if value
            .network
            .as_ref()
            .and_then(|network| network.enabled)
            .unwrap_or(false)
        {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        };
        Self::Managed {
            file_system,
            network: network_sandbox_policy,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PermissionProfileDe<PathType = AbsolutePathBuf> {
    Tagged(TaggedPermissionProfile<PathType>),
    Legacy(LegacyPermissionProfile<PathType>),
}

impl<'de, PathType> Deserialize<'de> for PermissionProfile<PathType>
where
    PathType: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match PermissionProfileDe::deserialize(deserializer)? {
            PermissionProfileDe::Tagged(tagged) => tagged.into(),
            PermissionProfileDe::Legacy(legacy) => legacy.into(),
        })
    }
}

impl From<NetworkSandboxPolicy> for NetworkPermissions {
    fn from(value: NetworkSandboxPolicy) -> Self {
        Self {
            enabled: Some(value.is_enabled()),
        }
    }
}

impl From<&FileSystemSandboxPolicy> for FileSystemPermissions {
    fn from(value: &FileSystemSandboxPolicy) -> Self {
        let entries = match value.kind {
            FileSystemSandboxKind::Restricted => value.entries.clone(),
            FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => {
                vec![FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Write,
                }]
            }
        };
        Self {
            entries,
            glob_scan_max_depth: value.glob_scan_max_depth.and_then(NonZeroUsize::new),
        }
    }
}

impl From<&FileSystemPermissions> for FileSystemSandboxPolicy {
    fn from(value: &FileSystemPermissions) -> Self {
        let mut policy = FileSystemSandboxPolicy::restricted(value.entries.clone());
        policy.glob_scan_max_depth = value.glob_scan_max_depth.map(usize::from);
        policy
    }
}
