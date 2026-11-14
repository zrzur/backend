use std::collections::HashMap;

use lazy_static::lazy_static;
use regex::Regex;
use revolt_models::v0::{
    BandcampType, Image, ImageSize, LightspeedType, Special, TwitchType, Video, WebsiteMetadata,
};
use scraper::{Html, Selector};
use url::Url;

/// Convert all URLs to absolute form
fn url_to_absolute(page_url: &str, url: String) -> String {
    if url.starts_with("http") && url.contains("://") {
        //External
        return url;
    } else if url.starts_with('/') {
        //Absolute
        let page = Url::parse(page_url);
        if page.is_err() {
            return "".to_string();
        }
        return format!("{}{}", &page.unwrap().origin().unicode_serialization(), url);
    }
    //Relative
    format!("{}/{}", &page_url.trim_end_matches('/'), url)
}

/// Create website metadata from URL and document
pub async fn create_website_embed(original_url: &str, document: &str) -> Option<WebsiteMetadata> {
    let (mut meta, mut link) = {
        let document = Html::parse_document(document);

        // create selectors
        let meta_selector = Selector::parse("meta").ok()?;
        let link_selector = Selector::parse("link").ok()?;

        // extract meta tags
        let mut meta = HashMap::new();
        for el in document.select(&meta_selector) {
            let node = el.value();

            if let (Some(property), Some(content)) = (
                node.attr("property").or_else(|| node.attr("name")),
                node.attr("content"),
            ) {
                meta.entry(property.to_string())
                    .or_insert(content.to_string());
            }
        }

        // extract rel links
        let mut link = HashMap::new();
        for el in document.select(&link_selector) {
            let node = el.value();

            if let (Some(property), Some(content)) = (node.attr("rel"), node.attr("href")) {
                link.insert(property.to_string(), content.to_string());
            }
        }

        (meta, link)
    };

    // build metadata
    let mut metadata = WebsiteMetadata {
        title: meta
            .remove("og:title")
            .or_else(|| meta.remove("twitter:title"))
            .or_else(|| meta.remove("title"))
            .map(|s| s.trim().to_owned()),
        description: meta
            .remove("og:description")
            .or_else(|| meta.remove("twitter:description"))
            .or_else(|| meta.remove("description"))
            .map(|s| s.trim().to_owned()),
        image: meta
            .remove("og:image")
            .or_else(|| meta.remove("og:image:secure_url"))
            .or_else(|| meta.remove("twitter:image"))
            .or_else(|| meta.remove("twitter:image:src"))
            .map(|s| s.trim().to_owned())
            .map(|url| {
                let mut size = ImageSize::Preview;
                if let Some(card) = meta.remove("twitter:card") {
                    if &card == "summary_large_image" {
                        size = ImageSize::Large;
                    }
                }

                Image {
                    url: url_to_absolute(original_url, url),
                    width: meta
                        .remove("og:image:width")
                        .unwrap_or_default()
                        .parse()
                        .unwrap_or(0),
                    height: meta
                        .remove("og:image:height")
                        .unwrap_or_default()
                        .parse()
                        .unwrap_or(0),
                    size,
                }
            }),
        video: meta
            .remove("og:video")
            .or_else(|| meta.remove("og:video:url"))
            .or_else(|| meta.remove("og:video:secure_url"))
            .map(|s| s.trim().to_owned())
            .map(|url| Video {
                url: url_to_absolute(original_url, url),
                width: meta
                    .remove("og:video:width")
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(0),
                height: meta
                    .remove("og:video:height")
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(0),
            }),
        icon_url: link
            .remove("apple-touch-icon")
            .or_else(|| link.remove("icon"))
            .map(|s| s.trim().to_owned())
            .map(|url| url_to_absolute(original_url, url)),
        colour: meta.remove("theme-color").map(|s| s.trim().to_owned()),
        site_name: meta.remove("og:site_name").map(|s| s.trim().to_owned()),
        url: meta
            .remove("og:url")
            .or_else(|| Some(original_url.to_owned())),
        original_url: Some(original_url.to_owned()),
        special: None,
    };

    // populate extra metadata for popular websites
    populate_special(original_url.to_owned(), &mut metadata).await;

    // fetch video size if missing
    if metadata.special.is_none() {
        if let Some(Video { width, height, url }) = &metadata.video {
            if url.is_empty() {
                metadata.video.take();
            } else if width == &0 || height == &0 {
                metadata.video =
                    match crate::requests::Request::fetch_video_metadata(url, None).await {
                        Ok(Some(video)) => Some(video),
                        _ => None,
                    }
            }
        }
    }

    // remove image if video exists
    if metadata.video.is_some() {
        metadata.image.take();
    }

    // fetch image size if missing
    if metadata.special.is_none() {
        if let Some(Image {
            width, height, url, ..
        }) = &metadata.image
        {
            if url.is_empty() {
                metadata.image.take();
            } else if width == &0 || height == &0 {
                let size = metadata.image.as_ref().unwrap().size.clone();
                metadata.image =
                    match crate::requests::Request::fetch_image_metadata(url, None, size).await {
                        Ok(Some(image)) => Some(image),
                        _ => None,
                    }
            }
        }
    }

    // truncate data
    metadata.truncate();

    // if it's empty, don't return anything
    if metadata.is_empty() {
        None
    } else {
        Some(metadata)
    }
}

