use std::{collections::HashSet, sync::LazyLock};

use lettre::{
    transport::smtp::{authentication::Credentials, client::Tls},
    SmtpTransport,
};
use regex::Regex;
use revolt_config::{config, ApiSmtp};
use revolt_result::Result;

static SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new("([^@]+)(@.+)").unwrap());
static SYMBOL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new("\\+.+|\\.").unwrap());
static HANDLEBARS: LazyLock<handlebars::Handlebars<'static>> =
    LazyLock::new(handlebars::Handlebars::new);
static REVOLT_SOURCE_LIST: LazyLock<HashSet<String>> = LazyLock::new(|| {
    include_str!("../../assets/revolt_source_list.txt")
        .split('\n')
        .map(|x| x.into())
        .collect()
});

/// Strip special characters and aliases from emails
pub fn normalise_email(original: String) -> String {
    let split = SPLIT.captures(&original).unwrap();
    let mut clean = SYMBOL_RE
        .replace_all(split.get(1).unwrap().as_str(), "")
        .to_string();

    clean.push_str(split.get(2).unwrap().as_str());
    clean.to_lowercase()
}

/// Email template
#[derive(Clone)]
pub struct Template {
    /// Title of the email
    pub title: String,
    /// Plain text version of this email
    pub text: String,
    /// HTML version of this email
    pub html: Option<String>,
    /// URL to redirect people to from the email
    ///
    /// Use `{{url}}` to fill this field.
    ///
    /// Any given URL will be suffixed with a unique token if applicable.
    ///
    /// e.g. `https://example.com?t=` becomes `https://example.com?t=UNIQUE_CODE`
    pub url: String,
}

/// Email templates
#[derive(Clone)]
pub struct Templates {
    /// Template for email verification
    pub verify: Template,
    /// Template for password reset
    pub reset: Template,
    /// Template for password reset when the account already exists on creation
    pub reset_existing: Template,
    /// Template for account deletion
    pub deletion: Template,
    /// Template for suspention
    pub suspension: Template,
    /// Template for confirming an email or two-factor change staff asked for
    pub account_change: Template,
    /// A notice from the Zrzur team to one person
    pub notice: Template,
    /// A message written in the reader's language (statement of reasons,
    /// receipts). The caller sets the subject, and fills `{{body}}` and
    /// `{{dir}}` (`ltr` or `rtl`).
    pub statement: Template,
}

pub async fn email_templates() -> Templates {
    let config = config().await;

    if std::env::var("TEST_DB").is_ok() {
        Templates {
            verify: Template {
                title: "verify".into(),
                text: "[[{{url}}]]".into(),
                url: "".into(),
                html: None,
            },
            reset: Template {
                title: "reset".into(),
                text: "[[{{url}}]]".into(),
                url: "".into(),
                html: None,
            },
            reset_existing: Template {
                title: "reset_existing".into(),
                text: "[[{{url}}]]".into(),
                url: "".into(),
                html: None,
            },
            deletion: Template {
                title: "deletion".into(),
                text: "[[{{url}}]]".into(),
                url: "".into(),
                html: None,
            },
            suspension: Template {
                title: "suspension".into(),
                text: "[[dummy]]".into(),
                url: "".into(),
                html: None,
            },
            account_change: Template {
                title: "account_change".into(),
                text: "[[{{url}}]]".into(),
                url: "".into(),
                html: None,
            },
            notice: Template {
                title: "notice".into(),
                text: "[[{{{title_en}}}]]".into(),
                url: "".into(),
                html: None,
            },
            statement: Template {
                title: "statement".into(),
                text: "[[{{{body}}}]]".into(),
                url: "".into(),
                html: None,
            },
        }
    } else if config.production {
        Templates {
            verify: Template {
                title: "Verify your Zrzur account.".into(),
                text: include_str!("../../templates/verify.txt").into(),
                url: format!("{}/login/verify/", config.hosts.app),
                html: Some(include_str!("../../templates/verify.html").into()),
            },
            reset: Template {
                title: "Reset your Zrzur password.".into(),
                text: include_str!("../../templates/reset.txt").into(),
                url: format!("{}/login/reset/", config.hosts.app),
                html: Some(include_str!("../../templates/reset.html").into()),
            },
            reset_existing: Template {
                title: "You already have a Zrzur account, reset your password.".into(),
                text: include_str!("../../templates/reset-existing.txt").into(),
                url: format!("{}/login/reset/", config.hosts.app),
                html: Some(include_str!("../../templates/reset-existing.html").into()),
            },
            deletion: Template {
                title: "Confirm account deletion.".into(),
                text: include_str!("../../templates/deletion.txt").into(),
                url: format!("{}/delete/", config.hosts.app),
                html: Some(include_str!("../../templates/deletion.html").into()),
            },
            suspension: Template {
                title: "Account Suspension".to_string(),
                html: Some(include_str!("../../templates/suspension.html").to_owned()),
                text: include_str!("../../templates/suspension.txt").to_owned(),
                url: Default::default(),
            },
            account_change: Template {
                title: "Confirm a change to your Zrzur account".into(),
                text: include_str!("../../templates/account-change.txt").into(),
                url: format!("{}/login/confirm-change/", config.hosts.app),
                html: None,
            },
            notice: Template {
                title: "A message from Zrzur".into(),
                text: include_str!("../../templates/notice.txt").into(),
                url: Default::default(),
                html: None,
            },
            statement: Template {
                title: "A message from Zrzur".into(),
                text: include_str!("../../templates/statement.txt").into(),
                url: Default::default(),
                html: Some(include_str!("../../templates/statement.html").into()),
            },
        }
    } else {
        Templates {
            verify: Template {
                title: "Verify your account.".into(),
                text: include_str!("../../templates/verify.whitelabel.txt").into(),
                url: format!("{}/login/verify/", config.hosts.app),
                html: None,
            },
            reset: Template {
                title: "Reset your password.".into(),
                text: include_str!("../../templates/reset.whitelabel.txt").into(),
                url: format!("{}/login/reset/", config.hosts.app),
                html: None,
            },
            reset_existing: Template {
                title: "Reset your password.".into(),
                text: include_str!("../../templates/reset.whitelabel.txt").into(),
                url: format!("{}/login/reset/", config.hosts.app),
                html: None,
            },
            deletion: Template {
                title: "Confirm account deletion.".into(),
                text: include_str!("../../templates/deletion.whitelabel.txt").into(),
                url: format!("{}/delete/", config.hosts.app),
                html: None,
            },
            suspension: Template {
                title: "Account Suspension".to_string(),
                text: include_str!("../../templates/suspension.whitelabel.txt").to_owned(),
                url: Default::default(),
                html: None,
            },
            account_change: Template {
                title: "Confirm a change to your account".into(),
                text: include_str!("../../templates/account-change.txt").into(),
                url: format!("{}/login/confirm-change/", config.hosts.app),
                html: None,
            },
            notice: Template {
                title: "A message from the team".into(),
                text: include_str!("../../templates/notice.txt").into(),
                url: Default::default(),
                html: None,
            },
            statement: Template {
                title: "A message from the team".into(),
                text: include_str!("../../templates/statement.txt").into(),
                url: Default::default(),
                html: None,
            },
        }
    }
}

