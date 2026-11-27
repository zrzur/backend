//! The public OpenAPI document, made valid for client generators.
//!
//! OpenAPI Generator (`openapi-generator-cli validate`) refused the document
//! the routes produce, for things that change nothing on the wire:
//!
//! - an `operationId` used by more than one route (`list` for both
//!   `/community/{kind}` and `/plugins/`): every route after the first gets
//!   one made of its method and path, `post_plugins`, the name the SDK
//!   generator already gave it; so does a name a generated client uses for
//!   itself (`request`: `POST /beta/@me/request` broke the TypeScript one);
//! - names with spaces (`Bot Token`, `ISO8601 Timestamp`): a component name
//!   may hold only letters, digits, `.`, `-` and `_`, so the spaces go, in
//!   the name and everywhere it is used;
//! - `"default": null`: a missing optional field is null already, and the
//!   validator refuses it beside a `$ref`;
//! - a fixed-length array written as a list of item schemas (a JSON Schema
//!   tuple, unknown to OpenAPI 3.0): its items become one of those schemas;
//! - `{"type": "null"}` as one alternative of `anyOf` (OpenAPI 3.1 only):
//!   the other alternative, marked `nullable`;
//! - an enum written as `oneOf` single strings, one per variant so each can
//!   say what it means: one string enum, the variants' meanings kept in its
//!   description (generators made no enum of it, and TypeScript code that
//!   did not compile).

use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use serde_json::{json, Value};

/// Method names of generated clients, never an operation's
const TAKEN: [&str; 4] = ["request", "configuration", "middleware", "clone"];

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Make the merged document valid for generators (see the module docs)
pub fn tidy(spec: &mut OpenApi) {
    let Ok(mut value) = serde_json::to_value(&*spec) else {
        return;
    };
    tidy_value(&mut value);
    // the document stays as it was if it no longer reads as one
    if let Ok(tidied) = serde_json::from_value::<OpenApi>(value) {
        *spec = tidied;
    }
}

/// The same, on the document as JSON
pub fn tidy_value(doc: &mut Value) {
    unique_operation_ids(doc);
    rename_components(doc);
    tidy_schemas(doc);
}

/// A component name as generators accept it
fn valid_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect()
}

