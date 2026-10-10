use std::collections::BTreeSet;

use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_handlers::{ToolCall, ToolHandlerError, ToolHandlerResult};
use octopus_worker_env::{ArtifactSurface, BoundArtifact};
use serde::{Deserialize, Serialize};

const RECIPE_LABEL: &str = "skill.recipe";
const CAPTURE_LABEL: &str = "skill.capture";
const SOURCE_CONTEXT_KEY: &str = "_skill_source_context";
const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_PAGE_SIZE: usize = 20;
const DEFAULT_BODY_BYTES: usize = 65_536;
const MAX_BODY_BYTES: usize = 524_288;
const MAX_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 2_048;
const MAX_BODY_MARKDOWN_BYTES: usize = 524_288;
const MAX_CONTEXT_BYTES: usize = 65_536;
const MAX_LIST_ITEMS: usize = 64;
const MAX_ITEM_BYTES: usize = 512;
const MAX_SKILL_CATALOG_ITEMS: usize = 2_048;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillSourceContext {
    run: String,
    trace: String,
    worker: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillRecipe {
    schema_version: u32,
    name: String,
    description: String,
    body_markdown: String,
    tags: Vec<String>,
    target_worker_types: Vec<String>,
    interfaces: Vec<String>,
    origin: String,
    source_run_id: String,
    source_trace_id: String,
    source_worker: String,
    references: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
    config_schema: serde_json::Map<String, serde_json::Value>,
    env_reqs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeatability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillCapture {
    schema_version: u32,
    source_run_id: String,
    source_trace_id: String,
    source_worker: String,
    task_context: String,
    decision_rationale: String,
    proposed_skill: SkillCandidate,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillCandidate {
    name: String,
    description: String,
    body_markdown: String,
    tags: Vec<String>,
    target_worker_types: Vec<String>,
    interfaces: Vec<String>,
    references: Vec<String>,
    config_schema: serde_json::Map<String, serde_json::Value>,
    env_reqs: Vec<String>,
}

struct VisibleRecipe {
    artifact: ArtifactRef,
    recipe: SkillRecipe,
}

pub(super) async fn execute(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
    operation: &str,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    match operation {
        "skill_list" => list(surface, call).await,
        "skill_get" => get(surface, call).await,
        "skill_search" => search(surface, call).await,
        "skill_capture" => capture(surface, call).await,
        "skill_upsert" => upsert(surface, call).await,
        other => Err(execution(
            call,
            format!("skill-bound operation '{other}' was not dispatched"),
        )),
    }
}

async fn list(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let recipes = visible_recipes(surface, call).await?;
    let fingerprint = catalog_fingerprint(&recipes);
    let start = parse_cursor(
        call,
        fingerprint.as_str(),
        optional_string(call, "cursor")?,
        recipes.len(),
    )?;
    let limit = optional_usize(call, "limit", 1, MAX_PAGE_SIZE)?.unwrap_or(DEFAULT_PAGE_SIZE);
    let end = start.saturating_add(limit).min(recipes.len());
    let skills = recipes[start..end]
        .iter()
        .map(|visible| {
            serde_json::json!({
                "id": visible.artifact.id.as_str(),
                "content": visible.artifact.content.to_hex(),
                "name": visible.recipe.name,
                "description": visible.recipe.description,
                "target_worker_types": visible.recipe.target_worker_types,
                "interfaces": visible.recipe.interfaces,
                "origin": visible.recipe.origin,
                "source_run_id": visible.recipe.source_run_id,
            })
        })
        .collect::<Vec<_>>();
    let next_cursor = (end < recipes.len()).then(|| format!("{fingerprint}:{end}"));
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "skills_listed",
        "skills": skills,
        "next_cursor": next_cursor,
    })))
}

