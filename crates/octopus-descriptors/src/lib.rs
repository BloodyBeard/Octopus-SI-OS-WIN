use std::collections::BTreeMap;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod process_authority;

pub use process_authority::{
    ProcessArgumentPathSpec, ProcessAuthoritySpec, ProcessEnvironmentSubpathSpec,
    ProcessRootSubpathSpec, ProcessSandboxMode,
};

#[derive(Debug, Error)]
pub enum DescriptorError {
    #[error("malformed descriptor json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid descriptor resource reference: {0}")]
    InvalidResourceRef(String),
    #[error("descriptor resource reference could not be resolved: {0}")]
    ResourceRefResolve(String),
    #[error("unknown descriptor kind: {0}")]
    UnknownKind(String),
    #[error(
        "version mismatch for kind={kind}: got schema={got_schema} contract={got_contract}, want schema={want_schema} contract={want_contract}"
    )]
    VersionMismatch {
        kind: &'static str,
        got_schema: u32,
        got_contract: u32,
        want_schema: u32,
        want_contract: u32,
    },
    #[error("descriptor not found by hash: {0}")]
    NotFound(DescriptorHash),
    #[error("head name not bound: {0}")]
    HeadUnbound(String),
    #[error("invalid descriptor contract: {0}")]
    Invalid(String),
    #[error("io error: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, DescriptorError>;

#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct DescriptorHash([u8; 32]);

impl DescriptorHash {
    pub fn of_canonical_json(value: &serde_json::Value) -> Self {
        Self(*blake3::hash(&canonical_bytes(value)).as_bytes())
    }
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
    pub fn to_hex(&self) -> String {
        hex_lower(&self.0)
    }
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 {
            return None;
        }
        let bytes = hex.as_bytes();
        let mut out = [0u8; 32];
        for (index, slot) in out.iter_mut().enumerate() {
            let high = hex_nibble(bytes[index * 2])?;
            let low = hex_nibble(bytes[index * 2 + 1])?;
            *slot = (high << 4) | low;
        }
        Some(Self(out))
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

impl std::fmt::Debug for DescriptorHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DescriptorHash({})", self.to_hex())
    }
}
impl std::fmt::Display for DescriptorHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

pub fn canonical_bytes(value: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical_json(value, &mut out);
    out
}

fn write_canonical_json(value: &serde_json::Value, out: &mut Vec<u8>) {
    match value {
        serde_json::Value::Null => out.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => out.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => out.extend_from_slice(b"false"),
        serde_json::Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        serde_json::Value::String(text) => write_canonical_json_string(text, out),
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical_json(item, out);
            }
            out.push(b']');
        }
        serde_json::Value::Object(members) => {
            let mut keys = members.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical_json_string(key, out);
                out.push(b':');
                write_canonical_json(&members[key], out);
            }
            out.push(b'}');
        }
    }
}

fn write_canonical_json_string(text: &str, out: &mut Vec<u8>) {
    let encoded = serde_json::to_vec(&serde_json::Value::String(text.to_string()))
        .expect("a JSON string always serializes");
    out.extend_from_slice(&encoded);
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
            pub fn into_string(self) -> String { self.0 }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
        }
        impl From<String> for $name { fn from(s: String) -> Self { Self(s) } }
        impl From<&str> for $name { fn from(s: &str) -> Self { Self(s.to_string()) } }
    };
}