/// `post /plugins/{id}/report` → `post_plugins_id_report`
fn route_name(method: &str, path: &str) -> String {
    let mut out = method.to_owned();
    let mut gap = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            if gap {
                out.push('_');
                gap = false;
            }
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

fn unique_operation_ids(doc: &mut Value) {
    let Some(paths) = doc.get_mut("paths").and_then(Value::as_object_mut) else {
        return;
    };
    let mut seen = std::collections::HashSet::new();
    for (path, item) in paths.iter_mut() {
        for method in METHODS {
            let Some(id) = item
                .get_mut(method)
                .and_then(|op| op.get_mut("operationId"))
            else {
                continue;
            };
            let Some(name) = id.as_str().map(str::to_owned) else {
                continue;
            };
            if TAKEN.contains(&name.as_str()) || !seen.insert(name) {
                let unique = route_name(method, path);
                seen.insert(unique.clone());
                *id = Value::String(unique);
            }
        }
    }
}

fn rename_components(doc: &mut Value) {
    let mut schemas_renamed: Vec<(String, String)> = Vec::new();
    let mut schemes_renamed: Vec<(String, String)> = Vec::new();
    if let Some(components) = doc.get_mut("components") {
        for (section, renamed) in [
            ("schemas", &mut schemas_renamed),
            ("securitySchemes", &mut schemes_renamed),
        ] {
            if let Some(map) = components.get_mut(section).and_then(Value::as_object_mut) {
                let old = std::mem::take(map);
                for (name, value) in old {
                    let valid = valid_name(&name);
                    if valid != name {
                        renamed.push((name, valid.clone()));
                    }
                    map.insert(valid, value);
                }
            }
        }
    }
    if schemas_renamed.is_empty() && schemes_renamed.is_empty() {
        return;
    }
    rename_uses(doc, &schemas_renamed, &schemes_renamed, false);
}

/// `$ref`s to renamed schemas, and renamed schemes in `security` lists
fn rename_uses(
    node: &mut Value,
    schemas: &[(String, String)],
    schemes: &[(String, String)],
    in_security: bool,
) {
    match node {
        Value::Object(map) => {
            if in_security {
                // one requirement: { "<scheme>": [scopes] }
                let old = std::mem::take(map);
                for (name, scopes) in old {
                    let name = schemes
                        .iter()
                        .find(|(from, _)| *from == name)
                        .map(|(_, to)| to.clone())
                        .unwrap_or(name);
                    map.insert(name, scopes);
                }
                return;
            }
            for (key, value) in map.iter_mut() {
                if key == "$ref" {
                    let renamed = value.as_str().and_then(|target| {
                        schemas
                            .iter()
                            .find(|(from, _)| target == format!("#/components/schemas/{from}"))
                            .map(|(_, to)| format!("#/components/schemas/{to}"))
                    });
                    if let Some(renamed) = renamed {
                        *value = Value::String(renamed);
                    }
                } else if key == "security" {
                    if let Value::Array(requirements) = value {
                        for requirement in requirements {
                            rename_uses(requirement, schemas, schemes, true);
                        }
                    }
                } else {
                    rename_uses(value, schemas, schemes, false);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                rename_uses(item, schemas, schemes, in_security);
            }
        }
        _ => {}
    }
}

fn tidy_schemas(node: &mut Value) {
    match node {
        Value::Object(map) => {
            // a default of null says nothing a missing field does not
            if map.get("default") == Some(&Value::Null) {
                map.remove("default");
            }
            // a tuple: one of its item schemas, the length kept by min/maxItems
            if let Some(items) = map.get("items").and_then(Value::as_array).cloned() {
                map.insert("items".to_owned(), json!({ "oneOf": items }));
            }
            // oneOf single strings: one string enum
            if let Some(alternatives) = map.get("oneOf").and_then(Value::as_array).cloned() {
                let single = |v: &Value| {
                    v.get("type") == Some(&json!("string"))
                        && v.get("enum").and_then(Value::as_array).is_some()
                        && v.as_object().is_some_and(|o| {
                            o.keys().all(|k| matches!(k.as_str(), "type" | "enum" | "description"))
                        })
                };
                if !alternatives.is_empty() && alternatives.iter().all(single) {
                    let mut values = Vec::new();
                    let mut meanings = Vec::new();
                    for alternative in &alternatives {
                        let names = alternative["enum"].as_array().cloned().unwrap_or_default();
                        if let Some(text) = alternative.get("description").and_then(Value::as_str) {
                            let list = names
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|n| format!("`{n}`"))
                                .collect::<Vec<_>>()
                                .join(", ");
                            meanings.push(format!("- {list}: {text}"));
                        }
                        values.extend(names);
                    }
                    map.remove("oneOf");
                    map.insert("type".to_owned(), json!("string"));
                    map.insert("enum".to_owned(), Value::Array(values));
                    if !meanings.is_empty() {
                        let head = map
                            .get("description")
                            .and_then(Value::as_str)
                            .map(|d| format!("{d}\n\n"))
                            .unwrap_or_default();
                        map.insert(
                            "description".to_owned(),
                            Value::String(format!("{head}{}", meanings.join("\n"))),
                        );
                    }
                }
            }
            // anyOf [null, X]: X that may be null
            if let Some(alternatives) = map.get("anyOf").and_then(Value::as_array).cloned() {
                let is_null = |v: &Value| v.get("type") == Some(&json!("null"));
                if alternatives.len() == 2 && alternatives.iter().any(is_null) {
                    let other = alternatives
                        .iter()
                        .find(|v| !is_null(*v))
                        .cloned()
                        .unwrap_or(Value::Null);
                    map.remove("anyOf");
                    match other {
                        Value::Object(other) if !other.contains_key("$ref") => {
                            for (key, value) in other {
                                map.entry(key).or_insert(value);
                            }
                        }
                        other => {
                            map.insert("allOf".to_owned(), Value::Array(vec![other]));
                        }
                    }
                    map.insert("nullable".to_owned(), Value::Bool(true));
                }
            }
            for value in map.values_mut() {
                tidy_schemas(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                tidy_schemas(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_operation_id_gets_its_route_name() {
        let mut doc = json!({ "paths": {
            "/community/{kind}": { "get": { "operationId": "list" } },
            "/plugins/": { "get": { "operationId": "list" }, "post": { "operationId": "create" } },
            "/plugins/{id}/report": { "post": { "operationId": "create" } }
        }});
        tidy_value(&mut doc);
        assert_eq!(doc["paths"]["/community/{kind}"]["get"]["operationId"], "list");
        assert_eq!(doc["paths"]["/plugins/"]["get"]["operationId"], "get_plugins");
        assert_eq!(doc["paths"]["/plugins/"]["post"]["operationId"], "create");
        assert_eq!(
            doc["paths"]["/plugins/{id}/report"]["post"]["operationId"],
            "post_plugins_id_report"
        );
    }

    #[test]
    fn names_lose_their_spaces_everywhere_they_are_used() {
        let mut doc = json!({
            "paths": { "/x": { "get": {
                "operationId": "x",
                "security": [{ "Session Token": [], "Valid MFA Ticket": [] }, { "Bot Token": [] }],
                "responses": { "200": { "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/ISO8601 Timestamp" }
                }}}}
            }}},
            "components": {
                "schemas": { "ISO8601 Timestamp": { "type": "string" }, "User": { "type": "object" } },
                "securitySchemes": { "Bot Token": {}, "Session Token": {}, "Valid MFA Ticket": {} }
            }
        });
        tidy_value(&mut doc);
        let op = &doc["paths"]["/x"]["get"];
        assert_eq!(
            op["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ISO8601Timestamp"
        );
        assert!(op["security"][0].get("SessionToken").is_some());
        assert!(op["security"][0].get("ValidMFATicket").is_some());
        assert!(op["security"][1].get("BotToken").is_some());
        assert!(doc["components"]["schemas"].get("ISO8601Timestamp").is_some());
        assert!(doc["components"]["schemas"].get("User").is_some());
        assert!(doc["components"]["securitySchemes"].get("BotToken").is_some());
        assert!(doc["components"]["securitySchemes"].get("Bot Token").is_none());
    }

    #[test]
    fn schemas_become_openapi_3_0() {
        let mut doc = json!({ "components": { "schemas": {
            "Embed": { "type": "object", "default": null },
            "Maybe": { "allOf": [{ "$ref": "#/components/schemas/Embed" }], "nullable": true, "default": null },
            "Pair": { "type": "array", "items": [{ "type": "integer" }, { "type": "string" }], "minItems": 2, "maxItems": 2 },
            "Verify": { "anyOf": [{ "type": "null" }, { "type": "object", "required": ["ticket"] }] },
            "Ref": { "anyOf": [{ "$ref": "#/components/schemas/Embed" }, { "type": "null" }] }
        }}});
        tidy_value(&mut doc);
        let s = &doc["components"]["schemas"];
        assert!(s["Embed"].get("default").is_none());
        assert!(s["Maybe"].get("default").is_none());
        assert_eq!(s["Maybe"]["nullable"], true);
        assert_eq!(s["Pair"]["items"]["oneOf"][1]["type"], "string");
        assert_eq!(s["Pair"]["maxItems"], 2);
        assert_eq!(s["Verify"]["type"], "object");
        assert_eq!(s["Verify"]["nullable"], true);
        assert!(s["Verify"].get("anyOf").is_none());
        assert_eq!(s["Ref"]["allOf"][0]["$ref"], "#/components/schemas/Embed");
        assert_eq!(s["Ref"]["nullable"], true);
    }

    #[test]
    fn single_string_alternatives_become_one_enum() {
        let mut doc = json!({ "components": { "schemas": {
            "Presence": { "description": "Presence status", "oneOf": [
                { "description": "User is online", "type": "string", "enum": ["Online"] },
                { "type": "string", "enum": ["Idle", "Busy"] }
            ]},
            "Status": { "oneOf": [
                { "type": "string", "enum": ["Pending"] },
                { "type": "object", "required": ["Denied"] }
            ]}
        }}});
        tidy_value(&mut doc);
        let s = &doc["components"]["schemas"];
        assert_eq!(s["Presence"]["type"], "string");
        assert_eq!(s["Presence"]["enum"], json!(["Online", "Idle", "Busy"]));
        assert_eq!(
            s["Presence"]["description"],
            "Presence status\n\n- `Online`: User is online"
        );
        assert!(s["Presence"].get("oneOf").is_none());
        // a real choice between a string and an object stays one
        assert!(s["Status"].get("oneOf").is_some());
    }

    #[test]
    fn a_name_the_client_uses_for_itself_is_not_an_operation_id() {
        let mut doc = json!({ "paths": {
            "/beta/@me/request": { "post": { "operationId": "request" } }
        }});
        tidy_value(&mut doc);
        assert_eq!(
            doc["paths"]["/beta/@me/request"]["post"]["operationId"],
            "post_beta_me_request"
        );
    }
}