async fn get(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let id = required_trimmed_string(call, "id", MAX_ITEM_BYTES)?;
    let recipes = visible_recipes(surface, call).await?;
    let visible = exact_recipe(call, &recipes, id.as_str())?;
    let offset = optional_usize(call, "offset_bytes", 0, MAX_BODY_MARKDOWN_BYTES)?.unwrap_or(0);
    let max_bytes =
        optional_usize(call, "max_bytes", 4, MAX_BODY_BYTES)?.unwrap_or(DEFAULT_BODY_BYTES);
    let (body_markdown, next_offset_bytes) = page_utf8(
        call,
        visible.recipe.body_markdown.as_str(),
        offset,
        max_bytes,
    )?;
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "skill_read",
        "skill": {
            "id": visible.artifact.id.as_str(),
            "content": visible.artifact.content.to_hex(),
            "schema_version": visible.recipe.schema_version,
            "name": visible.recipe.name,
            "description": visible.recipe.description,
            "body_markdown": body_markdown,
            "body_size_bytes": visible.recipe.body_markdown.len(),
            "next_offset_bytes": next_offset_bytes,
            "tags": visible.recipe.tags,
            "target_worker_types": visible.recipe.target_worker_types,
            "interfaces": visible.recipe.interfaces,
            "origin": visible.recipe.origin,
            "source_run_id": visible.recipe.source_run_id,
            "source_trace_id": visible.recipe.source_trace_id,
            "source_worker": visible.recipe.source_worker,
            "references": visible.recipe.references,
            "supersedes": visible.recipe.supersedes,
            "config_schema": visible.recipe.config_schema,
            "env_reqs": visible.recipe.env_reqs,
            "repeatability": visible.recipe.repeatability,
            "confidence": visible.recipe.confidence,
        }
    })))
}

async fn search(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let query = required_trimmed_string(call, "query", MAX_CONTEXT_BYTES)?;
    let targets = optional_string_array(call, "target_worker_types", MAX_LIST_ITEMS)?;
    let interfaces = optional_string_array(call, "interfaces", MAX_LIST_ITEMS)?;
    let limit = optional_usize(call, "max_results", 1, MAX_PAGE_SIZE)?.unwrap_or(MAX_PAGE_SIZE);
    let query_terms = tokenize(query.as_str());
    if query_terms.is_empty() {
        return Err(invalid(call, "query must contain searchable characters"));
    }
    let recipes = visible_recipes(surface, call).await?;
    let mut matches = recipes
        .iter()
        .filter(|visible| all_present(&visible.recipe.target_worker_types, &targets))
        .filter(|visible| all_present(&visible.recipe.interfaces, &interfaces))
        .filter_map(|visible| {
            let score = recipe_score(&visible.recipe, &query_terms);
            (score > 0).then(|| {
                serde_json::json!({
                    "id": visible.artifact.id.as_str(),
                    "content": visible.artifact.content.to_hex(),
                    "name": visible.recipe.name,
                    "description": visible.recipe.description,
                    "score": score,
                    "preview": preview(visible.recipe.body_markdown.as_str(), 320),
                    "target_worker_types": visible.recipe.target_worker_types,
                    "interfaces": visible.recipe.interfaces,
                })
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right["score"]
            .as_u64()
            .cmp(&left["score"].as_u64())
            .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
    });
    matches.truncate(limit);
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "skills_searched",
        "query": query,
        "matches": matches,
    })))
}

async fn capture(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let context = source_context(call)?;
    let references = canonical_bound_refs(
        surface,
        call,
        optional_string_array(call, "references", MAX_LIST_ITEMS)?,
        None,
    )
    .await?;
    let capture = SkillCapture {
        schema_version: 1,
        source_run_id: context.run,
        source_trace_id: context.trace,
        source_worker: context.worker,
        task_context: required_text_string(call, "task_context", MAX_CONTEXT_BYTES)?,
        decision_rationale: required_text_string(call, "decision_rationale", MAX_CONTEXT_BYTES)?,
        proposed_skill: candidate(call, references)?,
    };
    validate_capture(call, &capture)?;
    let artifact = append_json(surface, call, CAPTURE_LABEL, &capture).await?;
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "skill_capture_emitted",
        "artifact": artifact_value(&artifact),
        "source_run_id": capture.source_run_id,
        "source_trace_id": capture.source_trace_id,
    })))
}

