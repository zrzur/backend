use crate::AppState;
use axum::{
    http::Method,
    routing::{get, Router},
};
use revolt_config::CorsOrigins;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};

pub mod categories;
pub mod klipy;
pub mod root;
pub mod search;
pub mod trending;

/// Which websites may read answers in a browser (S-02)
fn cors_layer(origins: CorsOrigins) -> CorsLayer {
    CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_headers(AllowHeaders::mirror_request())
        .expose_headers(vec![
            "X-RateLimit-Limit".try_into().unwrap(),
            "X-RateLimit-Bucket".try_into().unwrap(),
            "X-RateLimit-Remaining".try_into().unwrap(),
            "X-RateLimit-Reset-After".try_into().unwrap(),
        ])
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin.to_str().is_ok_and(|origin| origins.allows(origin))
        }))
}

pub fn router(origins: CorsOrigins) -> Router<AppState> {
    let cors = cors_layer(origins);

    Router::new()
        .route("/", get(root::root))
        .route("/categories", get(categories::categories))
        .route("/search", get(search::search))
        .route("/trending", get(trending::trending))
        .route("/klipy/categories", get(klipy::categories))
        .route("/klipy/gifs", get(klipy::gifs))
        .route("/klipy/media", get(klipy::media).head(klipy::media_head))
        .layer(cors)
}

#[cfg(test)]
mod cors_tests {
    use std::time::Duration;

    use revolt_config::CorsOrigins;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    /// Ask a running service as a browser on `origin` does; the answer's headers
    async fn ask(port: u16, method: &str, origin: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = format!(
            "{method} / HTTP/1.1\r\nHost: test\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: GET\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8_lossy(&answer).to_lowercase()
    }

    #[tokio::test]
    async fn cors_answers_a_listed_origin_and_no_other() {
        let origins = CorsOrigins::new("https://app.zrzur.com", &[], Some("https://chat.example"));
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .layer(super::cors_layer(origins));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        for method in ["GET", "OPTIONS"] {
            for origin in ["https://discover.zrzur.com", "https://chat.example"] {
                let answer = ask(port, method, origin).await;
                let header = format!("access-control-allow-origin: {origin}");
                assert!(answer.contains(&header), "{method} {origin}: {answer}");
                // a cache must keep answers per origin
                assert!(answer.contains("vary: origin"), "{method} {origin}: {answer}");
            }
            for origin in ["https://evil.example", "http://localhost:3000"] {
                let answer = ask(port, method, origin).await;
                assert!(!answer.contains("access-control-allow-origin"), "{method} {origin}: {answer}");
                assert!(answer.contains("vary: origin"), "{method} {origin}: {answer}");
            }
        }
    }
}
