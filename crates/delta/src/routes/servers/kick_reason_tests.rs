//! A kick (or any audited action) keeps the reason sent in
//! X-Audit-Log-Reason, also when it is URL-encoded Arabic.

use crate::{rocket, util::test::TestHarness};
use revolt_database::Member;
use rocket::http::{Header, Status};

#[rocket::async_test]
async fn a_kick_keeps_its_reason_in_the_audit_log() {
    let harness = TestHarness::new().await;
    let (_, owner, owner_user) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner_user).await;
    let (_, _, member) = harness.new_user().await;
    Member::create(&harness.db, &server, &member, None)
        .await
        .unwrap();

    let response = harness
        .client
        .delete(format!("/servers/{}/members/{}", server.id, member.id))
        .header(Header::new("x-session-token", owner.token.clone()))
        .header(Header::new("x-audit-log-reason", "%D8%B3%D8%A8%D8%A7%D9%85"))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);

    let audit = harness
        .client
        .get(format!("/servers/{}/audit_logs", server.id))
        .header(Header::new("x-session-token", owner.token.clone()))
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    assert!(audit.contains("MemberKick"), "{audit}");
    assert!(audit.contains("سبام"), "{audit}");
}