async fn upsert(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let context = source_context(call)?;
    let references = canonical_bound_refs(
        surface,
        call,
        optional_string_array(call, "references", MAX_LIST_ITEMS)?,
        None,
    )
    .await?;
    let supersedes = match optional_string(call, "supersedes")? {
        Some(value) => canonical_bound_refs(
            surface,
            call,
            vec![bounded_trimmed(call, "supersedes", value, MAX_ITEM_BYTES)?],
            Some(RECIPE_LABEL),
        )
        .await?
        .into_iter()
        .next(),
        None => None,
    };
    let candidate = candidate(call, references)?;
    let recipe = SkillRecipe {
        schema_version: 1,
        name: candidate.name,
        description: candidate.description,
        body_markdown: candidate.body_markdown,
        tags: candidate.tags,
        target_worker_types: candidate.target_worker_types,
        interfaces: candidate.interfaces,
        origin: required_trimmed_string(call, "origin", 64)?,
        source_run_id: context.run,
        source_trace_id: context.trace,
        source_worker: context.worker,
        references: candidate.references,
        supersedes,
        config_schema: candidate.config_schema,
        env_reqs: candidate.env_reqs,
        repeatability: optional_string(call, "repeatability")?
            .map(|value| bounded_trimmed(call, "repeatability", value, 64))
            .transpose()?,
        confidence: optional_string(call, "confidence")?
            .map(|value| bounded_trimmed(call, "confidence", value, 64))
            .transpose()?,
    };
    validate_recipe(call, &recipe)?;
    let artifact = append_json(surface, call, RECIPE_LABEL, &recipe).await?;
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "skill_upserted",
        "artifact": artifact_value(&artifact),
        "source_run_id": recipe.source_run_id,
        "source_trace_id": recipe.source_trace_id,
        "supersedes": recipe.supersedes,
    })))
}

fn candidate(call: &ToolCall, references: Vec<String>) -> Result<SkillCandidate, ToolHandlerError> {
    Ok(SkillCandidate {
        name: required_trimmed_string(call, "name", MAX_NAME_BYTES)?,
        description: optional_text_string(call, "description")?
            .map(|value| bounded_text(call, "description", value, MAX_DESCRIPTION_BYTES, false))
            .transpose()?
            .unwrap_or_default(),
        body_markdown: required_text_string(call, "body_markdown", MAX_BODY_MARKDOWN_BYTES)?,
        tags: required_string_array(call, "tags", MAX_LIST_ITEMS)?,
        target_worker_types: required_string_array(call, "target_worker_types", MAX_LIST_ITEMS)?,
        interfaces: optional_string_array(call, "interfaces", MAX_LIST_ITEMS)?,
        references,
        config_schema: optional_object(call, "config_schema")?.unwrap_or_default(),
        env_reqs: optional_string_array(call, "env_reqs", MAX_LIST_ITEMS)?,
    })
}

