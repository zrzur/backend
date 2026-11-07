//! A bot's own HTTP endpoint, played by a small server on 127.0.0.1 that
//! really checks signatures the way the documentation tells bots to.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use revolt_database::{
    events::client::EventV1, util::signed_request::sign, FieldsBot, MFATicket, PartialBot,
};
use revolt_models::v0;
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::tests::{setup, Setup};
use crate::{
    rocket,
    util::{
        interaction_endpoint::verify_endpoint,
        signed_delivery::{post, Problem},
        test::PubSubTestHelper,
    },
};

#[derive(Clone, Debug)]
struct Received {
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

type Answer = Arc<dyn Fn(&Received) -> (u16, String) + Send + Sync>;

/// Answers every request with `answer`, keeps what it received
async fn endpoint(answer: Answer) -> (String, Arc<Mutex<Vec<Received>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/interactions", listener.local_addr().unwrap());
    let received: Arc<Mutex<Vec<Received>>> = Arc::default();
    let seen = received.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (seen, answer) = (seen.clone(), answer.clone());
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&chunk[..n]);
                    let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&data[..end]).to_string();
                    let headers: HashMap<String, String> = head
                        .lines()
                        .skip(1)
                        .filter_map(|line| line.split_once(": "))
                        .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                        .collect();
                    let length: usize = headers["content-length"].parse().unwrap();
                    if data.len() < end + 4 + length {
                        continue;
                    }
                    let request = Received {
                        headers,
                        body: data[end + 4..end + 4 + length].to_vec(),
                    };
                    seen.lock().unwrap().push(request.clone());
                    let (status, body) = answer(&request);
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    return;
                }
            });
        }
    });
    (url, received)
}