/// Sites whose own player is shown in place of a link card: the player
/// address is built here from the link, only for these hosts and only from
/// the parts each pattern takes (ids, never free text).
pub fn site_player(url: &str) -> Option<Special> {
    lazy_static! {
        static ref RE_VIMEO: Regex =
            Regex::new(r"^(?:https?://)?(?:www\.)?vimeo\.com/(\d+)(?:[/?#]|$)").unwrap();
        static ref RE_DAILYMOTION: Regex = Regex::new(
            r"^(?:https?://)?(?:(?:www\.)?dailymotion\.com/video|dai\.ly)/([A-Za-z0-9]+)(?:[/?#_]|$)"
        )
        .unwrap();
        static ref RE_TIKTOK: Regex =
            Regex::new(r"^(?:https?://)?(?:www\.)?tiktok\.com/@[\w.-]+/video/(\d+)(?:[/?#]|$)")
                .unwrap();
        static ref RE_INSTAGRAM: Regex = Regex::new(
            r"^(?:https?://)?(?:www\.)?instagram\.com/(p|reel|tv)/([A-Za-z0-9_-]+)(?:[/?#]|$)"
        )
        .unwrap();
        static ref RE_DEEZER: Regex = Regex::new(
            r"^(?:https?://)?(?:www\.)?deezer\.com/(?:[a-z]{2}/)?(track|album|playlist|artist|show|episode)/(\d+)(?:[/?#]|$)"
        )
        .unwrap();
        static ref RE_APPLE_PODCASTS: Regex = Regex::new(
            r"^(?:https?://)?podcasts\.apple\.com/((?:[a-z]{2}/)?podcast/[A-Za-z0-9%_-]+/id\d+)(?:\?i=(\d+))?"
        )
        .unwrap();
        static ref RE_KICK: Regex =
            Regex::new(r"^(?:https?://)?(?:www\.)?kick\.com/([A-Za-z0-9_-]{2,32})/?(?:[?#]|$)")
                .unwrap();
        static ref RE_MIXCLOUD: Regex = Regex::new(
            r"^(?:https?://)?(?:www\.)?mixcloud\.com/([A-Za-z0-9_-]+)/([A-Za-z0-9_-]+)/?(?:[?#]|$)"
        )
        .unwrap();
        static ref RE_TIDAL: Regex = Regex::new(
            r"^(?:https?://)?(?:listen\.)?tidal\.com/(?:browse/)?(track|album|playlist|video)/([A-Za-z0-9-]+)(?:[/?#]|$)"
        )
        .unwrap();
    }

    let player = |provider: &str, embed_url: String, width: u32, height: u32| {
        Some(Special::Player {
            provider: provider.to_string(),
            embed_url,
            width,
            height,
        })
    };

    if let Some(c) = RE_VIMEO.captures(url) {
        return player(
            "Vimeo",
            format!("https://player.vimeo.com/video/{}", &c[1]),
            1280,
            720,
        );
    }
    if let Some(c) = RE_DAILYMOTION.captures(url) {
        return player(
            "Dailymotion",
            format!("https://www.dailymotion.com/embed/video/{}", &c[1]),
            1280,
            720,
        );
    }
    if let Some(c) = RE_TIKTOK.captures(url) {
        return player(
            "TikTok",
            format!("https://www.tiktok.com/embed/v2/{}", &c[1]),
            325,
            740,
        );
    }
    if let Some(c) = RE_INSTAGRAM.captures(url) {
        let kind = if &c[1] == "reel" { "reel" } else { "p" };
        return player(
            "Instagram",
            format!("https://www.instagram.com/{kind}/{}/embed", &c[2]),
            400,
            560,
        );
    }
    if let Some(c) = RE_DEEZER.captures(url) {
        let height = if &c[1] == "track" || &c[1] == "episode" {
            152
        } else {
            380
        };
        return player(
            "Deezer",
            format!("https://widget.deezer.com/widget/dark/{}/{}", &c[1], &c[2]),
            420,
            height,
        );
    }
    if let Some(c) = RE_APPLE_PODCASTS.captures(url) {
        let (query, height) = match c.get(2) {
            Some(episode) => (format!("?i={}", episode.as_str()), 175),
            None => (String::new(), 450),
        };
        return player(
            "Apple Podcasts",
            format!("https://embed.podcasts.apple.com/{}{query}", &c[1]),
            450,
            height,
        );
    }
    if let Some(c) = RE_MIXCLOUD.captures(url) {
        return player(
            "Mixcloud",
            format!(
                "https://www.mixcloud.com/widget/iframe/?hide_cover=1&feed=%2F{}%2F{}%2F",
                &c[1], &c[2]
            ),
            480,
            120,
        );
    }
    if let Some(c) = RE_TIDAL.captures(url) {
        let (kind, height) = match &c[1] {
            "track" => ("tracks", 120),
            "album" => ("albums", 400),
            "playlist" => ("playlists", 400),
            _ => ("videos", 270),
        };
        return player(
            "Tidal",
            format!("https://embed.tidal.com/{kind}/{}", &c[2]),
            480,
            height,
        );
    }
    if let Some(c) = RE_KICK.captures(url) {
        // kick.com/<channel>, not kick.com/categories and the like
        const NOT_CHANNELS: &[&str] = &[
            "categories",
            "browse",
            "following",
            "search",
            "video",
            "videos",
            "clips",
            "terms-of-service",
            "privacy-policy",
            "dmca-policy",
            "community-guidelines",
        ];
        if !NOT_CHANNELS.contains(&c[1].to_lowercase().as_str()) {
            return player(
                "Kick",
                format!("https://player.kick.com/{}", &c[1]),
                1280,
                720,
            );
        }
    }
    None
}

