//! Shared Discover helpers: listing validation, owner actions and the
//! public card shapes used by both the app and the standalone website.

use revolt_database::{
    Bot, Database, DiscoverRequest, DiscoverRequestStatus, DiscoverRequestType, PartialBot,
    PartialServer, Server, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};

/// Upper bound of listings considered per query. Filtering and popularity
/// sorting happen in memory, which is fine while Discover stays curated.
pub const MAX_SCAN: i64 = 1000;

/// Validate and normalise owner-provided listing details onto a request
pub fn apply_listing(
    request: &mut DiscoverRequest,
    data: v0::DataDiscoverListing,
) -> Result<()> {
    let categories = match request.request_type {
        DiscoverRequestType::Server => v0::DISCOVER_SERVER_CATEGORIES,
        DiscoverRequestType::Bot => v0::DISCOVER_BOT_CATEGORIES,
    };

    if let Some(tags) = data.tags {
        let mut clean: Vec<String> = Vec::new();
        for tag in tags {
            let tag = tag.trim().trim_start_matches('#').to_lowercase();
            if tag.is_empty() {
                continue;
            }

            let length = tag.chars().count();
            if !(2..=24).contains(&length)
                || !tag
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Err(create_error!(FailedValidation {
                    error: format!("invalid tag: {tag}")
                }));
            }

            if !clean.contains(&tag) {
                clean.push(tag);
            }
        }

        if clean.len() > v0::DISCOVER_MAX_TAGS {
            return Err(create_error!(FailedValidation {
                error: format!("at most {} tags", v0::DISCOVER_MAX_TAGS)
            }));
        }

        request.tags = clean;
    }

    if let Some(category) = data.category {
        let category = category.trim().to_lowercase();
        request.category = if category.is_empty() {
            None
        } else if categories.contains(&category.as_str()) {
            Some(category)
        } else {
            return Err(create_error!(FailedValidation {
                error: format!("unknown category: {category}")
            }));
        };
    }

    if let Some(language) = data.language {
        let language = language.trim().to_lowercase();
        request.language = if language.is_empty() {
            None
        } else if language.len() == 2 && language.chars().all(|c| c.is_ascii_lowercase()) {
            Some(language)
        } else {
            return Err(create_error!(FailedValidation {
                error: format!("invalid language: {language}")
            }));
        };
    }

    Ok(())
}

/// Submit (or resubmit after denial) an item, saving its listing details
pub async fn submit(
    db: &Database,
    request_type: DiscoverRequestType,
    id: &str,
    data: Option<v0::DataDiscoverListing>,
) -> Result<v0::DiscoverRequest> {
    if db
        .get_discover_ban(request_type.clone(), id)
        .await
        .is_ok()
    {
        return Err(create_error!(Banned));
    }

    let mut request = db.insert_discover_request(request_type, id).await?;
    if let Some(data) = data {
        apply_listing(&mut request, data)?;
        db.save_discover_request(&request).await?;
    }

    Ok(request.into())
}

/// Edit listing details of a pending, reviewed, approved or denied request
pub async fn edit(
    db: &Database,
    request_type: DiscoverRequestType,
    id: &str,
    data: v0::DataDiscoverListing,
) -> Result<v0::DiscoverRequest> {
    let mut request = db
        .fetch_discover_request_by_item_id(request_type, id)
        .await?;

    if matches!(request.status, DiscoverRequestStatus::Removed(_)) {
        return Err(create_error!(ContactSupport {
            locale: "discover.removed_cannot_apply".to_string(),
            msg: "Your server/bot was removed from Discover. Please contact support for more information.".to_string()
        }));
    }

    apply_listing(&mut request, data)?;
    db.save_discover_request(&request).await?;
    Ok(request.into())
}

/// Withdraw a request or take a listing down. Owners may do this at any
/// time, except for items moderators removed.
pub async fn withdraw(db: &Database, request_type: DiscoverRequestType, id: &str) -> Result<()> {
    let request = db
        .fetch_discover_request_by_item_id(request_type.clone(), id)
        .await?;

    if let DiscoverRequestStatus::Removed(_) = request.status {
        return Err(create_error!(ContactSupport {
            locale: "discover.server_removal_removed".to_string(),
            msg: "Your server has been removed from discover, contact support for more information"
                .to_string()
        }));
    }

    set_discoverable(db, request_type.clone(), id, false).await?;
    db.delete_discover_request(request_type, id).await
}

