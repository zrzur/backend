use revolt_database::{AuditLogEntryAction, Database, PartialMember, Server, User,
    util::reference::Reference, Channel};
use revolt_models::v0;
use revolt_permissions::DEFAULT_PERMISSION_SERVER;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use crate::util::audit_log_reason::AuditLogReason;
use revolt_database::events::client::EventV1;

/// Public opt-in roles may only grant ordinary member permissions. Rechecked
/// when answering, so later role/channel edits cannot turn a choice into admin access.
pub(super) async fn check_public_role(db: &Database, server: &Server, id: &str) -> Result<()> {
    let role = server.roles.get(id).ok_or_else(|| create_error!(InvalidRole))?;
    if role.is_managed() || (role.permissions.a as u64 & !*DEFAULT_PERMISSION_SERVER) != 0 {
        return Err(create_error!(InvalidOperation));
    }
    for channel_id in &server.channels {
        if let Channel::TextChannel { role_permissions, .. } = db.fetch_channel(channel_id).await? {
            if role_permissions.get(id).is_some_and(|p| (p.a as u64 & !*DEFAULT_PERMISSION_SERVER) != 0) {
                return Err(create_error!(InvalidOperation));
            }
        }
    }
    Ok(())
}

/// # Answer Lobby Question
/// Add the role attached to a public interest choice to the authenticated member.
/// Existing roles are preserved; answering again is idempotent.
#[openapi(tag = "Server Members")]
#[post("/<server_id>/lobby/answers", data = "<data>")]
pub async fn answer(db: &State<Database>, user: User, server_id: Reference<'_>,
    data: Json<v0::DataLobbyAnswer>) -> Result<Json<v0::Member>> {
    let server = server_id.as_server(db).await?;
    let mut member = Reference::from_unchecked(&user.id).as_member(db, &server.id).await?;
    if member.in_timeout() { return Err(create_error!(InvalidOperation)); }
    // asked in their own window, so they work with the lobby page off
    let lobby = server.lobby.as_ref()
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let option = lobby.onboarding.iter().find(|q| q.id == data.question)
        .and_then(|q| q.options.iter().find(|o| o.id == data.option))
        .ok_or_else(|| create_error!(InvalidOperation))?;
    check_public_role(db, &server, &option.role).await?;
    if !member.roles.contains(&option.role) {
        let mut roles = member.roles.clone();
        roles.push(option.role.clone());
        let partial = PartialMember { roles: Some(roles), ..Default::default() };
        let before = member.generate_diff(&partial, &[]);
        member = db.add_member_role(&member.id, &option.role).await?;
        let partial = PartialMember { roles: Some(member.roles.clone()), ..Default::default() };
        EventV1::ServerMemberUpdate { id: member.id.clone().into(), data: partial.clone().into(), clear: vec![] }
            .p(server.id.clone()).await;
        AuditLogEntryAction::MemberEdit { user: user.id.clone(), before, after: partial }
            .insert(db, server.id, AuditLogReason(Some("Lobby interest choice".into())),
                user.id.clone(), Some(user.id)).await;
    }
    Ok(Json(member.into()))
}

/// # Remove Lobby Answer
/// Take back an interest choice: removes the role of that configured answer from
/// the authenticated member. Other roles stay; removing again is idempotent.
#[openapi(tag = "Server Members")]
#[delete("/<server_id>/lobby/answers/<question>/<option>")]
pub async fn remove(db: &State<Database>, user: User, server_id: Reference<'_>,
    question: String, option: String) -> Result<Json<v0::Member>> {
    let server = server_id.as_server(db).await?;
    let mut member = Reference::from_unchecked(&user.id).as_member(db, &server.id).await?;
    if member.in_timeout() { return Err(create_error!(InvalidOperation)); }
    // asked in their own window, so they work with the lobby page off
    let lobby = server.lobby.as_ref()
        .ok_or_else(|| create_error!(InvalidOperation))?;
    let role = lobby.onboarding.iter().find(|q| q.id == question)
        .and_then(|q| q.options.iter().find(|o| o.id == option))
        .map(|o| o.role.clone())
        .ok_or_else(|| create_error!(InvalidOperation))?;
    if member.roles.contains(&role) {
        let partial = PartialMember { roles: Some(member.roles.iter().filter(|r| **r != role).cloned().collect()), ..Default::default() };
        let before = member.generate_diff(&partial, &[]);
        member = db.remove_member_role(&member.id, &role).await?;
        let partial = PartialMember { roles: Some(member.roles.clone()), ..Default::default() };
        EventV1::ServerMemberUpdate { id: member.id.clone().into(), data: partial.clone().into(), clear: vec![] }
            .p(server.id.clone()).await;
        AuditLogEntryAction::MemberEdit { user: user.id.clone(), before, after: partial }
            .insert(db, server.id, AuditLogReason(Some("Lobby interest removed".into())),
                user.id.clone(), Some(user.id)).await;
    }
    Ok(Json(member.into()))
}

