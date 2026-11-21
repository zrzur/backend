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

fn fix_operation(operation: &mut Operation) {
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
        vec![requirement(&both)]
    });
}

/// Rewrite the security of every operation, and the schemes they name
pub fn fix_security(spec: &mut OpenApi) {
    for item in spec.paths.values_mut() {
        for operation in [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.options,
            &mut item.head,
            &mut item.patch,
            &mut item.trace,
        ]
        .into_iter()
        .flatten()
        {
            fix_operation(operation);
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
        let mut spec = OpenApi::default();
        spec.paths.insert(
            "/x".to_owned(),
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
        let mut out: Vec<Vec<String>> = spec.paths["/x"]
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