async fn visible_recipes(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<Vec<VisibleRecipe>, ToolHandlerError> {
    let bound = surface
        .list()
        .await
        .map_err(|error| execution(call, error.to_string()))?;
    let mut recipes = Vec::new();
    for entry in bound {
        if artifact_label(&entry) != RECIPE_LABEL {
            continue;
        }
        let text = entry.text.as_deref().ok_or_else(|| {
            execution(
                call,
                format!(
                    "bound skill recipe '{}' is not a UTF-8 JSON surface",
                    entry.artifact.id.as_str()
                ),
            )
        })?;
        let recipe: SkillRecipe = serde_json::from_str(text).map_err(|error| {
            execution(
                call,
                format!(
                    "bound skill recipe '{}' is malformed: {error}",
                    entry.artifact.id.as_str()
                ),
            )
        })?;
        validate_recipe(call, &recipe)?;
        recipes.push(VisibleRecipe {
            artifact: entry.artifact,
            recipe,
        });
        if recipes.len() > MAX_SKILL_CATALOG_ITEMS {
            return Err(execution(
                call,
                format!("bound skill catalog exceeds {MAX_SKILL_CATALOG_ITEMS} readable recipes"),
            ));
        }
    }
    recipes.sort_by(|left, right| {
        left.recipe
            .name
            .cmp(&right.recipe.name)
            .then_with(|| left.artifact.id.as_str().cmp(right.artifact.id.as_str()))
    });
    Ok(recipes)
}

fn validate_recipe(call: &ToolCall, recipe: &SkillRecipe) -> Result<(), ToolHandlerError> {
    if recipe.schema_version != 1 {
        return Err(execution(call, "skill.recipe schema_version must be 1"));
    }
    validate_stored_string(call, "name", &recipe.name, MAX_NAME_BYTES, true)?;
    validate_stored_text(
        call,
        "description",
        &recipe.description,
        MAX_DESCRIPTION_BYTES,
        false,
    )?;
    validate_stored_text(
        call,
        "body_markdown",
        &recipe.body_markdown,
        MAX_BODY_MARKDOWN_BYTES,
        true,
    )?;
    validate_stored_list(call, "tags", &recipe.tags, false)?;
    validate_stored_list(
        call,
        "target_worker_types",
        &recipe.target_worker_types,
        true,
    )?;
    validate_stored_list(call, "interfaces", &recipe.interfaces, false)?;
    validate_stored_list(call, "references", &recipe.references, false)?;
    validate_stored_list(call, "env_reqs", &recipe.env_reqs, false)?;
    for (name, value, maximum) in [
        ("origin", recipe.origin.as_str(), 64),
        (
            "source_run_id",
            recipe.source_run_id.as_str(),
            MAX_ITEM_BYTES,
        ),
        (
            "source_trace_id",
            recipe.source_trace_id.as_str(),
            MAX_ITEM_BYTES,
        ),
        (
            "source_worker",
            recipe.source_worker.as_str(),
            MAX_ITEM_BYTES,
        ),
    ] {
        validate_stored_string(call, name, value, maximum, true)?;
    }
    for (name, value) in [
        ("supersedes", recipe.supersedes.as_deref()),
        ("repeatability", recipe.repeatability.as_deref()),
        ("confidence", recipe.confidence.as_deref()),
    ] {
        if let Some(value) = value {
            validate_stored_string(call, name, value, MAX_ITEM_BYTES, true)?;
        }
    }
    Ok(())
}

fn validate_capture(call: &ToolCall, capture: &SkillCapture) -> Result<(), ToolHandlerError> {
    if capture.schema_version != 1 {
        return Err(execution(call, "skill.capture schema_version must be 1"));
    }
    validate_stored_string(
        call,
        "source_run_id",
        &capture.source_run_id,
        MAX_ITEM_BYTES,
        true,
    )?;
    validate_stored_string(
        call,
        "source_trace_id",
        &capture.source_trace_id,
        MAX_ITEM_BYTES,
        true,
    )?;
    validate_stored_string(
        call,
        "source_worker",
        &capture.source_worker,
        MAX_ITEM_BYTES,
        true,
    )?;
    validate_stored_text(
        call,
        "task_context",
        &capture.task_context,
        MAX_CONTEXT_BYTES,
        true,
    )?;
    validate_stored_text(
        call,
        "decision_rationale",
        &capture.decision_rationale,
        MAX_CONTEXT_BYTES,
        true,
    )?;
    validate_stored_string(
        call,
        "name",
        &capture.proposed_skill.name,
        MAX_NAME_BYTES,
        true,
    )?;
    validate_stored_text(
        call,
        "description",
        &capture.proposed_skill.description,
        MAX_DESCRIPTION_BYTES,
        false,
    )?;
    validate_stored_text(
        call,
        "body_markdown",
        &capture.proposed_skill.body_markdown,
        MAX_BODY_MARKDOWN_BYTES,
        true,
    )?;
    validate_stored_list(call, "tags", &capture.proposed_skill.tags, false)?;
    validate_stored_list(
        call,
        "target_worker_types",
        &capture.proposed_skill.target_worker_types,
        true,
    )?;
    validate_stored_list(
        call,
        "interfaces",
        &capture.proposed_skill.interfaces,
        false,
    )?;
    validate_stored_list(
        call,
        "references",
        &capture.proposed_skill.references,
        false,
    )?;
    validate_stored_list(call, "env_reqs", &capture.proposed_skill.env_reqs, false)
}

async fn canonical_bound_refs(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
    requested: Vec<String>,
    required_label: Option<&str>,
) -> Result<Vec<String>, ToolHandlerError> {
    let bound = surface
        .list()
        .await
        .map_err(|error| execution(call, error.to_string()))?;
    let mut canonical = Vec::with_capacity(requested.len());
    let mut seen = BTreeSet::new();
    for requested in requested {
        let mut matches = bound.iter().filter(|entry| {
            entry.artifact.id.as_str() == requested || entry.artifact.content.to_hex() == requested
        });
        let first = matches.next().ok_or_else(|| {
            invalid(
                call,
                format!("referenced artifact '{requested}' is not bound to this run"),
            )
        })?;
        if matches.next().is_some() {
            return Err(invalid(
                call,
                format!("referenced artifact '{requested}' is ambiguous"),
            ));
        }
        if required_label.is_some_and(|label| artifact_label(first) != label) {
            return Err(invalid(
                call,
                format!(
                    "referenced artifact '{}' is not a {} artifact",
                    first.artifact.id.as_str(),
                    required_label.unwrap_or_default()
                ),
            ));
        }
        let canonical_id = first.artifact.id.as_str().to_string();
        if !seen.insert(canonical_id.clone()) {
            return Err(invalid(
                call,
                format!("referenced artifact '{requested}' duplicates an earlier reference"),
            ));
        }
        canonical.push(canonical_id);
    }
    Ok(canonical)
}

async fn append_json<T: Serialize>(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
    label: &str,
    value: &T,
) -> Result<ArtifactRef, ToolHandlerError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| execution(call, format!("{label} did not serialize: {error}")))?;
    surface
        .append(label, &bytes)
        .await
        .map_err(|error| execution(call, error.to_string()))
}

