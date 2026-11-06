use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use chrono::Utc;
use contextwake::checkpoint::{CheckpointInput, CheckpointService};
use contextwake::continuity::switch_profile;
use contextwake::git::GitClient;
use contextwake::handoff::HandoffService;
use contextwake::model::{
    AuthState, ConversationExcerpt, Profile, TrustState, ValidationResult, ValidationStatus,
    Workspace,
};
use contextwake::paths::AppPaths;
use contextwake::store::Store;
use regex::Regex;
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn published_awhf_schema_is_valid_json_and_current() {
    let schema: Value = serde_json::from_str(include_str!("../schemas/awhf-1.2.0.schema.json"))
        .expect("AWHF schema JSON");
    assert_eq!(schema["properties"]["schema_version"]["const"], "1.2.0");
    assert!(
        schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|field| field == "source")
    );
    assert!(
        schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|field| field == "continuity_mode")
    );
}

#[test]
fn published_checkpoint_schema_is_valid_json_and_current() {
    let previous: Value =
        serde_json::from_str(include_str!("../schemas/checkpoint-v2.schema.json"))
            .expect("checkpoint v2 schema JSON");
    assert_eq!(previous["properties"]["schema_version"]["const"], 2);
    let schema = checkpoint_schema();
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        contextwake::checkpoint::CHECKPOINT_SCHEMA_VERSION
    );
    assert!(
        schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|field| field == "agent_id")
    );
    let conversation = &schema["properties"]["conversation"];
    assert_eq!(
        conversation["properties"]["user_requests"]["maxItems"],
        contextwake::model::ConversationExcerpt::MAX_USER_REQUESTS
    );
    assert_eq!(
        conversation["properties"]["user_requests"]["items"]["maxLength"],
        contextwake::model::ConversationExcerpt::MAX_REQUEST_CHARS
    );
    assert_eq!(
        conversation["properties"]["last_agent_message"]["maxLength"],
        contextwake::model::ConversationExcerpt::MAX_AGENT_MESSAGE_CHARS
    );
    // Every v2 field is still described by v3.
    for field in previous["required"].as_array().expect("v2 required") {
        assert!(
            schema["required"]
                .as_array()
                .expect("required")
                .contains(field),
            "{field}"
        );
    }
}

fn awhf_schema() -> Value {
    serde_json::from_str(include_str!("../schemas/awhf-1.2.0.schema.json")).expect("AWHF schema")
}

fn checkpoint_schema() -> Value {
    serde_json::from_str(include_str!("../schemas/checkpoint-v3.schema.json"))
        .expect("checkpoint schema")
}

/// Checks `instance` against the JSON Schema keywords the published schemas
/// use (type, const, enum, not, required, properties, additionalProperties,
/// items, min/maxItems, uniqueItems, min/maxLength, pattern, minimum and the
/// uuid/date-time formats) and returns every violation.
fn schema_violations(schema: &Value, instance: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    check(schema, instance, "$", &mut violations);
    violations
}

