//! The security section of the public OpenAPI document.
//!
//! The generator lists the security of every request guard of a route as a
//! separate alternative. That said two wrong things: that the `User` guard
//! takes a session token only (it takes a bot token too), and that an MFA
//! ticket can replace signing in. Here each operation gets what it really
//! takes: a session token or a bot token where the `User` guard sits, and a
//! required ticket on top of signing in, never instead of it.

use revolt_rocket_okapi::revolt_okapi::openapi3::{
    OpenApi, Operation, SecurityRequirement, SecurityScheme, SecuritySchemeData,
};

/// What the `User` guard asks for until [`fix_security`] replaces it
pub const USER_OR_BOT: &str = "Session or Bot Token";
pub const SESSION: &str = "Session Token";
pub const BOT: &str = "Bot Token";

/// Operations whose MFA ticket is needed only sometimes (the route takes
/// `Option<…Ticket>` and asks for it in some cases): signing in is enough,
/// and the ticket is listed as one more alternative. Every other route with
/// an optional ticket type refuses the request without one, so its ticket
/// stays required here.
pub const TICKET_SOMETIMES: &[(&str, &str)] = &[
    // only to remove the token (deprecated)
    ("patch", "/bots/{bot_id}"),
    // only for some changes, and to delete a server with members
    ("patch", "/servers/{target}"),
    ("delete", "/servers/{target}"),
    // only with MFA switched on
    ("patch", "/auth/account/change/password"),
    ("patch", "/auth/account/change/email"),
    // an existing ticket is one way to get a new one
    ("put", "/auth/mfa/ticket"),
];

fn is_ticket(name: &str) -> bool {
    name.ends_with("MFA Ticket")
}

fn requirement(names: &[&str]) -> SecurityRequirement {
    names.iter().map(|name| (name.to_string(), vec![])).collect()
}

fn api_key(header: &str, description: &str) -> SecurityScheme {
    SecurityScheme {
        data: SecuritySchemeData::ApiKey {
            name: header.to_owned(),
            location: "header".to_owned(),
        },
        description: Some(description.to_owned()),
        extensions: schemars::Map::new(),
    }
}

fn fix_operation(operation: &mut Operation, ticket_sometimes: bool) {
    let Some(security) = operation.security.take() else {
        return;
    };
    let names: Vec<String> = security.iter().flat_map(|r| r.keys().cloned()).collect();
    let user_or_bot = names.iter().any(|n| n == USER_OR_BOT);
    let session = user_or_bot || names.iter().any(|n| n == SESSION);
    let tickets: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| is_ticket(n))
        .collect();

    operation.security = Some(if !session {
        // only a ticket (account routes): as it was
        security
    } else if tickets.is_empty() {
        let mut alternatives = vec![requirement(&[SESSION])];
        if user_or_bot {
            alternatives.push(requirement(&[BOT]));
        }
        alternatives
    } else {
        // a ticket proves a person signed in again: on top of their session,
        // and never for a bot
        let mut both = vec![SESSION];
        both.extend(tickets);
        let mut alternatives = vec![requirement(&both)];
        if ticket_sometimes {
            alternatives.push(requirement(&[SESSION]));
            if user_or_bot {
                alternatives.push(requirement(&[BOT]));
            }
        }
        alternatives
    });
}