/// List or unlist the underlying server/bot
pub async fn set_discoverable(
    db: &Database,
    request_type: DiscoverRequestType,
    id: &str,
    discoverable: bool,
) -> Result<()> {
    match request_type {
        DiscoverRequestType::Server => {
            let mut server = db.fetch_server(id).await?;
            // only servers anyone can join are listed (approval and locked
            // servers stay off Discover, also through an approved request)
            if discoverable
                && server.access.unwrap_or_default() != revolt_models::v0::ServerAccess::Open
            {
                return Err(create_error!(InvalidOperation));
            }
            if server.discoverable != discoverable {
                server
                    .update(
                        db,
                        PartialServer {
                            discoverable: Some(discoverable),
                            ..Default::default()
                        },
                        vec![],
                    )
                    .await?;
            }
        }
        DiscoverRequestType::Bot => {
            let mut bot = db.fetch_bot(id).await?;
            if discoverable && !bot.public {
                return Err(create_error!(InvalidOperation));
            }

            if bot.discoverable != discoverable {
                bot.update(
                    db,
                    PartialBot {
                        discoverable: Some(discoverable),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;
            }
        }
    }

    Ok(())
}

/// Fill in the approximate online count of the cards about to be shown.
/// The counts are looked up in parallel; each is cached (also "no number"),
/// so a busy page does not walk every member list again.
pub async fn with_online(
    db: &Database,
    mut cards: Vec<v0::DiscoverServer>,
) -> Vec<v0::DiscoverServer> {
    let counts = futures::future::join_all(cards.iter().map(|card| async move {
        let server = db.fetch_server(&card.id).await.ok()?;
        server
            .get_approximate_online_count(db)
            .await
            .map(|count| count as i64)
    }))
    .await;
    for (card, count) in cards.iter_mut().zip(counts) {
        card.online_count = count;
    }
    cards
}

/// Public card for a listed server
pub async fn server_card(
    db: &Database,
    server: Server,
    request: Option<&DiscoverRequest>,
) -> v0::DiscoverServer {
    let member_count = server.get_approximate_member_count(db).await as i64;
    v0::DiscoverServer {
        id: server.id,
        name: server.name,
        description: server.description,
        icon: server.icon.map(Into::into),
        banner: server.banner.map(Into::into),
        flags: server.flags,
        nsfw: server.nsfw,
        member_count,
        online_count: None,
        link: server.vanity_invite.as_ref().map(|vanity| vanity.code.clone()),
        tags: request.map(|r| r.tags.clone()).unwrap_or_default(),
        category: request.and_then(|r| r.category.clone()),
        language: request.and_then(|r| r.language.clone()),
        featured: request.is_some_and(|r| r.featured),
        boost: None,
        promoted: false,
    }
}

/// Public card for a listed bot
pub async fn bot_card(
    db: &Database,
    bot: &Bot,
    user: User,
    request: Option<&DiscoverRequest>,
) -> v0::DiscoverBot {
    let server_count = db.fetch_server_count(&bot.id).await.unwrap_or(0) as i64;
    let profile = user.profile;
    v0::DiscoverBot {
        id: user.id,
        username: user.username,
        avatar: user.avatar.map(Into::into),
        banner: profile
            .as_ref()
            .and_then(|profile| profile.background.clone())
            .map(Into::into),
        description: profile
            .as_ref()
            .and_then(|profile| profile.public_content())
            .map(str::to_string),
        server_count,
        verified: bot.flags.unwrap_or_default() & v0::BotFlags::Verified as i32 != 0,
        tags: request.map(|r| r.tags.clone()).unwrap_or_default(),
        category: request.and_then(|r| r.category.clone()),
        language: request.and_then(|r| r.language.clone()),
        featured: request.is_some_and(|r| r.featured),
        boost: None,
        promoted: false,
    }
}

/// Filters shared by the server and bot listings
pub struct Filter {
    pub query: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub language: Option<String>,
    pub featured: Option<bool>,
}

impl From<&v0::OptionsDiscoverQuery> for Filter {
    fn from(options: &v0::OptionsDiscoverQuery) -> Self {
        let clean = |value: &Option<String>| {
            value
                .as_deref()
                .map(|v| v.trim().trim_start_matches('#').to_lowercase())
                .filter(|v| !v.is_empty())
        };

        Filter {
            query: clean(&options.query),
            category: clean(&options.category),
            tag: clean(&options.tag),
            language: clean(&options.language),
            featured: options.featured,
        }
    }
}

impl Filter {
    pub fn matches(
        &self,
        name: &str,
        description: Option<&str>,
        tags: &[String],
        category: Option<&str>,
        language: Option<&str>,
        featured: bool,
    ) -> bool {
        if let Some(query) = &self.query {
            let hit = name.to_lowercase().contains(query)
                || description.is_some_and(|d| d.to_lowercase().contains(query))
                || tags.iter().any(|t| t.contains(query));
            if !hit {
                return false;
            }
        }

        self.category.as_deref().is_none_or(|c| category == Some(c))
            && self.tag.as_ref().is_none_or(|t| tags.contains(t))
            && self.language.as_deref().is_none_or(|l| language == Some(l))
            && self.featured.is_none_or(|f| featured == f)
    }
}

/// Put boosted servers no lower than their guaranteed place.
///
/// `cards` arrive in organic order. At each place, a boosted server whose
/// guarantee runs out there goes first; otherwise the next server in
/// organic order does. So a boosted server lands at its organic place or
/// its guarantee, whichever is higher.
pub fn apply_boost_guarantees<T>(
    cards: Vec<T>,
    guarantee: impl Fn(&T) -> Option<usize>,
) -> Vec<T> {
    let mut pending: Vec<(usize, Option<usize>, T)> = cards
        .into_iter()
        .enumerate()
        .map(|(organic, card)| {
            let deadline = guarantee(&card).map(|place| place.saturating_sub(1));
            (organic, deadline, card)
        })
        .collect();

    let mut ranked = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let place = ranked.len();

        // A boosted server has to go now if waiting would leave too few
        // places for everyone due by some deadline (earliest deadline
        // first). With more boosted servers than places, they still fill
        // the places before anyone else does.
        let mut deadlines: Vec<usize> = pending
            .iter()
            .filter_map(|(_, deadline, _)| *deadline)
            .collect();
        deadlines.sort_unstable();
        let must_place = deadlines.iter().enumerate().any(|(index, deadline)| {
            // `index + 1` boosted servers are due by `deadline`
            let places_left = (*deadline + 1).saturating_sub(place);
            index + 1 >= places_left
        });

        let index = if must_place {
            pending
                .iter()
                .enumerate()
                .filter(|(_, (_, deadline, _))| deadline.is_some())
                .min_by_key(|(_, (organic, deadline, _))| (*deadline, *organic))
                .map(|(index, _)| index)
                .unwrap_or(0)
        } else {
            0
        };
        ranked.push(pending.remove(index).2);
    }
    ranked
}

/// Servers on Discover in listing order, with their boost and top-row
/// labels. Without options: everything, popular order (used for ranks).
///
/// While billing is available, servers below the member minimum only show
/// with a boost. Boosts and the top row never list a server moderators
/// haven't approved.
pub async fn ranked_servers(
    db: &Database,
    options: Option<&v0::OptionsDiscoverQuery>,
) -> Result<Vec<v0::DiscoverServer>> {
    let billing = revolt_config::config().await.features.billing;
    let promotions = revolt_database::active_promotions(db).await?;
    let min_members = if billing.available() {
        billing.discover.min_members as i64
    } else {
        0
    };

    let filter = options.map(Filter::from);
    let mut servers = db.fetch_discoverable_servers(None, MAX_SCAN, 0).await?;
    // Paid places are few (see the tier capacities); make sure they show
    // even when there are more listings than one scan reads
    for id in promotions.keys() {
        if !servers.iter().any(|server| &server.id == id) {
            if let Ok(server) = db.fetch_server(id).await {
                if server.is_listed() {
                    servers.push(server);
                }
            }
        }
    }
    let ids: Vec<String> = servers.iter().map(|s| s.id.clone()).collect();
    let requests = db
        .fetch_discover_requests_by_item_ids(DiscoverRequestType::Server, &ids)
        .await?;

    let mut cards = Vec::new();
    for server in servers {
        let request = requests.iter().find(|r| r.request_id == server.id);
        let mut card = server_card(db, server, request).await;
        if let Some(promotion) = promotions.get(&card.id) {
            card.boost = promotion.boost.as_ref().map(|(kind, _)| kind.clone());
            card.promoted = promotion.promoted.is_some();
        }
        if card.member_count < min_members && card.boost.is_none() {
            continue;
        }
        if filter.as_ref().is_none_or(|filter| {
            filter.matches(
                &card.name,
                card.description.as_deref(),
                &card.tags,
                card.category.as_deref(),
                card.language.as_deref(),
                card.featured,
            )
        }) {
            cards.push(card);
        }
    }

    // Servers arrive newest first; popular orders by member count and
    // keeps boosted servers at or above their guaranteed place
    if options.and_then(|options| options.sort.as_deref()) == Some("new") {
        return Ok(cards);
    }
    cards.sort_by(|a, b| b.member_count.cmp(&a.member_count));
    Ok(apply_boost_guarantees(cards, |card| {
        card.boost.as_deref().and_then(|kind| {
            billing
                .discover
                .boosts
                .iter()
                .find(|tier| tier.kind == kind)
                .map(|tier| tier.guarantee)
        })
    }))
}

/// Bots on Discover in listing order, with their boost and top-row labels.
/// Without options: everything, popular order (used for ranks).
pub async fn ranked_bots(
    db: &Database,
    options: Option<&v0::OptionsDiscoverQuery>,
) -> Result<Vec<v0::DiscoverBot>> {
    let billing = revolt_config::config().await.features.billing;
    let promotions = revolt_database::active_promotions(db).await?;

    let filter = options.map(Filter::from);
    let mut bots = db.fetch_discoverable_bots(MAX_SCAN).await?;
    for id in promotions.keys() {
        if !bots.iter().any(|bot| &bot.id == id) {
            if let Ok(bot) = db.fetch_bot(id).await {
                if bot.discoverable && bot.public {
                    bots.push(bot);
                }
            }
        }
    }
    let ids: Vec<String> = bots.iter().map(|bot| bot.id.clone()).collect();
    let users = db.fetch_users(&ids).await?;
    let requests = db
        .fetch_discover_requests_by_item_ids(DiscoverRequestType::Bot, &ids)
        .await?;

    let mut cards = Vec::new();
    for bot in &bots {
        let Some(user) = users.iter().find(|user| user.id == bot.id) else {
            continue;
        };
        let request = requests.iter().find(|r| r.request_id == bot.id);
        let mut card = bot_card(db, bot, user.clone(), request).await;
        if let Some(promotion) = promotions.get(&card.id) {
            card.boost = promotion.boost.as_ref().map(|(kind, _)| kind.clone());
            card.promoted = promotion.promoted.is_some();
        }
        if filter.as_ref().is_none_or(|filter| {
            filter.matches(
                &card.username,
                card.description.as_deref(),
                &card.tags,
                card.category.as_deref(),
                card.language.as_deref(),
                card.featured,
            )
        }) {
            cards.push(card);
        }
    }

    if options.and_then(|options| options.sort.as_deref()) == Some("new") {
        return Ok(cards);
    }
    cards.sort_by(|a, b| b.server_count.cmp(&a.server_count));
    Ok(apply_boost_guarantees(cards, |card| {
        card.boost.as_deref().and_then(|kind| {
            billing
                .discover
                .boosts
                .iter()
                .find(|tier| tier.kind == kind)
                .map(|tier| tier.guarantee)
        })
    }))
}

/// Add boost and top-row labels to one card
pub async fn label_card(db: &Database, mut card: v0::DiscoverServer) -> v0::DiscoverServer {
    if let Ok(promotions) = revolt_database::active_promotions(db).await {
        if let Some(promotion) = promotions.get(&card.id) {
            card.boost = promotion.boost.as_ref().map(|(kind, _)| kind.clone());
            card.promoted = promotion.promoted.is_some();
        }
    }
    card
}

#[cfg(test)]
mod ranking_test {
    use super::apply_boost_guarantees;
    use revolt_models::v0;

    fn card(id: &str) -> v0::DiscoverServer {
        v0::DiscoverServer {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            icon: None,
            banner: None,
            flags: None,
            nsfw: false,
            member_count: 0,
            online_count: None,
            link: None,
            tags: vec![],
            category: None,
            language: None,
            featured: false,
            boost: None,
            promoted: false,
        }
    }

    fn order(cards: Vec<v0::DiscoverServer>, guarantees: &[(&str, usize)]) -> Vec<String> {
        apply_boost_guarantees(cards, |card| {
            guarantees
                .iter()
                .find(|(id, _)| *id == card.id)
                .map(|(_, place)| *place)
        })
        .into_iter()
        .map(|card| card.id)
        .collect()
    }

    #[test]
    fn boosted_server_moves_up_to_its_guarantee() {
        let cards: Vec<_> = ["a", "b", "c", "d", "e"].iter().map(|id| card(id)).collect();
        assert_eq!(order(cards, &[("e", 2)]), vec!["a", "e", "b", "c", "d"]);
    }

    #[test]
    fn boosted_server_already_higher_keeps_its_place() {
        let cards: Vec<_> = ["a", "b", "c"].iter().map(|id| card(id)).collect();
        assert_eq!(order(cards, &[("a", 3)]), vec!["a", "b", "c"]);
    }

    #[test]
    fn tighter_guarantee_goes_first() {
        let cards: Vec<_> = ["a", "b", "c", "d"].iter().map(|id| card(id)).collect();
        assert_eq!(
            order(cards, &[("c", 2), ("d", 1)]),
            vec!["d", "c", "a", "b"]
        );
    }

    #[test]
    fn nobody_is_lost() {
        let cards: Vec<_> = (0..50).map(|n| card(&n.to_string())).collect();
        let guarantees: Vec<(String, usize)> =
            (30..50).map(|n| (n.to_string(), 5)).collect();
        let refs: Vec<(&str, usize)> = guarantees.iter().map(|(id, p)| (id.as_str(), *p)).collect();
        let out = order(cards, &refs);
        assert_eq!(out.len(), 50);
        // the five places are full of boosted servers
        assert!(out[..5].iter().all(|id| id.parse::<u32>().unwrap() >= 30));
    }
}