fn source_context(call: &ToolCall) -> Result<SkillSourceContext, ToolHandlerError> {
    let value = call.arguments.get(SOURCE_CONTEXT_KEY).ok_or_else(|| {
        execution(
            call,
            "skill write is missing interface-attested source context",
        )
    })?;
    let context: SkillSourceContext = serde_json::from_value(value.clone()).map_err(|error| {
        execution(
            call,
            format!("skill write source context is malformed: {error}"),
        )
    })?;
    for (name, value) in [
        ("run", context.run.as_str()),
        ("trace", context.trace.as_str()),
        ("worker", context.worker.as_str()),
    ] {
        validate_stored_string(call, name, value, MAX_ITEM_BYTES, true)?;
    }
    Ok(context)
}

fn exact_recipe<'a>(
    call: &ToolCall,
    recipes: &'a [VisibleRecipe],
    id: &str,
) -> Result<&'a VisibleRecipe, ToolHandlerError> {
    let mut matches = recipes.iter().filter(|visible| {
        visible.artifact.id.as_str() == id || visible.artifact.content.to_hex() == id
    });
    let first = matches.next().ok_or_else(|| {
        execution(
            call,
            format!("skill recipe '{id}' is not bound to this run"),
        )
    })?;
    if matches.next().is_some() {
        return Err(execution(
            call,
            format!("skill recipe reference '{id}' is ambiguous"),
        ));
    }
    Ok(first)
}

fn catalog_fingerprint(recipes: &[VisibleRecipe]) -> String {
    let rows = recipes
        .iter()
        .map(|visible| {
            serde_json::json!({
                "id": visible.artifact.id.as_str(),
                "content": visible.artifact.content.to_hex(),
            })
        })
        .collect::<Vec<_>>();
    ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(
        &serde_json::Value::Array(rows),
    ))
    .to_hex()
}

fn parse_cursor(
    call: &ToolCall,
    fingerprint: &str,
    cursor: Option<String>,
    item_count: usize,
) -> Result<usize, ToolHandlerError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let (cursor_fingerprint, offset) = cursor
        .rsplit_once(':')
        .ok_or_else(|| invalid(call, "cursor is invalid"))?;
    if cursor_fingerprint != fingerprint {
        return Err(invalid(
            call,
            "cursor does not match the current bound skill set",
        ));
    }
    let offset = offset
        .parse::<usize>()
        .map_err(|_| invalid(call, "cursor offset is invalid"))?;
    if offset > item_count {
        return Err(invalid(
            call,
            "cursor offset exceeds the current bound skill set",
        ));
    }
    Ok(offset)
}

fn page_utf8(
    call: &ToolCall,
    text: &str,
    offset: usize,
    max_bytes: usize,
) -> Result<(String, Option<usize>), ToolHandlerError> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err(invalid(
            call,
            format!("offset_bytes {offset} is not a UTF-8 boundary"),
        ));
    }
    let mut end = offset.saturating_add(max_bytes).min(text.len());
    while end > offset && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == offset && offset < text.len() {
        return Err(invalid(
            call,
            "max_bytes cannot retain the next UTF-8 scalar",
        ));
    }
    Ok((
        text[offset..end].to_string(),
        (end < text.len()).then_some(end),
    ))
}

