//! Upstream `POST /safety/report`, kept for older clients. It now files a
//! Trust & Safety case through the same checks as `POST /safety/reports`
//! (the old route snapshotted any message id without checking that the
//! reporter could read it).

use revolt_database::{Database, User};
use revolt_models::v0::{
    safety_needs_description, ContentReportReason, DataCreateSafetyReport, DataSafetyTarget,
    ReportedContent, SafetyCategory, SafetyTargetKind, UserReportReason,
};
use revolt_result::Result;
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use serde::Deserialize;
use validator::Validate;

/// # Report Data
#[derive(Validate, Deserialize, JsonSchema)]
pub struct DataReportContent {
    /// Content being reported
    content: ReportedContent,
    /// Additional report description
    #[validate(length(min = 0, max = 1000))]
    #[serde(default)]
    additional_context: String,
}

fn content_reason(reason: &ContentReportReason) -> (SafetyCategory, Option<&'static str>) {
    use ContentReportReason as R;
    use SafetyCategory as C;
    match reason {
        R::NoneSpecified => (C::Other, None),
        R::Illegal => (C::IllegalContent, None),
        R::IllegalGoods => (C::IllegalContent, Some("drugs_goods")),
        R::IllegalExtortion => (C::IllegalContent, Some("extortion")),
        R::IllegalPornography => (C::SexualContent, Some("non_consensual")),
        R::IllegalHacking => (C::IllegalContent, Some("hacking")),
        R::ExtremeViolence => (C::IllegalContent, Some("violent_gore")),
        R::PromotesHarm => (C::HateOrThreats, Some("violent_threat")),
        R::UnsolicitedSpam => (C::Spam, Some("unsolicited_ads")),
        R::Raid => (C::Harassment, Some("raid")),
        R::SpamAbuse => (C::Spam, None),
        R::ScamsFraud => (C::ScamFraud, None),
        R::Malware => (C::Malware, None),
        R::Harassment => (C::Harassment, None),
    }
}

fn user_reason(reason: &UserReportReason) -> (SafetyCategory, Option<&'static str>, &'static str) {
    use SafetyCategory as C;
    use UserReportReason as R;
    match reason {
        R::NoneSpecified => (C::Other, None, "Other"),
        R::UnsolicitedSpam => (C::Spam, Some("unsolicited_ads"), "Unsolicited spam"),
        R::SpamAbuse => (C::Spam, None, "Spam"),
        R::InappropriateProfile => (C::Other, None, "Inappropriate profile"),
        R::Impersonation => (C::Impersonation, None, "Impersonation"),
        R::BanEvasion => (C::Other, None, "Ban evasion"),
        R::Underage => (C::Underage, None, "Underage"),
    }
}

/// # Report Content
///
/// Report a piece of content to the moderation team. Prefer
/// `POST /safety/reports`, which returns a receipt.
#[openapi(tag = "User Safety")]
#[post("/report", data = "<data>")]
pub async fn report_content(
    db: &State<Database>,
    user: User,
    data: Json<DataReportContent>,
) -> Result<EmptyResponse> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let (target, category, subcategory, label) = match &data.content {
        ReportedContent::Message { id, report_reason } => {
            let (category, sub) = content_reason(report_reason);
            (
                DataSafetyTarget {
                    kind: SafetyTargetKind::Message,
                    id: id.clone(),
                    facet: None,
                    message_id: None,
                    server_id: None,
                },
                category,
                sub,
                "Reported message",
            )
        }
        ReportedContent::Server { id, report_reason } => {
            let (category, sub) = content_reason(report_reason);
            (
                DataSafetyTarget {
                    kind: SafetyTargetKind::Server,
                    id: id.clone(),
                    facet: None,
                    message_id: None,
                    server_id: None,
                },
                category,
                sub,
                "Reported server",
            )
        }
        ReportedContent::User {
            id,
            report_reason,
            message_id,
        } => {
            let (category, sub, label) = user_reason(report_reason);
            (
                DataSafetyTarget {
                    kind: SafetyTargetKind::User,
                    id: id.clone(),
                    facet: None,
                    message_id: message_id.clone(),
                    server_id: None,
                },
                category,
                sub,
                label,
            )
        }
    };

    // old clients send no description for reasons that now need one
    let mut description = data.additional_context.trim().to_string();
    if safety_needs_description(category) && description.chars().count() < 10 {
        description = format!("{label}. {description}").trim().to_string();
        while description.chars().count() < 10 {
            description.push('.');
        }
    }

    super::reports::file_report(
        db,
        &user,
        DataCreateSafetyReport {
            target,
            category,
            subcategory: subcategory.map(str::to_string),
            description: Some(description),
            evidence_message_ids: vec![],
        },
    )
    .await?;

    Ok(EmptyResponse)
}
