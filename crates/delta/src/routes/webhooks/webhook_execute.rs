use revolt_config::config;
use revolt_database::{
    util::{idempotency::IdempotencyKey, reference::Reference},
    Database, Webhook, AMQP,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{
    form::Form,
    fs::TempFile,
    serde::json::Json,
    tokio::io::AsyncReadExt,
    State,
};

use super::delivery::{deliver, message_from_body, MaybeMessage};
use crate::util::webhooks::WebhookCaller;

/// # Executes a webhook
///
/// Sends a message. Answers 204 unless `wait=true`, which returns the
/// message. `thread_id` posts into a thread of the webhook's channel.
#[openapi(tag = "Webhooks")]
#[post("/<webhook_id>/<token>?<wait>&<thread_id>", data = "<data>", rank = 2)]
#[allow(clippy::too_many_arguments)]
pub async fn webhook_execute(
    db: &State<Database>,
    amqp: &State<AMQP>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    wait: Option<bool>,
    thread_id: Option<String>,
    data: Json<v0::DataWebhookExecute>,
    idempotency: IdempotencyKey,
) -> Result<MaybeMessage> {
    let webhook = caller.authorise(db, webhook_id, &token).await?;
    let (message, mut delivery) = message_from_body(data.into_inner(), &[])?;
    delivery.thread_id = thread_id;
    let sent = deliver(db, amqp, &caller, &webhook, message, delivery, idempotency).await?;
    Ok(MaybeMessage(
        wait.unwrap_or(false).then(|| sent.into_model(None, None)),
    ))
}

/// Parts of a multipart request: `payload_json` holds the same body as the
/// JSON form, or the plain fields stand alone. Files are `files[0]`,
/// `files[1]`, ... or a single `file`.
#[derive(FromForm)]
pub struct WebhookForm<'r> {
    payload_json: Option<String>,
    content: Option<String>,
    username: Option<String>,
    avatar_url: Option<String>,
    thread_name: Option<String>,
    files: Vec<TempFile<'r>>,
    file: Option<TempFile<'r>>,
}

/// # Executes a webhook with files
///
/// As the JSON form, as `multipart/form-data`.
#[openapi(skip)]
#[post(
    "/<webhook_id>/<token>?<wait>&<thread_id>",
    data = "<data>",
    format = "multipart/form-data",
    rank = 1
)]
#[allow(clippy::too_many_arguments)]
pub async fn webhook_execute_files(
    db: &State<Database>,
    amqp: &State<AMQP>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    wait: Option<bool>,
    thread_id: Option<String>,
    data: Form<WebhookForm<'_>>,
    idempotency: IdempotencyKey,
) -> Result<MaybeMessage> {
    let webhook = caller.authorise(db, webhook_id, &token).await?;
    let form = data.into_inner();

    let mut body: v0::DataWebhookExecute = match &form.payload_json {
        Some(json) => serde_json::from_str(json).map_err(|error| {
            log::debug!("webhook payload_json refused: {error}");
            revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new(
                "payload_json",
                "format",
            )])
        })?,
        None => Default::default(),
    };
    body.content = body.content.or(form.content);
    body.username = body.username.or(form.username);
    body.avatar_url = body.avatar_url.or(form.avatar_url);
    body.thread_name = body.thread_name.or(form.thread_name);

    let files: Vec<&TempFile<'_>> = form.files.iter().chain(form.file.iter()).collect();
    let uploads = upload_files(&webhook, &token, &files).await?;

    let (message, mut delivery) = message_from_body(body, &uploads)?;
    delivery.thread_id = thread_id;
    let sent = deliver(db, amqp, &caller, &webhook, message, delivery, idempotency).await?;
    Ok(MaybeMessage(
        wait.unwrap_or(false).then(|| sent.into_model(None, None)),
    ))
}

/// A file name fit for a multipart header: its last path part, printable
fn safe_file_name(file: &TempFile<'_>) -> String {
    let raw = file
        .raw_name()
        .map(|name| name.dangerous_unsafe_unsanitized_raw().as_str().to_string())
        .unwrap_or_default();
    let name: String = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .take(128)
        .collect();
    if name.trim().is_empty() {
        "file".to_string()
    } else {
        name
    }
}

