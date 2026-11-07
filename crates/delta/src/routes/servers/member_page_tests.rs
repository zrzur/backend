//! The member list comes a page at a time, ordered by user id.

use std::collections::HashSet;

use crate::{rocket, util::test::TestHarness};
use futures::{stream, StreamExt};
use revolt_database::{Member, MemberCompositeKey, Server, Session, User};
use rocket::http::{Header, Status};

struct Setup {
    harness: TestHarness,
    server: Server,
    session: Session,
}

/// A server with its owner and `extra` more members
async fn setup(extra: usize) -> Setup {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (server, _) = harness.new_server(&owner).await;
    // the test server has no member entry for its owner; a real one does
    Member::create(&harness.db, &server, &owner, None).await.unwrap();
    // side by side: a thousand people, one after the other, take a real
    // database longer than a test may run
    stream::iter(0..extra)
        .map(|i| {
            let (harness, server) = (&harness, &server);
            async move {
                let user = ulid::Ulid::new().to_string();
                harness
                    .db
                    .insert_user(&User {
                        id: user.clone(),
                        username: format!("member{i}"),
                        discriminator: "0001".to_string(),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                harness
                    .db
                    .insert_or_merge_member(&Member {
                        id: MemberCompositeKey {
                            server: server.id.clone(),
                            user,
                        },
                        ..Default::default()
                    })
                    .await
                    .unwrap();
            }
        })
        .buffer_unordered(32)
        .collect::<Vec<_>>()
        .await;
    Setup {
        harness,
        server,
        session,
    }
}

impl Setup {
    async fn page(&self, query: &str) -> Vec<String> {
        self.page_and_next(query).await.0
    }

    async fn page_and_next(&self, query: &str) -> (Vec<String>, Option<String>) {
        let response = self
            .harness
            .client
            .get(format!("/servers/{}/members?{query}", self.server.id))
            .header(Header::new("x-session-token", self.session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.unwrap();
        let ids = body["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| member["_id"]["user"].as_str().unwrap().to_string())
            .collect();
        (ids, body["next"].as_str().map(str::to_string))
    }
}

#[rocket::async_test]
async fn pages_cover_every_member_once() {
    let setup = setup(2_499).await;

    let mut seen = Vec::new();
    let mut pages = 0;
    let mut query = "limit=1000".to_string();
    loop {
        let (page, next) = setup.page_and_next(&query).await;
        pages += 1;
        assert!(page.windows(2).all(|w| w[0] < w[1]), "ordered by user id");
        seen.extend(page);
        match next {
            Some(next) => query = format!("limit=1000&after={next}"),
            None => break,
        }
    }

    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 2_500, "nothing missing");
    assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 2_500, "no duplicates");
}

#[rocket::async_test]
async fn page_size_defaults_to_100_and_stops_at_1000() {
    let setup = setup(1_200).await;
    let first = setup.page("limit=5000").await;
    assert_eq!(first.len(), 1_000);

    let after = setup.page(&format!("after={}", first[0])).await;
    assert_eq!(after.len(), 100);
    assert_eq!(after[0], first[1]);
}

#[rocket::async_test]
async fn the_unpaged_list_is_capped_at_1000() {
    let setup = setup(1_200).await;
    assert_eq!(setup.page("").await.len(), 1_000);
}

#[rocket::async_test]
async fn offline_members_left_out_still_give_the_next_page() {
    // nobody is online in tests: pages come back empty, the cursor goes on
    let setup = setup(1_500).await;
    let (page, next) = setup
        .page_and_next("limit=1000&exclude_offline=true")
        .await;
    assert!(page.is_empty());
    let (_, last) = setup
        .page_and_next(&format!("limit=1000&exclude_offline=true&after={}", next.unwrap()))
        .await;
    assert_eq!(last, None);
}
