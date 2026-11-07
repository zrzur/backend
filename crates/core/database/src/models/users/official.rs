//! Zrzur's official accounts: the senders of messages from the Zrzur team.
//!
//! - An account is official because it carries the mark (`User::official`),
//!   never because of its id. The mark changes only through the staff route
//!   with two-person approval (`/staff/official`); no public API writes it.
//! - The four accounts below are created and marked on every start of delta
//!   ([`seed_official_accounts`]). They have no login, so nobody can sign in
//!   as them. Zrzur keeps the id the one account always had, so its
//!   conversations and messages stay as they are.
//! - Conversations with an official account are read-only for the other
//!   person (see `calculate_channel_permissions`), and nobody can react to,
//!   answer, pin, report, befriend or block one. Support is answered in a
//!   ticket; the app points there instead of showing the message box.
//! - The names are reserved ([`passes_for_official`]).

use std::collections::BTreeMap;

use revolt_models::v0::MessageAuthor;
use revolt_result::Result;
use ulid::Ulid;

use crate::{Bot, Channel, Database, Message, PartialChannel, PartialUser, User, AMQP};

/// The accounts every Zrzur has from the first start
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfficialAccount {
    /// System notices, grants and account matters
    Zrzur,
    /// Support and tickets
    Support,
    /// Report and suspension decisions
    Safety,
    /// News
    Updates,
}

impl OfficialAccount {
    pub const ALL: [OfficialAccount; 4] = [
        OfficialAccount::Zrzur,
        OfficialAccount::Support,
        OfficialAccount::Safety,
        OfficialAccount::Updates,
    ];

    /// Fixed, so two servers starting at once create one account, not two.
    /// Only the seed uses these: everyone else reads the mark.
    pub fn id(self) -> &'static str {
        match self {
            OfficialAccount::Zrzur => "0000000000000000000000ZRZR",
            OfficialAccount::Support => "0000000000000000000000ZSPT",
            OfficialAccount::Safety => "0000000000000000000000ZSFT",
            OfficialAccount::Updates => "0000000000000000000000ZNWS",
        }
    }

    fn username(self) -> &'static str {
        match self {
            OfficialAccount::Zrzur => "Zrzur",
            OfficialAccount::Support => "ZrzurSupport",
            OfficialAccount::Safety => "ZrzurSafety",
            OfficialAccount::Updates => "ZrzurUpdates",
        }
    }

    /// `(ar, en, de)`
    fn names(self) -> (&'static str, &'static str, &'static str) {
        match self {
            OfficialAccount::Zrzur => ("زرزور", "Zrzur", "Zrzur"),
            OfficialAccount::Support => ("دعم زرزور", "Zrzur Support", "Zrzur-Support"),
            OfficialAccount::Safety => ("أمان زرزور", "Zrzur Safety", "Zrzur-Sicherheit"),
            OfficialAccount::Updates => ("تحديثات زرزور", "Zrzur Updates", "Zrzur-Neuigkeiten"),
        }
    }

    fn localised_names(self) -> BTreeMap<String, String> {
        let (ar, en, de) = self.names();
        BTreeMap::from([
            ("ar".to_string(), ar.to_string()),
            ("en".to_string(), en.to_string()),
            ("de".to_string(), de.to_string()),
        ])
    }
}

/// Whether this id is one of the accounts the server keeps on its own. Their
/// mark can't be removed: the next start would put it back.
pub fn is_seeded_official(user_id: &str) -> bool {
    OfficialAccount::ALL.iter().any(|account| account.id() == user_id)
}

/// Whether this user id carries the official mark. Message authors can be
/// webhooks or the system, which are no users: those are not official.
pub async fn is_official_id(db: &Database, user_id: &str) -> bool {
    db.fetch_user(user_id).await.is_ok_and(|user| user.official)
}

/// Whether someone in this conversation is an official account
pub async fn has_official_recipient(db: &Database, channel: &Channel) -> bool {
    if let Channel::DirectMessage { recipients, .. } = channel {
        for recipient in recipients {
            if is_official_id(db, recipient).await {
                return true;
            }
        }
    }
    false
}