string_id!(DescriptorName);
string_id!(InterfaceId);
string_id!(ToolDescriptorId);
string_id!(ToolId);
string_id!(MarkId);
string_id!(VerifierId);
string_id!(EeTemplateId);
string_id!(EeId);
string_id!(RunId);
string_id!(CheckpointId);
string_id!(ArtifactId);
string_id!(WorkerEnvironmentKindId);
string_id!(ExecutorKind);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootRef {
    pub path: String,
    #[serde(default)]
    pub access: Vec<RootAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAccess {
    Read,
    Write,
}

pub trait Descriptor: DeserializeOwned + Serialize {
    const KIND: &'static str;
    const SCHEMA_VERSION: u32;
    const CONTRACT_VERSION: u32;

    fn declared_name(&self) -> Option<&str> {
        None
    }

    fn validate(&self) -> std::result::Result<(), String> {
        Ok(())
    }
}

pub fn parse_strict<T: Descriptor>(bytes: &[u8]) -> Result<T> {
    let descriptor = serde_json::from_slice::<T>(bytes)?;
    descriptor.validate().map_err(DescriptorError::Invalid)?;
    Ok(descriptor)
}

pub const DESCRIPTOR_RESOURCE_REF_KEY: &str = "$octopus_ref";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DescriptorResourceRef {
    Env { path: String },
    File { path: String },
    Artifact { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptorResourceRefLocation {
    pub pointer: String,
    pub reference: DescriptorResourceRef,
}

pub trait DescriptorResourceResolver {
    fn resolve(&self, reference: &DescriptorResourceRef) -> Result<serde_json::Value>;
}

pub fn descriptor_resource_ref(value: &serde_json::Value) -> Result<Option<DescriptorResourceRef>> {
    let Some(obj) = value.as_object() else {
        return Ok(None);
    };
    let Some(inner) = obj.get(DESCRIPTOR_RESOURCE_REF_KEY) else {
        return Ok(None);
    };
    if obj.len() != 1 {
        return Err(DescriptorError::InvalidResourceRef(format!(
            "`{DESCRIPTOR_RESOURCE_REF_KEY}` envelope must be the object's only key"
        )));
    }
    Ok(Some(serde_json::from_value(inner.clone())?))
}

pub fn collect_descriptor_resource_refs(
    value: &serde_json::Value,
) -> Result<Vec<DescriptorResourceRefLocation>> {
    fn walk(
        value: &serde_json::Value,
        pointer: &str,
        out: &mut Vec<DescriptorResourceRefLocation>,
    ) -> Result<()> {
        if let Some(reference) = descriptor_resource_ref(value)? {
            out.push(DescriptorResourceRefLocation {
                pointer: pointer.to_string(),
                reference,
            });
            return Ok(());
        }
        match value {
            serde_json::Value::Array(items) => {
                for (idx, item) in items.iter().enumerate() {
                    walk(item, &format!("{pointer}/{idx}"), out)?;
                }
            }
            serde_json::Value::Object(obj) => {
                for (key, child) in obj {
                    walk(
                        child,
                        &format!("{pointer}/{}", escape_json_pointer(key)),
                        out,
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    let mut out = Vec::new();
    walk(value, "", &mut out)?;
    Ok(out)
}

pub fn materialize_descriptor_resource_refs<R: DescriptorResourceResolver>(
    value: &serde_json::Value,
    resolver: &R,
) -> Result<serde_json::Value> {
    if let Some(reference) = descriptor_resource_ref(value)? {
        return resolver.resolve(&reference);
    }
    match value {
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(materialize_descriptor_resource_refs(item, resolver)?);
            }
            Ok(serde_json::Value::Array(out))
        }
        serde_json::Value::Object(obj) => {
            let mut out = serde_json::Map::new();
            for (key, child) in obj {
                out.insert(
                    key.clone(),
                    materialize_descriptor_resource_refs(child, resolver)?,
                );
            }
            Ok(serde_json::Value::Object(out))
        }
        _ => Ok(value.clone()),
    }
}

fn escape_json_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

#[derive(Clone, Debug)]
pub struct PinnedDescriptor {
    pub hash: DescriptorHash,
    pub kind: String,
    pub schema_version: u32,
    pub contract_version: u32,
    pub bytes: Arc<Vec<u8>>,
}

impl PinnedDescriptor {
    pub fn parse_typed<T: Descriptor>(&self) -> Result<T> {
        if self.kind != T::KIND {
            return Err(DescriptorError::UnknownKind(format!(
                "pinned kind={} cannot be parsed as {}",
                self.kind,
                T::KIND
            )));
        }
        if self.schema_version != T::SCHEMA_VERSION || self.contract_version != T::CONTRACT_VERSION
        {
            return Err(DescriptorError::VersionMismatch {
                kind: T::KIND,
                got_schema: self.schema_version,
                got_contract: self.contract_version,
                want_schema: T::SCHEMA_VERSION,
                want_contract: T::CONTRACT_VERSION,
            });
        }
        parse_strict::<T>(&self.bytes)
    }
}

#[derive(Default)]
pub struct DescriptorStore {
    by_hash: BTreeMap<DescriptorHash, Arc<PinnedDescriptor>>,
    head: BTreeMap<DescriptorName, DescriptorHash>,
}

impl DescriptorStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(
        &mut self,
        kind: &str,
        schema_version: u32,
        contract_version: u32,
        value: &serde_json::Value,
    ) -> DescriptorHash {
        let bytes = canonical_bytes(value);
        let hash = DescriptorHash::of_bytes(&bytes);
        self.by_hash.entry(hash).or_insert_with(|| {
            Arc::new(PinnedDescriptor {
                hash,
                kind: kind.to_string(),
                schema_version,
                contract_version,
                bytes: Arc::new(bytes),
            })
        });
        hash
    }

    pub fn get(&self, hash: &DescriptorHash) -> Result<Arc<PinnedDescriptor>> {
        self.by_hash
            .get(hash)
            .cloned()
            .ok_or(DescriptorError::NotFound(*hash))
    }

    pub fn set_head(&mut self, name: DescriptorName, hash: DescriptorHash) -> Result<()> {
        if !self.by_hash.contains_key(&hash) {
            return Err(DescriptorError::NotFound(hash));
        }
        self.head.insert(name, hash);
        Ok(())
    }

    pub fn resolve_head(&self, name: &DescriptorName) -> Result<DescriptorHash> {
        self.head
            .get(name)
            .copied()
            .ok_or_else(|| DescriptorError::HeadUnbound(name.as_str().to_string()))
    }

    pub fn heads_with_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a DescriptorName, DescriptorHash)> + 'a {
        self.head
            .iter()
            .filter(move |(name, _)| name.as_str().starts_with(prefix))
            .map(|(name, hash)| (name, *hash))
    }

    pub fn len(&self) -> usize {
        self.by_hash.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}

pub mod loader {
    use super::*;
    use std::path::Path;

    pub fn load_kind<T: Descriptor>(root: &Path) -> Result<Vec<(DescriptorName, T)>> {
        let entries = std::fs::read_dir(root)
            .map_err(|e| DescriptorError::Io(format!("{}: {e}", root.display())))?;
        let mut dirs = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| DescriptorError::Io(format!("{}: {e}", root.display())))?;
            let file_type = entry
                .file_type()
                .map_err(|e| DescriptorError::Io(format!("{}: {e}", entry.path().display())))?;
            if file_type.is_dir() {
                dirs.push(entry.path());
            }
        }
        dirs.sort();
        let mut out = Vec::with_capacity(dirs.len());
        for dir in dirs {
            let name = dir
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| {
                    DescriptorError::Io(format!("non-utf8 dir under {}", root.display()))
                })?
                .to_string();
            let file = dir.join(format!("{}.json", T::KIND));
            let bytes = match std::fs::read(&file) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(DescriptorError::Io(format!("{}: {error}", file.display())));
                }
            };
            let spec: T = parse_strict(&bytes)?;
            if let Some(declared) = spec.declared_name() {
                if declared != name {
                    return Err(DescriptorError::Io(format!(
                        "{}: dir/body identity mismatch — directory is {:?} but descriptor declares {:?}",
                        file.display(),
                        name,
                        declared
                    )));
                }
            }
            out.push((DescriptorName::new(format!("{}/{}", T::KIND, name)), spec));
        }
        Ok(out)
    }

    pub fn load_into_store<T: Descriptor>(
        root: &Path,
        store: &mut DescriptorStore,
    ) -> Result<Vec<(DescriptorName, DescriptorHash)>> {
        let mut out = Vec::new();
        for (name, spec) in load_kind::<T>(root)? {
            let value = serde_json::to_value(&spec)?;
            let hash = store.put(T::KIND, T::SCHEMA_VERSION, T::CONTRACT_VERSION, &value);
            store.set_head(name.clone(), hash)?;
            out.push((name, hash));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod loader_tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    #[serde(deny_unknown_fields)]
    struct TSpec {
        id: String,
    }
    impl Descriptor for TSpec {
        const KIND: &'static str = "tspec";
        const SCHEMA_VERSION: u32 = 1;
        const CONTRACT_VERSION: u32 = 1;
        fn declared_name(&self) -> Option<&str> {
            Some(&self.id)
        }
    }

    #[test]
    fn loader_reads_dir_identity_and_fails_closed() {
        use std::fs;
        let base = std::env::temp_dir().join(format!("octo_loader_{}", std::process::id()));
        let root = base.join("tspec");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(root.join("foo").join("tspec.json"), br#"{"id":"foo"}"#).unwrap();

        let mut store = DescriptorStore::new();
        let loaded = loader::load_into_store::<TSpec>(&root, &mut store).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, DescriptorName::new("tspec/foo"));
        assert!(store
            .resolve_head(&DescriptorName::new("tspec/foo"))
            .is_ok());

        fs::write(
            root.join("foo").join("tspec.json"),
            br#"{"id":"foo","bad":1}"#,
        )
        .unwrap();
        assert!(
            loader::load_kind::<TSpec>(&root).is_err(),
            "deny_unknown_fields must fail closed"
        );

        fs::write(root.join("foo").join("tspec.json"), br#"{"id":"not_foo"}"#).unwrap();
        assert!(
            loader::load_kind::<TSpec>(&root).is_err(),
            "dir/body identity mismatch must fail closed"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_dir_without_its_descriptor_json_is_an_absent_entry_not_a_kind_wide_poison() {
        use std::fs;
        let base = std::env::temp_dir().join(format!("octo_loader_absent_{}", std::process::id()));
        let root = base.join("tspec");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(root.join("foo")).unwrap();
        fs::write(root.join("foo").join("tspec.json"), br#"{"id":"foo"}"#).unwrap();
        fs::create_dir_all(root.join("mid_authored")).unwrap();
        fs::write(
            root.join("mid_authored").join("README.md"),
            b"json not written yet",
        )
        .unwrap();

        let loaded = loader::load_kind::<TSpec>(&root).expect(
            "a dir without its <kind>.json is an ABSENT entry (the entry IS the descriptor); \
             absence is a per-reference fact, never a kind-wide load failure",
        );
        assert_eq!(loaded.len(), 1, "only the dir carrying a descriptor loads");
        assert_eq!(loaded[0].0, DescriptorName::new("tspec/foo"));

        fs::write(
            root.join("mid_authored").join("tspec.json"),
            br#"{"id":"mid_authored"}"#,
        )
        .unwrap();
        let restored = loader::load_kind::<TSpec>(&root)
            .expect("the same dir loads once its descriptor lands");
        assert_eq!(
            restored.len(),
            2,
            "the finished folder is picked up by the next scan"
        );

        fs::write(
            root.join("mid_authored").join("tspec.json"),
            br#"{"id":"mid_authored","#,
        )
        .unwrap();
        assert!(
            loader::load_kind::<TSpec>(&root).is_err(),
            "a PRESENT malformed descriptor body stays a hard fail-closed error — \
             absent and malformed are different states"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn pinned_version_gate_fails_closed_on_mismatch() {
        let mut store = DescriptorStore::new();

        let bad = serde_json::json!({ "id": "bad_version" });
        let hash = store.put(TSpec::KIND, 99, 1, &bad);
        let err = store
            .get(&hash)
            .unwrap()
            .parse_typed::<TSpec>()
            .unwrap_err();
        assert!(
            matches!(
                err,
                DescriptorError::VersionMismatch {
                    got_schema: 99,
                    want_schema: 1,
                    ..
                }
            ),
            "a pinned schema_version the build cannot honor must fail closed, got {err:?}"
        );

        let ok = serde_json::json!({ "id": "ok_version" });
        let hash_ok = store.put(
            TSpec::KIND,
            TSpec::SCHEMA_VERSION,
            TSpec::CONTRACT_VERSION,
            &ok,
        );
        assert_eq!(
            store
                .get(&hash_ok)
                .unwrap()
                .parse_typed::<TSpec>()
                .unwrap()
                .id,
            "ok_version"
        );

        let other = serde_json::json!({ "id": "other_kind_body" });
        let hash_kind = store.put("other_kind", 1, 1, &other);
        assert!(store
            .get(&hash_kind)
            .unwrap()
            .parse_typed::<TSpec>()
            .is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_hex_is_the_exact_inverse_of_to_hex_and_fails_closed_on_junk() {
        let hash = DescriptorHash::of_bytes(b"content-addressed body");
        assert_eq!(
            DescriptorHash::from_hex(&hash.to_hex()),
            Some(hash),
            "from_hex recovers exactly the hash to_hex produced"
        );
        assert_eq!(
            DescriptorHash::from_hex("deadbeef"),
            None,
            "too short is None"
        );
        assert_eq!(
            DescriptorHash::from_hex(&"g".repeat(64)),
            None,
            "non-hex characters are None, never a fabricated hash"
        );
    }

    #[test]
    fn content_address_is_stable_and_store_is_append_only_failclosed() {
        let v1 = serde_json::json!({ "b": 1, "a": 2, "nested": { "y": 0, "x": 1 } });
        let v2 = serde_json::json!({ "a": 2, "nested": { "x": 1, "y": 0 }, "b": 1 });
        assert_eq!(
            DescriptorHash::of_canonical_json(&v1),
            DescriptorHash::of_canonical_json(&v2),
            "canonical hashing must be key-order independent"
        );

        let mut store = DescriptorStore::new();
        let h1 = store.put("tool", 1, 1, &v1);
        let h2 = store.put("tool", 1, 1, &v2);
        assert_eq!(h1, h2, "idempotent by content");
        assert_eq!(store.len(), 1, "append-only dedup");
        assert!(store.get(&h1).is_ok());

        let name = DescriptorName::new("tool/echo");
        assert!(
            store.resolve_head(&name).is_err(),
            "head fail-closed until set"
        );
        assert!(
            store
                .set_head(name.clone(), DescriptorHash::of_bytes(b"never-stored"))
                .is_err(),
            "cannot make-current an unstored hash"
        );
        store.set_head(name.clone(), h1).unwrap();
        assert_eq!(store.resolve_head(&name).unwrap(), h1);
    }

    #[test]
    fn descriptor_resource_refs_are_explicit_and_resolved_only_by_caller() {
        struct TestResolver;
        impl DescriptorResourceResolver for TestResolver {
            fn resolve(&self, reference: &DescriptorResourceRef) -> Result<serde_json::Value> {
                match reference {
                    DescriptorResourceRef::Env { path } if path == "OCTO_SSH_KEY_PATH" => {
                        Ok(serde_json::json!("/run/octopus/ssh/key"))
                    }
                    DescriptorResourceRef::File { path } => Ok(serde_json::json!({
                        "file_path": path
                    })),
                    other => Err(DescriptorError::ResourceRefResolve(format!(
                        "test resolver has no value for {other:?}"
                    ))),
                }
            }
        }

        let value = serde_json::json!({
            "transport": {
                "ssh_key": {
                    "$octopus_ref": {
                        "kind": "env",
                        "path": "OCTO_SSH_KEY_PATH"
                    }
                },
                "known_hosts": {
                    "$octopus_ref": {
                        "kind": "file",
                        "path": "/etc/ssh/ssh_known_hosts"
                    }
                }
            },
            "literal": "env:NOT_MAGIC"
        });

        let refs = collect_descriptor_resource_refs(&value).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].pointer, "/transport/known_hosts");
        assert_eq!(
            refs[0].reference,
            DescriptorResourceRef::File {
                path: "/etc/ssh/ssh_known_hosts".into()
            }
        );
        assert_eq!(refs[1].pointer, "/transport/ssh_key");
        assert_eq!(
            refs[1].reference,
            DescriptorResourceRef::Env {
                path: "OCTO_SSH_KEY_PATH".into()
            }
        );

        let materialized = materialize_descriptor_resource_refs(&value, &TestResolver).unwrap();
        assert_eq!(
            materialized["transport"]["ssh_key"],
            serde_json::json!("/run/octopus/ssh/key")
        );
        assert_eq!(materialized["literal"], serde_json::json!("env:NOT_MAGIC"));

        let invalid = serde_json::json!({
            "$octopus_ref": { "kind": "env", "path": "X" },
            "extra": true
        });
        assert!(
            matches!(
                descriptor_resource_ref(&invalid),
                Err(DescriptorError::InvalidResourceRef(_))
            ),
            "a resource-ref envelope with extra keys must fail closed"
        );
    }
}
