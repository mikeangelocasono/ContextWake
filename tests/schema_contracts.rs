use serde_json::Value;

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
    let schema: Value = serde_json::from_str(include_str!("../schemas/checkpoint-v2.schema.json"))
        .expect("checkpoint schema JSON");
    assert_eq!(schema["properties"]["schema_version"]["const"], 2);
    assert!(
        schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|field| field == "agent_id")
    );
}