/// Refuse an action on a message written by an official account
pub async fn assert_not_official_message(db: &Database, message: &Message) -> Result<()> {
    if is_official_id(db, &message.author).await {
        Err(create_error!(OfficialAccount))
    } else {
        Ok(())
    }
}

/// Names no one else may use. The retired spellings read the same.
const RESERVED_WORDS: &[&str] = &["zrzur", "زرزور", "zarzour", "zarzur", "zarzoor", "zrzoor", "زارزور"];

/// Letters people swap in to get past a filter: leetspeak and Arabic letters
/// that look the same, with their presentation forms
fn fold_lookalike(c: char) -> char {
    match c {
        '2' => 'z',
        'v' => 'u',
        '0' => 'o',
        'ژ' | 'ڙ' | 'ڒ' | '\u{FEAF}' | '\u{FEB0}' => 'ز',
        'ړ' | 'ڕ' | 'ڑ' | '\u{FEAD}' | '\u{FEAE}' => 'ر',
        'ۆ' | 'ۇ' | 'ۈ' | 'ۉ' | 'ۋ' | '\u{FEED}' | '\u{FEEE}' => 'و',
        c => c,
    }
}

/// Would this name pass for Zrzur? True for any username, display name,
/// nickname, bot, webhook or masquerade name that contains Zrzur or زرزور,
/// whatever is around it: lookalike letters, case, spaces, punctuation,
/// harakat, tatweel and doubled letters don't help.
///
/// The words Official, Support and Safety are refused only next to those two,
/// so they need no check of their own: "Support" alone is free and
/// "Zrzur Support" is already caught.
pub fn passes_for_official(name: &str) -> bool {
    let squash = |text: &str| -> String {
        crate::util::automod::normalise(text)
            .chars()
            .filter(|c| c.is_alphanumeric())
            .map(fold_lookalike)
            .collect()
    };
    let without_repeats = |text: &str| -> String {
        let mut letters: Vec<char> = text.chars().collect();
        letters.dedup();
        letters.into_iter().collect()
    };
    // lookalike Latin letters (Cyrillic г, full-width Ｚ, ...) become plain
    // ones; the cure changes Arabic, so the original is checked as well
    let cured = decancer::cure(name, decancer::Options::default())
        .map(|c| c.to_string())
        .unwrap_or_default();
    [squash(&cured), squash(name)].iter().any(|spelling| {
        let single = without_repeats(spelling);
        RESERVED_WORDS.iter().any(|word| spelling.contains(word) || single.contains(word))
    })
}

/// Bots Zrzur runs itself, with their display names: the only names the rule
/// lets through, and only through [`create_system_bot`], which nothing but
/// `zrzur-staff` on the server calls. No API takes this path, so a person
/// asking for one of these names is refused like for any other.
pub const SYSTEM_BOTS: &[(&str, &str)] = &[("zrzur-assistant", "زرزور المساعد")];

/// Whether this username is on the allowlist of [`SYSTEM_BOTS`]
pub fn is_system_bot_name(username: &str) -> bool {
    SYSTEM_BOTS.iter().any(|(name, _)| *name == username)
}

/// Create a system bot under its reserved name, with the official bot mark.
/// It is made under a plain name, so the one path that creates users never
/// has to know an exception, and then takes its reserved name.
pub async fn create_system_bot(db: &Database, username: &str, owner: &User) -> Result<(Bot, User)> {
    let Some((name, display_name)) = SYSTEM_BOTS.iter().find(|(name, _)| *name == username) else {
        return Err(create_error!(InvalidUsername));
    };
    // 16 random letters and digits: usernames stop at 32 characters
    let plain = Ulid::new().to_string().split_off(10).to_lowercase();
    let (bot, mut user) = Bot::create(db, format!("system{plain}"), owner, None).await?;
    let partial = PartialUser {
        username: Some(name.to_string()),
        discriminator: Some(User::find_discriminator(db, name, None).await?),
        display_name: Some(display_name.to_string()),
        official_bot: Some(true),
        ..Default::default()
    };
    db.update_user(&user.id, &partial, vec![]).await?;
    user.apply_options(partial);
    Ok((bot, user))
}