fn recipe_score(recipe: &SkillRecipe, query_terms: &[String]) -> u64 {
    let name = tokenize(&recipe.name);
    let description = tokenize(&recipe.description);
    let body = tokenize(&recipe.body_markdown);
    let tags = recipe
        .tags
        .iter()
        .flat_map(|tag| tokenize(tag))
        .collect::<Vec<_>>();
    query_terms
        .iter()
        .map(|term| {
            (name.iter().filter(|candidate| *candidate == term).count() as u64 * 8)
                + (tags.iter().filter(|candidate| *candidate == term).count() as u64 * 6)
                + (description
                    .iter()
                    .filter(|candidate| *candidate == term)
                    .count() as u64
                    * 3)
                + body.iter().filter(|candidate| *candidate == term).count() as u64
        })
        .sum()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| term.to_lowercase())
        .collect()
}

fn preview(text: &str, maximum_chars: usize) -> String {
    let mut value = text.chars().take(maximum_chars).collect::<String>();
    if text.chars().count() > maximum_chars {
        value.push('…');
    }
    value
}

fn all_present(values: &[String], requested: &[String]) -> bool {
    requested.iter().all(|item| values.contains(item))
}

fn artifact_label(entry: &BoundArtifact) -> &str {
    entry
        .artifact
        .id
        .as_str()
        .split_once(':')
        .map(|(label, _)| label)
        .unwrap_or_else(|| entry.artifact.id.as_str())
}

fn artifact_value(artifact: &ArtifactRef) -> serde_json::Value {
    serde_json::json!({
        "id": artifact.id.as_str(),
        "content": artifact.content.to_hex(),
    })
}

fn required_string_array(
    call: &ToolCall,
    name: &str,
    maximum_items: usize,
) -> Result<Vec<String>, ToolHandlerError> {
    let values = optional_string_array(call, name, maximum_items)?;
    if values.is_empty() {
        return Err(invalid(
            call,
            format!("arguments field '{name}' must contain at least one item"),
        ));
    }
    Ok(values)
}

fn optional_string_array(
    call: &ToolCall,
    name: &str,
    maximum_items: usize,
) -> Result<Vec<String>, ToolHandlerError> {
    let Some(value) = call.arguments.get(name) else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| {
        invalid(
            call,
            format!("arguments field '{name}' must be an array of strings"),
        )
    })?;
    if items.len() > maximum_items {
        return Err(invalid(
            call,
            format!("arguments field '{name}' exceeds {maximum_items} items"),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let value = item.as_str().ok_or_else(|| {
            invalid(
                call,
                format!("arguments field '{name}[{index}]' must be a string"),
            )
        })?;
        let value = bounded_trimmed(
            call,
            format!("{name}[{index}]").as_str(),
            value.to_string(),
            MAX_ITEM_BYTES,
        )?;
        if !seen.insert(value.clone()) {
            return Err(invalid(
                call,
                format!("arguments field '{name}' contains duplicate item '{value}'"),
            ));
        }
        result.push(value);
    }
    Ok(result)
}

fn optional_object(
    call: &ToolCall,
    name: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, ToolHandlerError> {
    match call.arguments.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_object()
            .cloned()
            .map(Some)
            .ok_or_else(|| invalid(call, format!("arguments field '{name}' must be an object"))),
    }
}

fn optional_string(call: &ToolCall, name: &str) -> Result<Option<String>, ToolHandlerError> {
    match call.arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid(
            call,
            format!("arguments field '{name}' must be a string"),
        )),
    }
}

fn optional_text_string(call: &ToolCall, name: &str) -> Result<Option<String>, ToolHandlerError> {
    optional_string(call, name)
}

fn required_trimmed_string(
    call: &ToolCall,
    name: &str,
    maximum_bytes: usize,
) -> Result<String, ToolHandlerError> {
    let value = optional_string(call, name)?
        .ok_or_else(|| invalid(call, format!("arguments must include string '{name}'")))?;
    bounded_trimmed(call, name, value, maximum_bytes)
}

fn required_text_string(
    call: &ToolCall,
    name: &str,
    maximum_bytes: usize,
) -> Result<String, ToolHandlerError> {
    let value = optional_text_string(call, name)?
        .ok_or_else(|| invalid(call, format!("arguments must include string '{name}'")))?;
    bounded_text(call, name, value, maximum_bytes, true)
}

fn bounded_trimmed(
    call: &ToolCall,
    name: &str,
    value: String,
    maximum_bytes: usize,
) -> Result<String, ToolHandlerError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(invalid(
            call,
            format!(
                "arguments field '{name}' must be non-empty, trimmed, and at most {maximum_bytes} bytes"
            ),
        ));
    }
    Ok(value)
}