/// What the docs tell a bot to do: refuse bad signatures, answer pings
fn honest(secret: String, then: Value) -> Answer {
    Arc::new(move |request| {
        let timestamp: i64 = request.headers["x-zrzur-timestamp"].parse().unwrap();
        if sign(&secret, timestamp, &request.body) != request.headers["x-zrzur-signature"] {
            return (401, String::new());
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if body["type"] == "Ping" {
            return (200, json!({ "type": "Pong" }).to_string());
        }
        (200, then.to_string())
    })
}

async fn with_secret(s: &mut Setup) -> String {
    s.bot
        .update(
            &s.harness.db,
            PartialBot {
                interactions_secret_version: Some(1),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    revolt_database::util::signed_request::derive_secret(
        "test-signing-key-of-at-least-32-bytes",
        crate::util::interaction_endpoint::PURPOSE,
        &s.bot.id,
        1,
    )
}

async fn use_endpoint(s: &mut Setup, url: &str) {
    s.bot
        .update(
            &s.harness.db,
            PartialBot {
                interactions_endpoint: Some(url.to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

#[rocket::async_test]
async fn an_honest_endpoint_passes_and_a_careless_one_does_not() {
    let mut s = setup().await;
    let secret = with_secret(&mut s).await;

    let (url, received) = endpoint(honest(secret.clone(), json!({}))).await;
    verify_endpoint(&url, &s.bot).await.expect("honest endpoint");
    let received = received.lock().unwrap().clone();
    assert_eq!(received.len(), 2, "one wrong signature, one real");
    let ping: Value = serde_json::from_slice(&received[1].body).unwrap();
    assert_eq!(ping["type"], "Ping");
    assert_eq!(ping["application_id"].as_str(), Some(s.bot.id.as_str()));

    // says yes to everything: it would run forged interactions
    let (url, _) = endpoint(Arc::new(|_| (200, json!({ "type": "Pong" }).to_string()))).await;
    let error = verify_endpoint(&url, &s.bot).await.unwrap_err();
    assert!(format!("{:?}", error.error_type).contains("accepts_bad_signatures"), "{error:?}");

    // checks signatures, but does not answer the ping properly
    let wrong_answer: Answer = {
        let secret = secret.clone();
        Arc::new(move |request| {
            let timestamp: i64 = request.headers["x-zrzur-timestamp"].parse().unwrap();
            if sign(&secret, timestamp, &request.body)
                != request.headers["x-zrzur-signature"]
            {
                return (401, String::new());
            }
            (200, "not json".to_string())
        })
    };
    let (url, _) = endpoint(wrong_answer).await;
    let error = verify_endpoint(&url, &s.bot).await.unwrap_err();
    assert!(format!("{:?}", error.error_type).contains("bad_answer"), "{error:?}");
}

#[rocket::async_test]
async fn the_endpoint_answers_in_its_reply_and_the_gateway_stays_quiet() {
    let mut s = setup().await;
    let secret = with_secret(&mut s).await;
    let (url, received) = endpoint(honest(
        secret.clone(),
        json!({ "type": "Message", "data": { "content": "over http" } }),
    ))
    .await;
    use_endpoint(&mut s, &url).await;
    // no gateway session needed: this bot lives on HTTP only
    revolt_presence::delete_session(&s.bot.id, 0).await;

    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let mut channel_events = PubSubTestHelper::new(s.channel.id()).await;

    let (status, accepted) = s.invoke(s.body("http1", json!([]))).await;
    assert_eq!(status, Status::Ok);
    let accepted = accepted.unwrap();

    let message = channel_events.wait_for_message().await;
    assert_eq!(message.content.as_deref(), Some("over http"));
    assert_eq!(message.author, s.bot.id);

    let request = received.lock().unwrap()[0].clone();
    assert_eq!(request.headers["x-zrzur-delivery"], accepted.id);
    let timestamp: i64 = request.headers["x-zrzur-timestamp"].parse().unwrap();
    assert!((chrono_now() - timestamp).abs() < 30);
    assert_eq!(
        request.headers["x-zrzur-signature"],
        sign(&secret, timestamp, &request.body)
    );
    let interaction: v0::Interaction = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(interaction.id, accepted.id);

    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            bot_events.wait_for_event(|e| matches!(e, EventV1::InteractionCreate { .. }))
        )
        .await
        .is_err(),
        "the gateway must not get it too"
    );
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[rocket::async_test]
async fn a_failing_endpoint_tells_the_person_waiting() {
    let mut s = setup().await;
    with_secret(&mut s).await;
    let (url, _) = endpoint(Arc::new(|_| (500, String::new()))).await;
    use_endpoint(&mut s, &url).await;

    let mut user_events = PubSubTestHelper::new(&format!("{}!", s.user.id)).await;
    let (status, _) = s.invoke(s.body("http2", json!([]))).await;
    assert_eq!(status, Status::Ok);

    let event = user_events
        .wait_for_event(|e| matches!(e, EventV1::InteractionStateUpdate { .. }))
        .await;
    let EventV1::InteractionStateUpdate { state, .. } = event else { unreachable!() };
    assert_eq!(state, v0::InteractionState::Failed);
}

#[rocket::async_test]
async fn an_endpoint_may_answer_later_through_the_callback_route() {
    let mut s = setup().await;
    let secret = with_secret(&mut s).await;
    let later: Answer = Arc::new(move |request| {
        let _ = &secret;
        let _ = request;
        (202, String::new())
    });
    let (url, received) = endpoint(later).await;
    use_endpoint(&mut s, &url).await;
    let mut channel_events = PubSubTestHelper::new(s.channel.id()).await;

    let (status, _) = s.invoke(s.body("http3", json!([]))).await;
    assert_eq!(status, Status::Ok);

    // wait for the delivery, then answer as a bot would
    let mut interaction = None;
    for _ in 0..100 {
        if let Some(request) = received.lock().unwrap().first() {
            interaction = Some(serde_json::from_slice::<v0::Interaction>(&request.body).unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let interaction = interaction.expect("delivered");
    let response = s
        .harness
        .client
        .post(format!(
            "/interactions/{}/{}/callback",
            interaction.id,
            interaction.token.unwrap()
        ))
        .header(ContentType::JSON)
        .body(json!({ "type": "Message", "data": { "content": "later" } }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    assert_eq!(channel_events.wait_for_message().await.content.as_deref(), Some("later"));
}

#[rocket::async_test]
async fn only_the_owner_sets_an_endpoint_and_a_secret_needs_a_ticket() {
    let s = setup().await;
    let (_, session, owner) = s.harness.new_user().await;
    let (bot, _) = s.harness.new_bot(&owner).await;

    // somebody else's bot does not exist for you
    let response = s
        .harness
        .client
        .put(format!("/bots/{}/interactions/endpoint", bot.id))
        .header(Header::new("x-session-token", s.user_session.clone()))
        .header(Header::new("x-mfa-ticket", "not needed to get a 404"))
        .header(ContentType::JSON)
        .body(json!({ "url": "http://127.0.0.1:1/x" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NotFound);

    // no ticket, no secret
    let secret = || {
        s.harness
            .client
            .post(format!("/bots/{}/interactions/secret", bot.id))
            .header(Header::new("x-session-token", session.token.clone()))
    };
    assert_eq!(secret().dispatch().await.status(), Status::Unauthorized);

    let ticket = MFATicket::new(owner.id.to_string(), true);
    ticket.save(&s.harness.db).await.unwrap();
    let response = secret()
        .header(Header::new("x-mfa-ticket", ticket.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let created: v0::InteractionsSecret = response.into_json().await.unwrap();
    assert!(created.secret.starts_with("zrzur_sign_"));
    assert_eq!(created.version, 1);

    // only a generation number is stored, never the secret
    let stored = s.harness.db.fetch_bot(&bot.id).await.unwrap();
    assert_eq!(stored.interactions_secret_version, 1);
    assert!(!serde_json::to_string(&stored).unwrap().contains(&created.secret));

    // no endpoint without a secret to sign with
    let (_, session2, owner2) = s.harness.new_user().await;
    let (bot2, _) = s.harness.new_bot(&owner2).await;
    let put = |ticket: Option<String>| {
        let mut request = s
            .harness
            .client
            .put(format!("/bots/{}/interactions/endpoint", bot2.id))
            .header(Header::new("x-session-token", session2.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "url": "http://127.0.0.1:1/x" }).to_string());
        if let Some(ticket) = ticket {
            request = request.header(Header::new("x-mfa-ticket", ticket));
        }
        request
    };
    // this address will get what people type: no ticket, no change
    assert_eq!(put(None).dispatch().await.status(), Status::Unauthorized);
    let ticket = MFATicket::new(owner2.id.to_string(), true);
    ticket.save(&s.harness.db).await.unwrap();
    let response = put(Some(ticket.token.clone())).dispatch().await;
    assert_eq!(response.status(), Status::BadRequest);
}

/// A server that always gives the same raw answer, after a pause
async fn fixed(reply: Vec<u8>, pause: Duration) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/x", listener.local_addr().unwrap());
    let hits: Arc<Mutex<usize>> = Arc::default();
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (reply, counter) = (reply.clone(), counter.clone());
            tokio::spawn(async move {
                let mut request = [0u8; 8192];
                let _ = socket.read(&mut request).await;
                *counter.lock().unwrap() += 1;
                tokio::time::sleep(pause).await;
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    (url, hits)
}

fn parsed(url: &str) -> url::Url {
    url.parse().unwrap()
}

#[rocket::async_test]
async fn a_redirect_is_never_followed() {
    let (target, target_hits) = fixed(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(), Duration::ZERO).await;
    let redirect = format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n");
    let (url, _) = fixed(redirect.into_bytes(), Duration::ZERO).await;

    let answer = post(&parsed(&url), "secret", "delivery", b"{}", true).await.expect("an answer");
    assert_eq!(answer.status, 302);
    assert_eq!(*target_hits.lock().unwrap(), 0, "nothing went to the target");

    // and as an endpoint it does not pass
    let mut s = setup().await;
    with_secret(&mut s).await;
    let error = verify_endpoint(&url, &s.bot).await.unwrap_err();
    assert!(format!("{:?}", error.error_type).contains("bad_status"), "{error:?}");
    assert_eq!(*target_hits.lock().unwrap(), 0);
}

#[rocket::async_test]
async fn an_answer_over_the_cap_is_refused() {
    let size = 1024 * 1024 + 1;
    let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n").into_bytes();
    reply.extend(std::iter::repeat_n(b'x', size));
    let (url, _) = fixed(reply, Duration::ZERO).await;
    assert_eq!(
        post(&parsed(&url), "secret", "delivery", b"{}", true).await.err(),
        Some(Problem::AnswerTooLarge)
    );
}

#[rocket::async_test]
async fn a_slow_endpoint_times_out() {
    let (url, _) = fixed(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(), Duration::from_secs(5)).await;
    assert_eq!(
        post(&parsed(&url), "secret", "delivery", b"{}", true).await.err(),
        Some(Problem::TimedOut)
    );
}

#[rocket::async_test]
async fn the_two_test_requests_have_their_own_ids_and_change_order() {
    let mut s = setup().await;
    let secret = with_secret(&mut s).await;
    let (url, received) = endpoint(honest(secret.clone(), json!({}))).await;
    let mut wrong_first = 0;
    for run in 0..12 {
        verify_endpoint(&url, &s.bot).await.expect("honest endpoint");
        let requests = received.lock().unwrap().clone();
        let (first, second) = (&requests[run * 2], &requests[run * 2 + 1]);
        assert_ne!(first.headers["x-zrzur-delivery"], second.headers["x-zrzur-delivery"]);
        let timestamp: i64 = first.headers["x-zrzur-timestamp"].parse().unwrap();
        if sign(&secret, timestamp, &first.body) != first.headers["x-zrzur-signature"] {
            wrong_first += 1;
        }
    }
    assert!((1..12).contains(&wrong_first), "the order must vary: {wrong_first}");
}

/// a disabled bot gets nothing, whatever it says it has saved
#[rocket::async_test]
async fn a_disabled_bot_receives_nothing_over_http() {
    let mut s = setup().await;
    let secret = with_secret(&mut s).await;
    let (url, received) = endpoint(honest(secret, json!({ "type": "Message", "data": { "content": "x" } }))).await;
    use_endpoint(&mut s, &url).await;
    s.bot
        .update(
            &s.harness.db,
            PartialBot {
                flags: Some(revolt_database::BOT_FLAG_SUSPENDED),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let (status, _) = s.invoke(s.body("disabled", json!([]))).await;
    assert_eq!(status, Status::ServiceUnavailable);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(received.lock().unwrap().is_empty(), "nothing may reach it");
    // the address stays for reinstatement
    let stored = s.harness.db.fetch_bot(&s.bot.id).await.unwrap();
    assert_eq!(stored.interactions_endpoint.as_deref(), Some(url.as_str()));
}

/// nor does the bot of a suspended or banned owner
#[rocket::async_test]
async fn the_bot_of_a_blocked_owner_receives_nothing() {
    let mut s = setup().await;
    let secret = with_secret(&mut s).await;
    let (url, received) = endpoint(honest(secret, json!({ "type": "Message", "data": { "content": "x" } }))).await;
    use_endpoint(&mut s, &url).await;

    for (nonce, flag) in [
        ("banned", v0::UserFlags::Banned as i32),
        ("suspended", v0::UserFlags::SuspendedUntil as i32),
    ] {
        let owner = s.harness.db.fetch_user(&s.bot.owner).await.unwrap();
        s.harness
            .db
            .update_user(
                &owner.id,
                &revolt_database::PartialUser {
                    flags: Some(flag),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        let (status, _) = s.invoke(s.body(nonce, json!([]))).await;
        assert_eq!(status, Status::ServiceUnavailable, "{nonce}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(received.lock().unwrap().is_empty(), "nothing may reach it");
}

/// and the routes of a disabled bot change nothing
#[rocket::async_test]
async fn a_disabled_bots_endpoint_routes_are_closed() {
    let s = setup().await;
    let (_, session, owner) = s.harness.new_user().await;
    let (mut bot, _) = s.harness.new_bot(&owner).await;
    bot.update(
        &s.harness.db,
        PartialBot {
            flags: Some(revolt_database::BOT_FLAG_SUSPENDED),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();
    let ticket = MFATicket::new(owner.id.to_string(), true);
    ticket.save(&s.harness.db).await.unwrap();

    let secret = s
        .harness
        .client
        .post(format!("/bots/{}/interactions/secret", bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(Header::new("x-mfa-ticket", ticket.token.clone()))
        .dispatch()
        .await;
    assert_eq!(secret.status(), Status::BadRequest);
    let remove = s
        .harness
        .client
        .delete(format!("/bots/{}/interactions/endpoint", bot.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(remove.status(), Status::BadRequest);
}

#[rocket::async_test]
async fn removing_the_endpoint_brings_the_gateway_back() {
    let mut s = setup().await;
    with_secret(&mut s).await;
    use_endpoint(&mut s, "http://127.0.0.1:1/never").await;
    s.bot
        .update(&s.harness.db, PartialBot::default(), vec![FieldsBot::InteractionsEndpoint])
        .await
        .unwrap();

    let mut bot_events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;
    let (status, _) = s.invoke(s.body("http4", json!([]))).await;
    assert_eq!(status, Status::Ok);
    bot_events
        .wait_for_event(|e| matches!(e, EventV1::InteractionCreate { .. }))
        .await;
}
