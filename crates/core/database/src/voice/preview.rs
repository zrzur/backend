//! Stream previews: a small still of an active screen share, taken by the
//! sharer's own client (the server never decodes video). Kept in Redis for a
//! short while only; a share that stopped has no preview a minute later.

use base64::{engine::general_purpose::STANDARD, Engine};
use redis_kiss::{redis::cmd, AsyncCommands};
use revolt_models::v0::StreamPreview;
use revolt_result::{create_error, Result, ToRevoltError};

use super::get_connection;

/// Largest preview image, in bytes
pub const STREAM_PREVIEW_MAX_BYTES: usize = 20_000;
/// A preview is replaced at most this often
pub const STREAM_PREVIEW_INTERVAL_SECS: u64 = 25;
/// How long a preview is kept without a newer one
pub const STREAM_PREVIEW_TTL_SECS: u64 = 75;

pub(crate) fn preview_key(channel_id: &str, user_id: &str) -> String {
    format!("stream_preview:{channel_id}:{user_id}")
}

pub(crate) fn cooldown_key(channel_id: &str, user_id: &str) -> String {
    format!("stream_preview_cd:{channel_id}:{user_id}")
}

/// The type of a preview image, from its first bytes (JPEG or WebP only)
pub fn preview_content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Check a preview sent as base64: small, and a JPEG or WebP picture
pub fn check_stream_preview(image: &str) -> Result<&'static str> {
    // base64 is 4 characters for every 3 bytes
    if image.len() > STREAM_PREVIEW_MAX_BYTES.div_ceil(3) * 4 {
        return Err(create_error!(PayloadTooLarge));
    }
    let bytes = STANDARD
        .decode(image)
        .map_err(|_| create_error!(FileTypeNotAllowed))?;
    if bytes.len() > STREAM_PREVIEW_MAX_BYTES {
        return Err(create_error!(PayloadTooLarge));
    }
    preview_content_type(&bytes).ok_or_else(|| create_error!(FileTypeNotAllowed))
}

/// Store a new preview. Returns false when the last one is too recent.
pub async fn set_stream_preview(channel_id: &str, user_id: &str, image: &str) -> Result<bool> {
    let content_type = check_stream_preview(image)?;
    let mut conn = get_connection().await?.into_inner();

    let fresh: Option<String> = cmd("SET")
        .arg(cooldown_key(channel_id, user_id))
        .arg("1")
        .arg("NX")
        .arg("EX")
        .arg(STREAM_PREVIEW_INTERVAL_SECS)
        .query_async(&mut conn)
        .await
        .to_internal_error()?;
    if fresh.is_none() {
        return Ok(false);
    }

    let preview = StreamPreview {
        image: image.to_string(),
        content_type: content_type.to_string(),
    };
    conn.set_ex::<_, _, ()>(
        preview_key(channel_id, user_id),
        serde_json::to_string(&preview).to_internal_error()?,
        STREAM_PREVIEW_TTL_SECS as usize,
    )
    .await
    .to_internal_error()?;
    Ok(true)
}

/// The latest preview of someone's screen share, if there is one
pub async fn get_stream_preview(channel_id: &str, user_id: &str) -> Result<Option<StreamPreview>> {
    let value: Option<String> = get_connection()
        .await?
        .get(preview_key(channel_id, user_id))
        .await
        .to_internal_error()?;
    Ok(value.and_then(|value| serde_json::from_str(&value).ok()))
}

/// Forget a preview (the share stopped)
pub async fn delete_stream_preview(channel_id: &str, user_id: &str) -> Result<()> {
    get_connection()
        .await?
        .del::<_, ()>(vec![
            preview_key(channel_id, user_id),
            cooldown_key(channel_id, user_id),
        ])
        .await
        .to_internal_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_small_jpeg_or_webp() {
        let jpeg = STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]);
        assert_eq!(check_stream_preview(&jpeg).unwrap(), "image/jpeg");

        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.extend([0; 8]);
        assert_eq!(check_stream_preview(&STANDARD.encode(&webp)).unwrap(), "image/webp");

        let png = STANDARD.encode([0x89, b'P', b'N', b'G', 0x0D, 0x0A]);
        assert!(check_stream_preview(&png).is_err());
        assert!(check_stream_preview("not base64 at all!").is_err());

        let mut big = vec![0xFF, 0xD8, 0xFF];
        big.resize(STREAM_PREVIEW_MAX_BYTES + 1, 0);
        assert!(check_stream_preview(&STANDARD.encode(&big)).is_err());

        let mut fits = vec![0xFF, 0xD8, 0xFF];
        fits.resize(STREAM_PREVIEW_MAX_BYTES, 0);
        assert!(check_stream_preview(&STANDARD.encode(&fits)).is_ok());
    }
}