#[cfg(test)]
mod site_player_tests {
    use super::site_player;
    use revolt_models::v0::Special;

    fn embed(url: &str) -> Option<(String, String)> {
        match site_player(url) {
            Some(Special::Player {
                provider,
                embed_url,
                ..
            }) => Some((provider, embed_url)),
            _ => None,
        }
    }

    #[test]
    fn builds_each_sites_player_from_the_link() {
        let cases = [
            (
                "https://vimeo.com/76979871",
                "Vimeo",
                "https://player.vimeo.com/video/76979871",
            ),
            (
                "https://www.dailymotion.com/video/x8abcd1",
                "Dailymotion",
                "https://www.dailymotion.com/embed/video/x8abcd1",
            ),
            (
                "https://dai.ly/x8abcd1",
                "Dailymotion",
                "https://www.dailymotion.com/embed/video/x8abcd1",
            ),
            (
                "https://www.tiktok.com/@some.user/video/7301234567890123456?lang=ar",
                "TikTok",
                "https://www.tiktok.com/embed/v2/7301234567890123456",
            ),
            (
                "https://www.instagram.com/p/CxYz_12-ab/",
                "Instagram",
                "https://www.instagram.com/p/CxYz_12-ab/embed",
            ),
            (
                "https://www.instagram.com/reel/CxYz12ab/?igsh=x",
                "Instagram",
                "https://www.instagram.com/reel/CxYz12ab/embed",
            ),
            (
                "https://www.deezer.com/de/playlist/1479458365",
                "Deezer",
                "https://widget.deezer.com/widget/dark/playlist/1479458365",
            ),
            (
                "https://podcasts.apple.com/us/podcast/some-show/id1200361736?i=1000600000000",
                "Apple Podcasts",
                "https://embed.podcasts.apple.com/us/podcast/some-show/id1200361736?i=1000600000000",
            ),
            (
                "https://www.mixcloud.com/someone/a-mix/",
                "Mixcloud",
                "https://www.mixcloud.com/widget/iframe/?hide_cover=1&feed=%2Fsomeone%2Fa-mix%2F",
            ),
            (
                "https://tidal.com/browse/track/12345678",
                "Tidal",
                "https://embed.tidal.com/tracks/12345678",
            ),
            (
                "https://kick.com/somechannel",
                "Kick",
                "https://player.kick.com/somechannel",
            ),
        ];
        for (url, provider, player) in cases {
            assert_eq!(
                embed(url),
                Some((provider.to_string(), player.to_string())),
                "{url}"
            );
        }
    }