fn bounded_text(
    call: &ToolCall,
    name: &str,
    value: String,
    maximum_bytes: usize,
    required: bool,
) -> Result<String, ToolHandlerError> {
    if (required && value.trim().is_empty())
        || (!required && !value.is_empty() && value.trim().is_empty())
        || value.len() > maximum_bytes
        || value.chars().any(disallowed_text_control)
    {
        return Err(invalid(
            call,
            format!(
                "arguments field '{name}' must be bounded UTF-8 text of at most {maximum_bytes} bytes"
            ),
        ));
    }
    Ok(value)
}

fn optional_usize(
    call: &ToolCall,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<usize>, ToolHandlerError> {
    match call.arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| (minimum..=maximum).contains(value))
            .map(Some)
            .ok_or_else(|| {
                invalid(
                    call,
                    format!(
                        "arguments field '{name}' must be an integer between {minimum} and {maximum}"
                    ),
                )
            }),
        Some(_) => Err(invalid(
            call,
            format!(
                "arguments field '{name}' must be an integer between {minimum} and {maximum}"
            ),
        )),
    }
}

fn validate_stored_string(
    call: &ToolCall,
    name: &str,
    value: &str,
    maximum_bytes: usize,
    required: bool,
) -> Result<(), ToolHandlerError> {
    if (required && value.is_empty())
        || value.trim() != value
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(execution(
            call,
            format!("stored skill field '{name}' violates its bounded string contract"),
        ));
    }
    Ok(())
}

fn validate_stored_text(
    call: &ToolCall,
    name: &str,
    value: &str,
    maximum_bytes: usize,
    required: bool,
) -> Result<(), ToolHandlerError> {
    if (required && value.trim().is_empty())
        || (!required && !value.is_empty() && value.trim().is_empty())
        || value.len() > maximum_bytes
        || value.chars().any(disallowed_text_control)
    {
        return Err(execution(
            call,
            format!("stored skill field '{name}' violates its bounded text contract"),
        ));
    }
    Ok(())
}

fn disallowed_text_control(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
}

fn validate_stored_list(
    call: &ToolCall,
    name: &str,
    values: &[String],
    required: bool,
) -> Result<(), ToolHandlerError> {
    if (required && values.is_empty()) || values.len() > MAX_LIST_ITEMS {
        return Err(execution(
            call,
            format!("stored skill field '{name}' violates its item-count contract"),
        ));
    }
    for value in values {
        validate_stored_string(call, name, value, MAX_ITEM_BYTES, true)?;
    }
    Ok(())
}

fn invalid(call: &ToolCall, reason: impl Into<String>) -> ToolHandlerError {
    ToolHandlerError::InvalidArguments {
        tool_id: call.tool_id.clone(),
        reason: reason.into(),
    }
}

