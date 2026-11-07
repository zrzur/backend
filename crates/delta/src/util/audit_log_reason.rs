use revolt_result::{Error, create_error};
use revolt_rocket_okapi::{
    OpenApiError,
    r#gen::OpenApiGenerator,
    request::{OpenApiFromRequest, RequestHeaderInput},
    revolt_okapi::openapi3::{Parameter, ParameterValue},
};
use rocket::{
    http::Status,
    request::{FromRequest, Outcome, Request},
};
use schemars::schema::{InstanceType, SchemaObject, SingleOrVec};

/// Newtype for an audit log reason.
///
/// Extracts the reason from the `X-Audit-Log-Reason` header if provided.
/// The value may be URL-encoded (needed for anything outside Latin-1, such
/// as Arabic); it is decoded and limited to 512 characters.
pub struct AuditLogReason(pub Option<String>);

#[async_trait]
impl<'r> FromRequest<'r> for AuditLogReason {
    type Error = Error;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let reason = req.headers().get_one("x-audit-log-reason").map(decode);

        if reason
            .as_ref()
            .is_some_and(|str| str.chars().count() > 512)
        {
            return Outcome::Error((Status::BadRequest, create_error!(HeaderTooLarge)));
        };

        Outcome::Success(Self(reason))
    }
}

impl OpenApiFromRequest<'_> for AuditLogReason {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> Result<RequestHeaderInput, OpenApiError> {
        Ok(RequestHeaderInput::Parameter(Parameter {
            name: "X-Audit-Log-Reason".to_string(),
            description: Some("Reason for action which is stored in the audit log.".to_string()),
            allow_empty_value: false,
            required: false,
            deprecated: false,
            extensions: schemars::Map::new(),
            location: "header".to_string(),
            value: ParameterValue::Schema {
                allow_reserved: false,
                example: None,
                examples: None,
                explode: None,
                style: None,
                schema: SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
                    ..Default::default()
                },
            },
        }))
    }
}

impl From<AuditLogReason> for Option<String> {
    fn from(value: AuditLogReason) -> Self {
        value.0
    }
}

/// Percent-decode a header value; one that isn't valid UTF-8 after
/// decoding is kept as it came
fn decode(raw: &str) -> String {
    if !raw.contains('%') {
        return raw.to_string();
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(high << 4 | low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn reasons_may_be_url_encoded() {
        assert_eq!(decode("spam"), "spam");
        assert_eq!(decode("%D8%B3%D8%A8%D8%A7%D9%85"), "سبام");
        assert_eq!(decode("100%25 sure"), "100% sure");
        assert_eq!(decode("50% off"), "50% off");
        assert_eq!(decode("%FF"), "%FF");
        assert_eq!(decode("%ع"), "%ع");
    }
}
