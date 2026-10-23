use octopus_worker_env::{
    EnvContext, ProjectedSandbox, RootAccess, WorkerEnvCaps, WorkerEnvError, WorkerEnvKindSpec,
};

fn expand_protected_relative(
    root: &octopus_worker_env::AbsPath,
    relative: &str,
) -> Result<Vec<octopus_worker_env::AbsPath>, WorkerEnvError> {
    let mut segments = relative.split('/').filter(|segment| !segment.is_empty());
    let Some(head) = segments.next() else {
        return Err(WorkerEnvError::CapExceedsBoundary(format!(
            "protected relative path '{relative}' is empty"
        )));
    };
    let tail = segments.collect::<Vec<_>>();
    if head != "*" {
        let mut joined = root.as_path().join(head);
        for segment in tail {
            joined = joined.join(segment);
        }
        return Ok(vec![octopus_worker_env::AbsPath::new(&joined)?]);
    }
    let mut expanded = Vec::new();
    let entries = std::fs::read_dir(root.as_str()).map_err(|error| {
        WorkerEnvError::CapExceedsBoundary(format!(
            "protected root '{}' cannot be listed: {error}",
            root.as_str()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            WorkerEnvError::CapExceedsBoundary(format!(
                "protected root '{}' entry unreadable: {error}",
                root.as_str()
            ))
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let mut joined = root.as_path().join(&name);
        for segment in &tail {
            joined = joined.join(segment);
        }
        expanded.push(octopus_worker_env::AbsPath::new(&joined)?);
    }
    Ok(expanded)
}

pub fn project_sandbox(
    spec: &WorkerEnvKindSpec,
    caps: &WorkerEnvCaps,
    ctx: &EnvContext,
) -> Result<ProjectedSandbox, WorkerEnvError> {
    let bd = &spec.boundary_defaults;

    if let Some(named) = caps.explicit_writable() {
        let offered: std::collections::BTreeSet<&str> =
            bd.write_root_refs.iter().map(|r| r.path.as_str()).collect();
        for name in named {
            if !offered.contains(name.as_str()) {
                return Err(WorkerEnvError::CapExceedsBoundary(name.clone()));
            }
        }
    }

    let mut writable_roots = Vec::new();
    for r in &bd.write_root_refs {
        let writable_decl = r.access.is_empty() || r.access.contains(&RootAccess::Write);
        if writable_decl && caps.admits_writable(&r.path) {
            writable_roots.extend(ctx.resolve_roots(&r.path)?);
        }
    }

    let mut readable_roots = Vec::new();
    for r in &bd.read_root_refs {
        let readable_decl = r.access.is_empty() || r.access.contains(&RootAccess::Read);
        if readable_decl && caps.admits_readable(&r.path) {
            readable_roots.extend(ctx.resolve_roots(&r.path)?);
        }
    }

    let mut read_only_subpaths = Vec::new();
    for r in &bd.protected_read_only_subpaths {
        for root in ctx.resolve_roots(&r.path)? {
            match r.relative.as_deref() {
                None => read_only_subpaths.push(root),
                Some(relative) => {
                    for leaf in expand_protected_relative(&root, relative)? {
                        read_only_subpaths.push(leaf);
                    }
                }
            }
        }
    }
    let network = bd.network_ceiling.min(caps.network());

    Ok(ProjectedSandbox::from_parts(
        writable_roots,
        read_only_subpaths,
        readable_roots,
        network,
    ))
}