fn execution(call: &ToolCall, reason: impl Into<String>) -> ToolHandlerError {
    ToolHandlerError::Execution {
        tool_id: call.tool_id.clone(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use octopus_descriptors::ArtifactId;
    use octopus_ledger::{ArtifactRef, ContentHash};
    use octopus_worker_env::{ArtifactSurface, BoundArtifact, WorkerEnvError};

    use super::execute;
    use octopus_tool_handlers::ToolCall;

    struct SkillSurface {
        entries: Mutex<Vec<(ArtifactRef, Vec<u8>)>>,
    }

    impl SkillSurface {
        fn new() -> Self {
            Self {
                entries: Mutex::new(Vec::new()),
            }
        }
    }

    impl ArtifactSurface for SkillSurface {
        fn append(
            &self,
            label: &str,
            payload: &[u8],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ArtifactRef, WorkerEnvError>> + Send + '_>,
        > {
            let label = label.to_string();
            let payload = payload.to_vec();
            Box::pin(async move {
                let content = ContentHash::of_bytes(&payload);
                let artifact = ArtifactRef {
                    id: ArtifactId::new(format!("{label}:{}", content.to_hex())),
                    content,
                };
                self.entries
                    .lock()
                    .unwrap()
                    .push((artifact.clone(), payload));
                Ok(artifact)
            })
        }

        fn read(
            &self,
            id: &str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<Vec<u8>>, WorkerEnvError>>
                    + Send
                    + '_,
            >,
        > {
            let id = id.to_string();
            Box::pin(async move {
                Ok(self
                    .entries
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(artifact, _)| {
                        artifact.id.as_str() == id || artifact.content.to_hex() == id
                    })
                    .map(|(_, bytes)| bytes.clone()))
            })
        }

        fn list(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Vec<BoundArtifact>, WorkerEnvError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                Ok(self
                    .entries
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(artifact, bytes)| BoundArtifact {
                        artifact: artifact.clone(),
                        text: String::from_utf8(bytes.clone()).ok(),
                    })
                    .collect())
            })
        }
    }

    fn source_context() -> serde_json::Value {
        serde_json::json!({
            "run": "run_skill",
            "trace": "trace_skill",
            "worker": "system_worker_coder_max"
        })
    }

    async fn run(
        surface: &SkillSurface,
        operation: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, octopus_tool_handlers::ToolHandlerError> {
        let call = ToolCall {
            tool_id: operation.replace('_', "."),
            arguments,
            handler_config: serde_json::json!({"operation": operation}),
        };
        execute(surface, &call, operation)
            .await
            .map(|result| result.body)
    }

    fn recipe_arguments(name: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "description": format!("Use {name} safely"),
            "body_markdown": body,
            "tags": ["rust", name],
            "target_worker_types": ["system_worker_coder_max"],
            "interfaces": ["world_provider_codex"],
            "references": [],
            "config_schema": BTreeMap::<String, serde_json::Value>::new(),
            "env_reqs": [],
            "origin": "autonomous",
            "_skill_source_context": source_context()
        })
    }

    #[tokio::test]
    async fn skill_tools_keep_multiline_recipes_binding_visible_and_page_exactly() {
        let surface = SkillSurface::new();
        for (name, body) in [
            ("applypatch", "# Apply patch\n\nPreserve line endings."),
            ("search", "# Search\n\nRespect Git ignore boundaries."),
        ] {
            let written = run(&surface, "skill_upsert", recipe_arguments(name, body))
                .await
                .unwrap();
            assert_eq!(written["result"], "skill_upserted");
            assert_eq!(written["source_run_id"], "run_skill");
        }
        let first = run(&surface, "skill_list", serde_json::json!({"limit": 1}))
            .await
            .unwrap();
        assert_eq!(first["skills"].as_array().unwrap().len(), 1);
        let cursor = first["next_cursor"].as_str().unwrap();
        let second = run(
            &surface,
            "skill_list",
            serde_json::json!({"limit": 1, "cursor": cursor}),
        )
        .await
        .unwrap();
        assert_eq!(second["skills"].as_array().unwrap().len(), 1);
        assert!(second["next_cursor"].is_null());
        let id = first["skills"][0]["id"].as_str().unwrap();
        let read = run(
            &surface,
            "skill_get",
            serde_json::json!({"id": id, "max_bytes": 12}),
        )
        .await
        .unwrap();
        assert_eq!(read["result"], "skill_read");
        assert!(read["skill"]["next_offset_bytes"].is_number());
        let searched = run(
            &surface,
            "skill_search",
            serde_json::json!({
                "query": "line endings",
                "target_worker_types": ["system_worker_coder_max"],
                "interfaces": ["world_provider_codex"]
            }),
        )
        .await
        .unwrap();
        assert_eq!(searched["matches"].as_array().unwrap().len(), 1);
        assert_eq!(searched["matches"][0]["name"], "applypatch");
    }

    #[tokio::test]
    async fn skill_writes_reject_unattested_or_silently_repaired_input() {
        let surface = SkillSurface::new();
        let mut missing_context = recipe_arguments("strict", "Exact evidence");
        missing_context
            .as_object_mut()
            .unwrap()
            .remove("_skill_source_context");
        assert!(run(&surface, "skill_upsert", missing_context)
            .await
            .is_err());

        let mut duplicate = recipe_arguments("strict", "Exact evidence");
        duplicate["tags"] = serde_json::json!(["same", "same"]);
        assert!(run(&surface, "skill_upsert", duplicate).await.is_err());

        let mut unbound = recipe_arguments("strict", "Exact evidence");
        unbound["references"] = serde_json::json!(["artifact-missing"]);
        assert!(run(&surface, "skill_upsert", unbound).await.is_err());
    }
}