/// Create SMTP transport
pub fn create_transport(smtp: &ApiSmtp) -> SmtpTransport {
    let relay = if smtp.use_starttls == Some(true) {
        SmtpTransport::starttls_relay(&smtp.host).unwrap()
    } else {
        SmtpTransport::relay(&smtp.host).unwrap()
    };

    let relay = if let Some(port) = smtp.port {
        relay.port(port.try_into().unwrap())
    } else {
        relay
    };

    let relay = if smtp.use_tls == Some(false) {
        relay.tls(Tls::None)
    } else {
        relay
    };

    relay
        .credentials(Credentials::new(
            smtp.username.clone(),
            smtp.password.clone(),
        ))
        .build()
}

/// Render an email template
fn render_template(text: &str, variables: &handlebars::JsonValue) -> Result<String> {
    HANDLEBARS
        .render_template(text, variables)
        .map_err(|_| create_error!(RenderFail))
}

/// Send an email
pub fn send_email(
    smtp: &ApiSmtp,
    address: String,
    template: &Template,
    variables: handlebars::JsonValue,
) -> Result<()> {
    let m = lettre::Message::builder()
        .from(smtp.from_address.parse().expect("valid `smtp_from`"))
        .to(address.parse().expect("valid `smtp_to`"))
        .subject(template.title.clone());

    let m = if let Some(reply_to) = &smtp.reply_to {
        m.reply_to(reply_to.parse().expect("valid `smtp_reply_to`"))
    } else {
        m
    };

    let text = render_template(&template.text, &variables).expect("valid `template`");

    let m = if let Some(html) = &template.html {
        m.multipart(lettre::message::MultiPart::alternative_plain_html(
            text,
            render_template(html, &variables).expect("valid `template`"),
        ))
    } else {
        // say it's UTF-8, or mail apps garble Arabic and other non-Latin text
        m.header(lettre::message::header::ContentType::TEXT_PLAIN)
            .body(text)
    }
    .expect("valid `message`");

    use lettre::Transport;
    let sender = create_transport(smtp);

    match sender.send(&m) {
        Ok(_) => Ok(()),
        Err(error) => {
            error!(
                "Failed to send email to {}!\nlettre error: {}",
                address, error
            );

            revolt_config::capture_error(&error);

            Err(create_error!(EmailFailed))
        }
    }
}

pub fn validate_email(email: &str) -> Result<()> {
    // Make sure this is an actual email
    if !validator::validate_email(email) {
        return Err(create_error!(IncorrectData {
            with: "email".to_string()
        }));
    }

    // Check if the email is blacklisted
    if let Some(domain) = email.split('@').next_back() {
        if REVOLT_SOURCE_LIST.contains(&domain.to_string()) {
            return Err(create_error!(Blacklisted));
        }
    }

    Ok(())
}

#[cfg(test)]
mod suspension_template_tests {
    /// The person cannot sign in while suspended: the mail has to give a way
    /// to appeal without the app, in every language.
    #[test]
    fn the_mail_works_for_someone_who_cannot_sign_in() {
        for template in [include_str!("../../templates/suspension.txt"), include_str!("../../templates/suspension.html")] {
            assert_eq!(template.matches("support@zrzur.com").count(), 3);
            for words in ["at least 6 months", "do not need to sign in", "mindestens 6 Monate", "ستة أشهر على الأقل"] {
                assert!(template.contains(words), "{words}");
            }
            assert!(!template.contains("Settings"));
        }
    }
}