/// Give or remove the official bot mark of a bot on the allowlist
pub async fn set_official_bot(db: &Database, user_id: &str, mark: bool) -> Result<User> {
    let mut user = db.fetch_user(user_id).await?;
    if user.bot.is_none() {
        return Err(create_error!(IsNotBot));
    }
    if mark && !is_system_bot_name(&user.username) {
        return Err(create_error!(InvalidUsername));
    }
    let partial = PartialUser {
        official_bot: Some(mark),
        ..Default::default()
    };
    db.update_user(user_id, &partial, vec![]).await?;
    user.apply_options(partial);
    Ok(user)
}

/// One official account, created and marked if it isn't yet. Two servers can
/// do this at the same moment: the fixed id lets only one insert succeed, the
/// other finds that account, and setting the mark twice changes nothing.
pub async fn official_account(db: &Database, account: OfficialAccount) -> Result<User> {
    let id = account.id();
    let (_, english, _) = account.names();
    let names = account.localised_names();

    let mut user = match db.fetch_user(id).await {
        Ok(user) => user,
        Err(_) => {
            let mut inserted = None;
            // an old account may hold the name: the id is what counts
            for discriminator in ["0001", "0002", "0003", "0004", "0005"] {
                let user = User {
                    id: id.to_string(),
                    username: account.username().to_string(),
                    discriminator: discriminator.to_string(),
                    display_name: Some(english.to_string()),
                    official: true,
                    official_names: Some(names.clone()),
                    ..Default::default()
                };
                if db.insert_user(&user).await.is_ok() {
                    inserted = Some(user);
                    break;
                }
                // created at the same moment by another server
                if let Ok(user) = db.fetch_user(id).await {
                    inserted = Some(user);
                    break;
                }
            }
            match inserted {
                Some(user) => user,
                None => return db.fetch_user(id).await,
            }
        }
    };

    if !user.official || user.official_names.as_ref() != Some(&names) {
        let partial = PartialUser {
            official: Some(true),
            official_names: Some(names),
            ..Default::default()
        };
        db.update_user(id, &partial, vec![]).await?;
        user.apply_options(partial);
    }
    Ok(user)
}

/// Create and mark the official accounts. Runs on every start of delta and
/// does nothing when all is in place. A failure is logged, not fatal: the
/// account is made on first use as well.
pub async fn seed_official_accounts(db: &Database) {
    for account in OfficialAccount::ALL {
        if let Err(error) = official_account(db, account).await {
            log::error!("official account {account:?} not ready: {:?}", error.error_type);
        }
    }
}

/// Send a message from an official account to one person's private
/// messages. It arrives like any new message (unread, push notification).
pub async fn send_official_message(
    db: &Database,
    amqp: Option<&AMQP>,
    from: OfficialAccount,
    user_id: &str,
    content: String,
) -> Result<Message> {
    let user = db.fetch_user(user_id).await?;
    if user.official {
        return Err(create_error!(OfficialAccount));
    }
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let official = official_account(db, from).await?;
    let mut channel = Channel::create_dm(db, &official, &user).await?;
    // a conversation the person closed opens again
    if let Channel::DirectMessage { active: false, .. } = &channel {
        channel
            .update(
                db,
                PartialChannel {
                    active: Some(true),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }

    let author = official.clone().into(db, None).await;
    let mut message = Message {
        id: Ulid::new().to_string(),
        channel: channel.id().to_string(),
        author: official.id.clone(),
        content: Some(content.chars().take(2000).collect()),
        ..Default::default()
    };
    message
        .send(
            db,
            amqp,
            MessageAuthor::User(&author),
            Some(author.clone()),
            None,
            &channel,
            false,
        )
        .await?;
    Ok(message)
}

/// The same text in Arabic, English and German, one after another (the
/// server doesn't know which language the person reads)
pub fn trilingual(ar: &str, en: &str, de: &str) -> String {
    format!("{en}\n\n{ar}\n\n{de}")
}