#[cfg(test)]
mod tests {
    use crate::util::test::TestHarness;
    use revolt_database::{Member, Role, PartialServer};
    use revolt_permissions::{ChannelPermission, OverrideField};
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    /// Exercise the real API: membership, forged choices, persistence, idempotency,
    /// preserving unrelated roles, disabled forms and permission changes after setup.
    #[rocket::async_test]
    async fn lobby_roles_are_public_validated_and_additive() {
        let h = TestHarness::new().await;
        let (_, owner_session, owner) = h.new_user().await;
        let (_, session, user) = h.new_user().await;
        let (_, outsider_session, _) = h.new_user().await;
        let (mut server, channels) = h.new_server(&owner).await;
        Member::create(&h.db, &server, &owner, Some(channels.clone())).await.unwrap();
        let (member, _) = Member::create(&h.db, &server, &user, Some(channels)).await.unwrap();
        let first = Role::create(&h.db, &server, "Games".into()).await.unwrap();
        server = h.db.fetch_server(&server.id).await.unwrap();
        let second = Role::create(&h.db, &server, "Art".into()).await.unwrap();
        server = h.db.fetch_server(&server.id).await.unwrap();
        let unrelated = Role::create(&h.db, &server, "Existing".into()).await.unwrap();
        h.db.add_member_role(&member.id, &unrelated.id).await.unwrap();

        let config = json!({"lobby": {"enabled": true, "onboarding": [{
            "id": "interests", "question": "What do you like?", "options": [
                {"id": "games", "label": "Games", "role": first.id},
                {"id": "art", "label": "Art", "role": second.id}
            ]
        }]}});
        let response = h.client.patch(format!("/servers/{}", server.id))
            .header(ContentType::JSON).header(Header::new("x-session-token", owner_session.token.clone()))
            .body(config.to_string()).dispatch().await;
        assert_eq!(response.status(), Status::Ok);

        let path = format!("/servers/{}/lobby/answers", server.id);
        for (token, body, expected) in [
            (&outsider_session.token, json!({"question":"interests","option":"games"}), Status::NotFound),
            (&session.token, json!({"question":"interests","option":"forged"}), Status::BadRequest),
            (&session.token, json!({"question":"interests","option":"games"}), Status::Ok),
            (&session.token, json!({"question":"interests","option":"games"}), Status::Ok),
            (&session.token, json!({"question":"interests","option":"art"}), Status::Ok),
        ] {
            let response = h.client.post(&path).header(ContentType::JSON)
                .header(Header::new("x-session-token", token.clone())).body(body.to_string()).dispatch().await;
            assert_eq!(response.status(), expected, "{body}");
        }
        let stored = h.db.fetch_member(&server.id, &user.id).await.unwrap();
        assert_eq!(stored.roles.len(), 3);
        for role in [&first.id, &second.id, &unrelated.id] { assert!(stored.roles.contains(role)); }

        // A formerly harmless role becoming elevated must stop being self assignable.
        server = h.db.fetch_server(&server.id).await.unwrap();
        server.set_role_permission(&h.db, &first.id, OverrideField { a: ChannelPermission::ManageServer as i64, d: 0 }).await.unwrap();
        let response = h.client.post(&path).header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.clone()))
            .body(json!({"question":"interests","option":"games"}).to_string()).dispatch().await;
        assert_eq!(response.status(), Status::BadRequest);

        // The questions are asked in their own window on first join, so they
        // keep working when the lobby page is switched off.
        let mut lobby = server.lobby.clone().unwrap(); lobby.enabled = false;
        server.update(&h.db, PartialServer { lobby: Some(lobby), ..Default::default() }, vec![]).await.unwrap();
        let response = h.client.post(&path).header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token))
            .body(json!({"question":"interests","option":"art"}).to_string()).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
    }

    /// Members change their mind: removing an answer drops only that answer's role,
    /// is idempotent, and cannot be used on choices that are not configured.
    #[rocket::async_test]
    async fn members_can_drop_an_interest() {
        let h = TestHarness::new().await;
        let (_, owner_session, owner) = h.new_user().await;
        let (_, session, user) = h.new_user().await;
        let (_, outsider_session, _) = h.new_user().await;
        let (mut server, channels) = h.new_server(&owner).await;
        Member::create(&h.db, &server, &owner, Some(channels.clone())).await.unwrap();
        let (member, _) = Member::create(&h.db, &server, &user, Some(channels)).await.unwrap();
        let first = Role::create(&h.db, &server, "Games".into()).await.unwrap();
        server = h.db.fetch_server(&server.id).await.unwrap();
        let second = Role::create(&h.db, &server, "Art".into()).await.unwrap();
        server = h.db.fetch_server(&server.id).await.unwrap();
        let unrelated = Role::create(&h.db, &server, "Existing".into()).await.unwrap();
        h.db.add_member_role(&member.id, &unrelated.id).await.unwrap();

        let config = json!({"lobby": {"enabled": true, "onboarding": [{
            "id": "interests", "question": "What do you like?", "options": [
                {"id": "games", "label": "Games", "role": first.id},
                {"id": "art", "label": "Art", "role": second.id}
            ]
        }]}});
        let response = h.client.patch(format!("/servers/{}", server.id))
            .header(ContentType::JSON).header(Header::new("x-session-token", owner_session.token.clone()))
            .body(config.to_string()).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let path = format!("/servers/{}/lobby/answers", server.id);
        for option in ["games", "art"] {
            let response = h.client.post(&path).header(ContentType::JSON)
                .header(Header::new("x-session-token", session.token.clone()))
                .body(json!({"question":"interests","option":option}).to_string()).dispatch().await;
            assert_eq!(response.status(), Status::Ok);
        }

        for (token, option, expected) in [
            (&outsider_session.token, "games", Status::NotFound),
            (&session.token, "forged", Status::BadRequest),
            (&session.token, "games", Status::Ok),
            (&session.token, "games", Status::Ok),
        ] {
            let response = h.client.delete(format!("{path}/interests/{option}"))
                .header(Header::new("x-session-token", token.clone())).dispatch().await;
            assert_eq!(response.status(), expected, "{option}");
        }
        let stored = h.db.fetch_member(&server.id, &user.id).await.unwrap();
        assert!(!stored.roles.contains(&first.id));
        for role in [&second.id, &unrelated.id] { assert!(stored.roles.contains(role)); }
    }
}