fn check(schema: &Value, instance: &Value, at: &str, violations: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    if let Some(expected) = schema.get("type") {
        let types = match expected {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !types.iter().any(|name| type_matches(name, instance)) {
            violations.push(format!("{at}: expected type {types:?}, found {instance}"));
            return;
        }
    }
    if let Some(expected) = schema.get("const")
        && instance != expected
    {
        violations.push(format!("{at}: expected constant {expected}"));
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(instance)
    {
        violations.push(format!("{at}: {instance} is not one of {options:?}"));
    }
    if let Some(forbidden) = schema.get("not") {
        let mut nested = Vec::new();
        check(forbidden, instance, at, &mut nested);
        if nested.is_empty() {
            violations.push(format!("{at}: {instance} matches a forbidden schema"));
        }
    }
    match instance {
        Value::String(text) => check_string(schema, text, at, violations),
        Value::Number(number) => {
            if let (Some(minimum), Some(value)) = (
                schema.get("minimum").and_then(Value::as_f64),
                number.as_f64(),
            ) && value < minimum
            {
                violations.push(format!("{at}: {value} is below {minimum}"));
            }
        }
        Value::Array(items) => check_array(schema, items, at, violations),
        Value::Object(map) => check_object(schema, map, at, violations),
        Value::Null | Value::Bool(_) => {}
    }
}

fn type_matches(name: &str, instance: &Value) -> bool {
    match name {
        "string" => instance.is_string(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "integer" => instance.is_i64() || instance.is_u64(),
        "number" => instance.is_number(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        _ => false,
    }
}

fn limit(schema: &serde_json::Map<String, Value>, keyword: &str) -> Option<usize> {
    schema
        .get(keyword)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn check_string(
    schema: &serde_json::Map<String, Value>,
    text: &str,
    at: &str,
    violations: &mut Vec<String>,
) {
    let length = text.chars().count();
    if limit(schema, "minLength").is_some_and(|minimum| length < minimum) {
        violations.push(format!("{at}: shorter than minLength"));
    }
    if let Some(maximum) = limit(schema, "maxLength")
        && length > maximum
    {
        violations.push(format!(
            "{at}: {length} characters exceed maxLength {maximum}"
        ));
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str)
        && !Regex::new(pattern).expect("schema pattern").is_match(text)
    {
        violations.push(format!("{at}: {text:?} does not match {pattern}"));
    }
    match schema.get("format").and_then(Value::as_str) {
        Some("uuid") if Uuid::parse_str(text).is_err() => {
            violations.push(format!("{at}: {text:?} is not a UUID"));
        }
        Some("date-time") if chrono::DateTime::parse_from_rfc3339(text).is_err() => {
            violations.push(format!("{at}: {text:?} is not an RFC 3339 date-time"));
        }
        _ => {}
    }
}

fn check_array(
    schema: &serde_json::Map<String, Value>,
    items: &[Value],
    at: &str,
    violations: &mut Vec<String>,
) {
    if limit(schema, "minItems").is_some_and(|minimum| items.len() < minimum) {
        violations.push(format!("{at}: fewer than minItems"));
    }
    if limit(schema, "maxItems").is_some_and(|maximum| items.len() > maximum) {
        violations.push(format!("{at}: more than maxItems"));
    }
    if schema.get("uniqueItems") == Some(&Value::Bool(true))
        && items
            .iter()
            .enumerate()
            .any(|(index, item)| items[index + 1..].contains(item))
    {
        violations.push(format!("{at}: items are not unique"));
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            check(item_schema, item, &format!("{at}[{index}]"), violations);
        }
    }
}

fn check_object(
    schema: &serde_json::Map<String, Value>,
    map: &serde_json::Map<String, Value>,
    at: &str,
    violations: &mut Vec<String>,
) {
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !map.contains_key(required) {
            violations.push(format!("{at}: missing required property {required}"));
        }
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    for (key, value) in map {
        let location = format!("{at}.{key}");
        match properties.and_then(|properties| properties.get(key)) {
            Some(property_schema) => check(property_schema, value, &location, violations),
            None => match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    violations.push(format!("{location}: additional property is not allowed"));
                }
                Some(additional) => check(additional, value, &location, violations),
                None => {}
            },
        }
    }
}

fn profile(paths: &AppPaths, name: &str, agent_id: &str) -> Profile {
    let now = Utc::now();
    let id = Uuid::new_v4();
    Profile {
        id,
        name: name.into(),
        display_name: name.into(),
        agent_id: agent_id.into(),
        model_provider_id: None,
        model_preference: None,
        description: None,
        agent_home: paths.agent_home(agent_id, &id),
        home_mode: contextwake::model::ProfileHomeMode::Isolated,
        account_fingerprint: None,
        auth_state: AuthState::SignedOut,
        created_at: now,
        updated_at: now,
        last_used_at: None,
    }
}

fn workspace(store: &Store, path: &Path) -> Workspace {
    fs::create_dir_all(path).expect("workspace directory");
    let now = Utc::now();
    let workspace = Workspace {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        display_name: "schema-fixture".into(),
        trust_state: TrustState::Untrusted,
        preferred_agent_id: None,
        preferred_model_provider_id: None,
        preferred_model: None,
        preferred_profile_id: None,
        last_session_id: None,
        git_root: None,
        created_at: now,
        last_opened_at: now,
    };
    store.upsert_workspace(&workspace).expect("workspace");
    workspace
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("Git must be installed for schema contract tests");
    assert!(status.success(), "git {args:?} failed");
}

fn manifest_json(record_path: &Path) -> Value {
    serde_json::from_slice(&fs::read(record_path.join("manifest.json")).expect("manifest"))
        .expect("manifest JSON")
}

fn assert_conforms(schema: &Value, instance: &Value, what: &str) {
    let violations = schema_violations(schema, instance);
    assert!(
        violations.is_empty(),
        "{what} violates its published schema:\n{}",
        violations.join("\n")
    );
}

#[test]
fn schema_checker_detects_known_violations() {
    let schema = awhf_schema();
    let valid = json!({
        "schema_version": "1.2.0",
        "handoff_id": Uuid::new_v4(),
        "created_at": "2026-11-06T10:00:00Z",
        "source_tool": "contextwake/test",
        "source": {"agent_id": "codex", "model_provider_id": null, "model": null, "profile_id": null, "session_id": null},
        "continuity_mode": "portable_handoff",
        "checkpoint_id": Uuid::new_v4(),
        "workspace": {"display_name": "repo", "path_fingerprint": "a".repeat(64), "branch": null, "commit": null},
        "objective": "Continue",
        "content_files": [{"path": "context.md", "sha256": "b".repeat(64), "media_type": "text/markdown"}],
        "redactions": {},
        "security_flags": ["commands_are_untrusted_notes_only"]
    });
    assert_conforms(&schema, &valid, "reference manifest");

    let mut without_source = valid.clone();
    without_source
        .as_object_mut()
        .expect("object")
        .remove("source");
    let mut bad_agent = valid.clone();
    bad_agent["source"]["agent_id"] = json!("--evil agent");
    let mut traversal = valid.clone();
    traversal["content_files"][0]["path"] = json!("../context.md");
    let mut extra = valid.clone();
    extra["unexpected"] = json!(true);
    let mut long_objective = valid.clone();
    long_objective["objective"] = json!("x".repeat(65_537));
    let mut bad_media = valid.clone();
    bad_media["content_files"][0]["media_type"] = json!("text/html");
    let mut bad_uuid = valid;
    bad_uuid["handoff_id"] = json!("not-a-uuid");
    for (name, instance) in [
        ("missing source", without_source),
        ("bad agent id", bad_agent),
        ("parent traversal", traversal),
        ("additional property", extra),
        ("long objective", long_objective),
        ("media type", bad_media),
        ("bad uuid", bad_uuid),
    ] {
        assert!(
            !schema_violations(&schema, &instance).is_empty(),
            "{name} must be reported"
        );
    }
}

#[test]
fn created_manifests_conform_to_the_published_awhf_schema() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::from_root(root.path().join("state"));
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let checkpoints = CheckpointService::new(
        store.clone(),
        paths.clone(),
        GitClient::new(Duration::from_secs(5)),
    );
    let handoffs = HandoffService::new(store.clone(), paths.clone());
    let schema = awhf_schema();

    // A checkpoint captured before any profile existed.
    let ordinary = workspace(&store, &root.path().join("ordinary"));
    let profile_less = checkpoints
        .create(
            &ordinary,
            None,
            None,
            CheckpointInput {
                objective: "Ship CSV export".into(),
                ..CheckpointInput::default()
            },
        )
        .expect("profile-less checkpoint");
    let record = handoffs.create(&profile_less, &ordinary).expect("handoff");
    assert_conforms(
        &schema,
        &manifest_json(&record.storage_path),
        "profile-less manifest",
    );

    // A switch handoff (with destination) from a dirty Git repository.
    let personal = profile(&paths, "personal", "codex");
    let work = profile(&paths, "work", "claude");
    store.insert_profile(&personal).expect("personal");
    store.insert_profile(&work).expect("work");
    store.set_active_profile(personal.id).expect("activate");
    let repository = root.path().join("repository");
    fs::create_dir_all(&repository).expect("repository");
    git(&repository, &["init", "-b", "feature/csv-export"]);
    git(
        &repository,
        &["config", "user.email", "schema@example.invalid"],
    );
    git(&repository, &["config", "user.name", "ContextWake Schema"]);
    fs::write(repository.join("lib.rs"), "pub fn export() {}\n").expect("seed");
    git(&repository, &["add", "lib.rs"]);
    git(&repository, &["commit", "-m", "Add export"]);
    fs::write(repository.join("csv.rs"), "// TODO\n").expect("untracked");
    let mut repository_workspace = workspace(&store, &repository);
    repository_workspace.git_root = Some(repository.clone());
    store
        .upsert_workspace(&repository_workspace)
        .expect("repository workspace");
    checkpoints
        .create(
            &repository_workspace,
            Some(&personal),
            None,
            CheckpointInput {
                objective: "Add CSV export".into(),
                validation_results: vec![ValidationResult {
                    command: vec!["cargo".into(), "test".into()],
                    status: ValidationStatus::Passed,
                    observed_at: Utc::now(),
                    summary: "exit status 0".into(),
                }],
                ..CheckpointInput::default()
            },
        )
        .expect("repository checkpoint");
    let outcome = switch_profile(
        &store,
        &checkpoints,
        &handoffs,
        "work",
        Some(&repository_workspace),
        true,
        None,
    )
    .expect("switch");
    let handoff = outcome.handoff.expect("switch handoff");
    let manifest = manifest_json(&handoff.storage_path);
    assert_eq!(manifest["workspace"]["branch"], "feature/csv-export");
    assert_eq!(manifest["destination"]["agent_id"], "claude");
    assert_conforms(&schema, &manifest, "switch manifest");
}

#[test]
fn created_checkpoints_conform_to_the_published_checkpoint_schema() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::from_root(root.path().join("state"));
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let checkpoints =
        CheckpointService::new(store.clone(), paths, GitClient::new(Duration::from_secs(5)));
    let ordinary = workspace(&store, &root.path().join("ordinary"));
    let checkpoint = checkpoints
        .create(
            &ordinary,
            None,
            None,
            CheckpointInput {
                // Longer than the schema's 65,536-character maxLength.
                objective: "Untangle the export pipeline. ".repeat(3_000),
                active_task: Some("x".repeat(70_000)),
                validation_results: vec![ValidationResult {
                    command: vec!["tool".into(), "a".repeat(10_000)],
                    status: ValidationStatus::Failed,
                    observed_at: Utc::now(),
                    summary: "exit status 1".into(),
                }],
                ..CheckpointInput::default()
            },
        )
        .expect("checkpoint");
    let path = store
        .checkpoint_path(&checkpoint.id.to_string())
        .expect("path");
    let persisted: Value =
        serde_json::from_slice(&fs::read(path).expect("checkpoint file")).expect("JSON");
    assert_conforms(&checkpoint_schema(), &persisted, "checkpoint");

    // With an oversized excerpt from a provider transcript.
    let with_excerpt = checkpoints
        .create_with_conversation(
            &ordinary,
            None,
            None,
            CheckpointInput {
                objective: "Continue the parser".into(),
                ..CheckpointInput::default()
            },
            Some(ConversationExcerpt {
                agent_id: "claude".into(),
                provider_session_id: "s".repeat(400),
                session_title: Some("t".repeat(400)),
                captured_at: Utc::now(),
                last_activity_at: Some(Utc::now()),
                user_requests: vec!["r".repeat(2_000); 9],
                last_agent_message: Some("a".repeat(9_000)),
                truncated: false,
            }),
        )
        .expect("checkpoint with excerpt");
    assert!(with_excerpt.conversation.is_some());
    let persisted: Value = serde_json::from_slice(
        &fs::read(
            store
                .checkpoint_path(&with_excerpt.id.to_string())
                .expect("path"),
        )
        .expect("checkpoint file"),
    )
    .expect("JSON");
    assert!(persisted.get("conversation").is_some());
    assert_conforms(&checkpoint_schema(), &persisted, "checkpoint with excerpt");
}