    #[test]
    fn other_links_stay_link_cards() {
        for url in [
            "https://vimeo.com/channels/staffpicks",
            "https://kick.com/categories",
            "https://www.tiktok.com/@some.user",
            "https://www.instagram.com/someone/",
            "https://evil.example/vimeo.com/123",
            "https://vimeo.com.evil.example/123",
            "https://www.deezer.com/en/profile/123",
            "https://zrzur.com/blog",
        ] {
            assert_eq!(embed(url), None, "{url}");
        }
    }
}

pub async fn populate_special(original_url: String, metadata: &mut WebsiteMetadata) {
    lazy_static! {
        static ref RE_YOUTUBE: Regex = Regex::new("^(?:(?:https?:)?//)?(?:(?:www|m)\\.)?(?:(?:youtube\\.com|youtu.be))(?:/(?:[\\w\\-]+\\?v=|embed/|v/|shorts/)?)([\\w\\-]+)(?:\\S+)?$").unwrap();

        static ref RE_LIGHTSPEED: Regex = Regex::new("^(?:https?://)?(?:[\\w]+\\.)?lightspeed\\.tv/([a-z0-9_]{4,25})").unwrap();

        static ref RE_TWITCH: Regex = Regex::new("^(?:https?://)?(?:www\\.|go\\.)?twitch\\.tv/([a-z0-9_]+)($|\\?)").unwrap();
        static ref RE_TWITCH_VOD: Regex = Regex::new("^(?:https?://)?(?:www\\.|go\\.)?twitch\\.tv/videos/([0-9]+)($|\\?)").unwrap();
        static ref RE_TWITCH_CLIP: Regex = Regex::new("^(?:https?://)?(?:www\\.|go\\.)?twitch\\.tv/(?:[a-z0-9_]+)/clip/([A-z0-9_-]+)($|\\?)").unwrap();

        static ref RE_SPOTIFY: Regex = Regex::new("^(?:https?://)?open.spotify.com/(track|user|artist|album|playlist)/([A-z0-9]+)").unwrap();
        static ref RE_SOUNDCLOUD: Regex = Regex::new("^(?:https?://)?soundcloud.com/([a-zA-Z0-9-]+)/([A-z0-9-]+)").unwrap();
        static ref RE_BANDCAMP: Regex = Regex::new("^(?:https?://)?(?:[A-z0-9_-]+).bandcamp.com/(track|album)/([A-z0-9_-]+)").unwrap();
        static ref RE_APPLE_MUSIC: Regex = Regex::new("^(?:https?://)?music\\.apple\\.com/(?:[a-z]{2}/)?album/(?:[a-zA-Z0-9-]+)/(\\d+)(?:\\?i=(\\d+))?").unwrap();

        static ref RE_STREAMABLE: Regex = Regex::new("^(?:https?://)?(?:www\\.)?streamable\\.com/([\\w\\d-]+)").unwrap();

        static ref RE_GIF: Regex = Regex::new("^(?:https?://)?(www\\.)?(gifbox\\.me/view|yiffbox\\.me/view|tenor\\.com/view|giphy\\.com/gifs|klipy\\.com/gifs|gfycat\\.com|redgifs\\.com/watch)/[\\w\\d-]+").unwrap();
    }

    let url = metadata
        .url
        .as_ref()
        .or(metadata.original_url.as_ref())
        .unwrap_or(&original_url)
        .as_str();

    metadata.special = if let Some(player) = site_player(url) {
        Some(player)
    } else if let Some(captures) = RE_STREAMABLE.captures_iter(url).next() {
        Some(Special::Streamable {
            id: captures[1].to_string(),
        })
    } else if let Some(captures) = RE_LIGHTSPEED.captures_iter(url).next() {
        Some(Special::Lightspeed {
            id: captures[1].to_string(),
            content_type: LightspeedType::Channel,
        })
    } else if let Some(captures) = RE_TWITCH.captures_iter(url).next() {
        Some(Special::Twitch {
            id: captures[1].to_string(),
            content_type: TwitchType::Channel,
        })
    } else if let Some(captures) = RE_TWITCH_VOD.captures_iter(url).next() {
        Some(Special::Twitch {
            id: captures[1].to_string(),
            content_type: TwitchType::Video,
        })
    } else if let Some(captures) = RE_TWITCH_CLIP.captures_iter(url).next() {
        Some(Special::Twitch {
            id: captures[1].to_string(),
            content_type: TwitchType::Clip,
        })
    } else if let Some(captures) = RE_SPOTIFY.captures_iter(url).next() {
        Some(Special::Spotify {
            content_type: captures[1].to_string(),
            id: captures[2].to_string(),
        })
    } else if RE_SOUNDCLOUD.is_match(url) {
        Some(Special::Soundcloud)
    } else if RE_BANDCAMP.is_match(url) {
        lazy_static! {
            static ref RE_TRACK: Regex = Regex::new("track=(\\d+)").unwrap();
            static ref RE_ALBUM: Regex = Regex::new("album=(\\d+)").unwrap();
        }

        if let Some(video) = &metadata.video {
            if let Some(captures) = RE_TRACK.captures_iter(&video.url).next() {
                Some(Special::Bandcamp {
                    content_type: BandcampType::Track,
                    id: captures[1].to_string(),
                })
            } else {
                RE_ALBUM
                    .captures_iter(&video.url)
                    .next()
                    .map(|captures| Special::Bandcamp {
                        content_type: BandcampType::Album,
                        id: captures[1].to_string(),
                    })
            }
        } else {
            None
        }
    } else if RE_GIF.is_match(url) {
        Some(Special::GIF)
    } else {
        RE_APPLE_MUSIC
            .captures_iter(&original_url)
            .next()
            .map(|captures| Special::AppleMusic {
                album_id: captures[1].to_string(),
                track_id: captures.get(2).map(|m| m.as_str().to_string()),
            })
    };

    // add colours for popular websites
    if let Some(special) = &metadata.special {
        match special {
            Special::YouTube { .. } => metadata.colour = Some("#FF424F".to_string()),
            Special::Twitch { .. } => metadata.colour = Some("#7B68EE".to_string()),
            Special::Lightspeed { .. } => metadata.colour = Some("#7445D9".to_string()),
            Special::Spotify { .. } => metadata.colour = Some("#1ABC9C".to_string()),
            Special::Soundcloud => metadata.colour = Some("#FF7F50".to_string()),
            Special::AppleMusic { .. } => metadata.colour = Some("#FA233B".to_string()),
            _ => {}
        }
    }
}