/// Rewrite the security of every operation, and the schemes they name
pub fn fix_security(spec: &mut OpenApi) {
    for (path, item) in spec.paths.iter_mut() {
        for (method, operation) in [
            ("get", &mut item.get),
            ("put", &mut item.put),
            ("post", &mut item.post),
            ("delete", &mut item.delete),
            ("options", &mut item.options),
            ("head", &mut item.head),
            ("patch", &mut item.patch),
            ("trace", &mut item.trace),
        ] {
            if let Some(operation) = operation {
                let sometimes = TICKET_SOMETIMES.contains(&(method, path.as_str()));
                fix_operation(operation, sometimes);
            }
        }
    }

    let components = spec.components.get_or_insert_with(Default::default);
    components.security_schemes.remove(USER_OR_BOT);
    components.security_schemes.insert(
        SESSION.to_owned(),
        revolt_rocket_okapi::revolt_okapi::openapi3::RefOr::Object(api_key(
            "x-session-token",
            "Used to authenticate as a user.",
        )),
    );
    components.security_schemes.insert(
        BOT.to_owned(),
        revolt_rocket_okapi::revolt_okapi::openapi3::RefOr::Object(api_key(
            "x-bot-token",
            "Used to authenticate as a bot: the token from the bot's settings.",
        )),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_rocket_okapi::revolt_okapi::openapi3::PathItem;

    fn spec_with(security: Vec<SecurityRequirement>) -> OpenApi {
        spec_at("/x", security)
    }

    fn spec_at(path: &str, security: Vec<SecurityRequirement>) -> OpenApi {
        let mut spec = OpenApi::default();
        spec.paths.insert(
            path.to_owned(),
            PathItem {
                get: Some(Operation {
                    security: Some(security),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        spec
    }

    fn security_of(spec: &OpenApi) -> Vec<Vec<String>> {
        security_at(spec, "/x")
    }

    fn security_at(spec: &OpenApi, path: &str) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = spec.paths[path]
            .get
            .as_ref()
            .unwrap()
            .security
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| {
                let mut names: Vec<String> = r.keys().cloned().collect();
                names.sort();
                names
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn a_user_guard_takes_a_session_or_a_bot_token() {
        let mut spec = spec_with(vec![requirement(&[USER_OR_BOT])]);
        fix_security(&mut spec);
        assert_eq!(
            security_of(&spec),
            vec![vec![BOT.to_string()], vec![SESSION.to_string()]]
        );
        let schemes = &spec.components.as_ref().unwrap().security_schemes;
        assert!(schemes.contains_key(BOT));
        assert!(schemes.contains_key(SESSION));
        assert!(!schemes.contains_key(USER_OR_BOT));
    }

    #[test]
    fn a_ticket_comes_on_top_of_signing_in_not_instead() {
        let mut spec = spec_with(vec![
            requirement(&[USER_OR_BOT]),
            requirement(&["Valid MFA Ticket"]),
        ]);
        fix_security(&mut spec);
        assert_eq!(
            security_of(&spec),
            vec![vec!["Session Token".to_string(), "Valid MFA Ticket".to_string()]]
        );
    }

    /// PATCH /bots/{bot_id} needs a ticket only to remove the token
    #[test]
    fn a_ticket_needed_only_sometimes_leaves_signing_in_enough() {
        let mut spec = OpenApi::default();
        for path in ["/bots/{bot_id}", "/bots/{bot_id}/token/reset"] {
            let operation = Operation {
                security: Some(vec![
                    requirement(&[USER_OR_BOT]),
                    requirement(&["Valid MFA Ticket"]),
                ]),
                ..Default::default()
            };
            let item = if path == "/bots/{bot_id}" {
                PathItem { patch: Some(operation), ..Default::default() }
            } else {
                PathItem { get: Some(operation), ..Default::default() }
            };
            spec.paths.insert(path.to_owned(), item);
        }
        fix_security(&mut spec);
        let patch = spec.paths["/bots/{bot_id}"].patch.as_ref().unwrap();
        let mut alternatives: Vec<Vec<String>> = patch
            .security
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| {
                let mut names: Vec<String> = r.keys().cloned().collect();
                names.sort();
                names
            })
            .collect();
        alternatives.sort();
        assert_eq!(
            alternatives,
            vec![
                vec![BOT.to_string()],
                vec![SESSION.to_string()],
                vec![SESSION.to_string(), "Valid MFA Ticket".to_string()],
            ]
        );
        // a route that always needs it keeps it required
        assert_eq!(
            security_at(&spec, "/bots/{bot_id}/token/reset"),
            vec![vec![SESSION.to_string(), "Valid MFA Ticket".to_string()]]
        );
    }

    #[test]
    fn a_session_only_route_stays_session_only() {
        let mut spec = spec_with(vec![requirement(&[SESSION])]);
        fix_security(&mut spec);
        assert_eq!(security_of(&spec), vec![vec![SESSION.to_string()]]);
    }

    #[test]
    fn a_ticket_only_route_stays_as_it_was() {
        let mut spec = spec_with(vec![requirement(&["Unvalidated MFA Ticket"])]);
        fix_security(&mut spec);
        assert_eq!(
            security_of(&spec),
            vec![vec!["Unvalidated MFA Ticket".to_string()]]
        );
    }
}

/// Writes the API's own documents for the SDK snapshot (`sdk-js/api/`),
/// from whatever branch is built, without running any service:
///
///   touch /tmp/zrzur-openapi-export.want   (in WSL)
///   internal/test-env/cargo-locked.sh <backend> test 'test(/export_api_documents/)'
///
/// then /tmp/zrzur-openapi.json and /tmp/zrzur-gateway-schema.json hold
/// `GET /openapi.json` and `GET /gateway/schema`. Without the marker file
/// the test does nothing, so a normal run writes nothing.
#[cfg(test)]
mod export {
    use crate::util::test::TestHarness;
    use rocket::http::Status;

    #[rocket::async_test]
    async fn export_api_documents() {
        if !std::path::Path::new("/tmp/zrzur-openapi-export.want").exists() {
            return;
        }
        revolt_config::overwrite_config(|config| {
            config.features.webhooks_enabled = true;
        })
        .await;
        let harness = TestHarness::new().await;
        for (path, out) in [
            ("/openapi.json", "/tmp/zrzur-openapi.json"),
            ("/gateway/schema", "/tmp/zrzur-gateway-schema.json"),
        ] {
            let response = harness.client.get(path).dispatch().await;
            assert_eq!(response.status(), Status::Ok, "{path}");
            let body = response.into_string().await.expect("a body");
            std::fs::write(out, body).expect("written");
        }
    }
}