/// The multipart body for the file service
fn multipart_body(boundary: &str, name: &str, content_type: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes.len() + 256);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Hand one file to the file service, which checks it as any upload.
/// The token goes in a header: addresses end up in logs.
async fn upload_one(
    base: &str,
    webhook_id: &str,
    token: &str,
    name: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Uploaded {
        id: String,
    }

    let boundary = ulid::Ulid::new().to_string();
    let response = reqwest::Client::new()
        .post(format!("{base}/webhooks/{webhook_id}/attachments"))
        .header("x-webhook-token", token)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_body(&boundary, name, content_type, bytes))
        .send()
        .await
        .map_err(|_| create_error!(ProxyError))?;

    let status = response.status();
    if status.is_success() {
        let uploaded: Uploaded = response.json().await.map_err(|_| create_error!(ProxyError))?;
        return Ok(uploaded.id);
    }
    // the service's own refusal (type not allowed, too small, ...) says it best
    Err(response
        .json::<revolt_result::Error>()
        .await
        .unwrap_or(create_error!(ProxyError)))
}

/// Check and upload the files of a request; `(name, id)` of each
async fn upload_files(
    webhook: &Webhook,
    token: &str,
    files: &[&TempFile<'_>],
) -> Result<Vec<(String, String)>> {
    if files.is_empty() {
        return Ok(vec![]);
    }

    // A clear answer, never a message that silently lost its files
    if !webhook.can_upload_files() {
        return Err(create_error!(MissingPermission {
            permission: "UploadFiles".to_string()
        }));
    }

    let config = config().await;
    let limits = &config.features.limits.default;
    if files.len() > limits.message_attachments {
        return Err(create_error!(TooManyAttachments {
            max: limits.message_attachments,
        }));
    }
    let max = limits
        .file_upload_size_limit
        .get("attachments")
        .copied()
        .unwrap_or_default();
    if files.iter().any(|file| file.len() as usize > max) {
        return Err(create_error!(FileTooLarge { max }));
    }

    let mut uploads = Vec::with_capacity(files.len());
    for file in files {
        let name = safe_file_name(file);
        let content_type = file
            .content_type()
            .map(|content_type| content_type.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let mut bytes = Vec::with_capacity(file.len() as usize);
        let reader = file.open().await.map_err(|_| create_error!(InternalError))?;
        Box::pin(reader)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| create_error!(InternalError))?;

        let id = upload_one(
            &config.hosts.autumn,
            &webhook.id,
            token,
            &name,
            &content_type,
            &bytes,
        )
        .await?;
        uploads.push((name, id));
    }
    Ok(uploads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_body_has_the_shape_the_file_service_reads() {
        let body = multipart_body("B", "a.png", "image/png", b"123");
        let text = String::from_utf8(body).unwrap();
        assert_eq!(
            text,
            "--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n123\r\n--B--\r\n"
        );
    }

    /// One answer on a local port, standing in for the file service
    async fn file_service(answer: &'static str) -> (String, rocket::tokio::task::JoinHandle<String>) {
        use rocket::tokio::io::AsyncWriteExt;
        let listener = rocket::tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = rocket::tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = vec![0u8; 4096];
            let read = socket.read(&mut seen).await.unwrap();
            socket.write_all(answer.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&seen[..read]).to_string()
        });
        (address, handle)
    }

    #[rocket::async_test]
    async fn the_token_goes_in_a_header_never_in_the_address() {
        let (base, seen) = file_service(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"id\":\"F1\"}",
        )
        .await;
        let id = upload_one(&base, "W", "zrzur_wh_secret", "a.png", "image/png", b"x")
            .await
            .unwrap();
        assert_eq!(id, "F1");
        let request = seen.await.unwrap();
        let first_line = request.lines().next().unwrap();
        assert_eq!(first_line, "POST /webhooks/W/attachments HTTP/1.1");
        assert!(request.to_lowercase().contains("x-webhook-token: zrzur_wh_secret"));
    }

    #[rocket::async_test]
    async fn the_file_service_refusal_reaches_the_caller() {
        let (base, _) = file_service(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: 29\r\n\r\n{\"type\":\"FileTypeNotAllowed\"}",
        )
        .await;
        let error = upload_one(&base, "W", "t", "a.exe", "application/x-msdownload", b"x")
            .await
            .unwrap_err();
        assert!(matches!(error.error_type, revolt_result::ErrorType::FileTypeNotAllowed));
    }
}
