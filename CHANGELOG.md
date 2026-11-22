# Changelog

## Unreleased

### Rich presence (2026-11-22, behind `[features.rich_presence] enabled`, off by default)

- Users carry `activities` (`UserActivity`: source `Rpc` / `Desktop` / `Steam`, type `Playing` / `Listening` / `Watching` / `Streaming` / `Competing` / `Using`, name, details, state, start and end, pictures, up to two buttons), the main one first. Only for users shown online: never while offline, invisible, or to someone they blocked; a bot sees them only with the Presence intent. On `Ready`, user fetches, member and DM lists. `UserUpdate` carries the whole list (`[]` clears); going offline sends `[]` with `online: false`.
- New gateway message `SetActivity { source, activity }` from a person's own session (`Rpc` or `Desktop`; `Steam` is the server's). Every text is checked (lengths, one line), run through the platform word list; buttons are `https://` only; a picture is kept on the file server or proxy, any other `https://` picture is shown through the proxy. 5 changes in 20 seconds per person (Redis counter; `updates_per_window`, `window_seconds`). Refusals are `Error` events (`FeatureDisabled`, `IsBot`, `RateLimited`, `FailedValidation`, `ProfileTextFiltered`).
- Never stored in MongoDB: a Redis hash `activity:{user_id}` holds one entry per source. What a session set goes when its socket goes; the last session removes all (also after a crash, through `clear_region`); the hash expires after a day at the latest.
- Privacy: `UserPrivacy.share_activity` (unset: on for adults, off under 18; answers carry the value in force) and `share_steam_activity` (off until chosen). Turning sharing off removes what others see at once; while it is off `SetActivity` is ignored.
- Steam "now playing" (`steam_enabled`): delta reads `GetPlayerSummaries` (game id and name only) for people online, visible, sharing Steam and linked under the current consent text, 100 accounts a call, at most `steam_daily_calls` (20,000) a day, rounds every `steam_interval_seconds` (60) or slower to stay within it; one delta at a time (Redis lock). Pictures go through the proxy unless the game is in `no_picture_appids`.
- Going invisible or coming back sends the list again (empty to others while invisible).
- `GET /` gains `features.rich_presence` and `features.rich_presence_steam`.

## 26.3.0-beta.2 (2026-11-21)

### The owner's /badge command (2026-11-21)

- `[features.badge_command]` (`owner`, `bot`; empty = off) lets one account give and take any platform badge, the privileged ones included, through one bot's `/badge give` and `/badge remove`. The bot hands the stored interaction back to `POST /badges/command/grant` or `/revoke`; the server checks the bot, a single use, that the presser is the configured owner, the option kinds, and that the command is exactly the version the owner pressed and offers exactly the catalogue's badges. Every press is written to the platform audit log; the owner gets an official message for each change. `GET /badges/command/catalogue` gives the bot the list. Outside the public OpenAPI document.
- Command options and the command version are stored on an interaction only for that bot's `badge` command; a `/badge` press must say which command version it saw.

### Member channel overrides when someone leaves (2026-11-21)

- A member who leaves, is kicked or is banned loses their own entry in `member_permissions` on every channel of the server (a `ChannelUpdate` per channel changed). Before, the entry stayed: the app could not remove it and it let the person back into a private channel when they rejoined.
- `PUT /channels/{id}/permissions/members/{user_id}` takes an empty override (allow 0, deny 0) for a user who is no longer a member, so an entry left behind can always be removed. Any other override for a non-member is still `404 NotFound`. An empty override where nothing is stored returns the channel as it is: no write, no `ChannelUpdate`, no audit-log entry. On leave only the channels holding the person's entry are read.

### Friend-request pushes open the profile

- A web push for a friend request (received or accepted) carries `title`, `url`, `tag` and `icon` next to its `body`: the `url` is `/friends?profile=<user id>`, relative, so a click opens that person's profile instead of the app's front page. One notification per person (`tag: friend-<user id>`); the icon is their avatar. Apple pushes for the same two events carry the same link as a full `url` (`{hosts.app}/friends?profile=<user id>`, like message pushes); the web one stays relative. Old fields are unchanged.

### Bot platform: fixes from the audit of 2026-11-21

- A bot name may have one space between words ("Liro Bot", owner's report of 2026-11-21): on `POST /bots/create` and `PATCH /bots/{bot_id}` it becomes the bot's display name, and its username has `_` for the spaces (`Liro_Bot`), so mentions, uniqueness and the discriminator work as before. A name without a space is unchanged; a double space, a space at either end, a zero-width character or a new line is refused. Renaming to a name without a space drops the display name made from the old one.
- A public interaction answer the server refuses (the bot may not write in the channel, a message that is gone) no longer uses up the interaction: it goes back to the state it had, so the same interaction still takes a private answer. The same for `UpdateMessage`. A deferred answer stays as public as it was deferred: a refused finish through `PATCH /interactions/{id}/{token}/original` leaves it deferred, to be finished again.
- Bots of a suspended or banned owner stop: their token is refused (`401 InvalidSession`) on the API, the file service and the gateway, open connections close, and they are added to no server or group (`404 NotFound`). They work again when the owner's standing is restored. The owner's standing is kept 5 seconds (forgotten at once by a suspension or ban); a database fault answers `500 InternalError`, not `InvalidSession`.
- Deleting a bot takes it out of its group DMs, closes its connections at once (after the bot is gone, so a reconnect finds its token refused), and is no longer stopped by a membership left behind by a server that is gone.
- An install without `?permissions=` (`POST /bots/{id}/invite`, OAuth2) gives the bot its `default_permissions` through a role of its own, as an install from the Developer Portal does. The installer gives the part they hold, with Manage Roles; an explicit `?permissions=` keeps its strict check, and `?permissions=0` gives no role (the add dialog sends it with everything unticked). The OAuth2 preview (`GET /oauth2/authorize`) names the default permissions when the link has none, so the consent page shows what the bot gets. (Owner decision, 2026-11-21; second review.)
- A bot with a real token (one that may act: not disabled, owner in good standing) is counted by its id: its permission `403`s no longer count towards blocking its hosting address, and a blocked address no longer stops it. Its `401`s still count. The file service counts refusals like the API (a 429 no longer counts).
- A command sync (`PUT /applications/{id}/commands`, and per server) that changes nothing no longer sends `ApplicationCommandsUpdate` to every server.
- `PATCH /bots/{id}`: who may edit is checked first (someone else gets `404` whatever else is wrong), everything is validated before anything changes (a refused request no longer renames the bot), and `default_permissions` refuses unknown bits.
- The file service answers an unknown or reset bot token with `401 InvalidSession` (was `404 NotFound`).
- Public follow-ups sent at the same moment can no longer pass the limit of 5.
- Making a new webhook token and sending a test delivery have their own bucket (`webhook_manage`, 5 per 10 s per webhook); they shared the 100 of the posts through webhooks.
- A bot that leaves, is kicked or is banned loses its install record and command permissions, so the Apps list no longer shows it.
- OpenAPI: a `Bot Token` security scheme (`x-bot-token`); routes behind the user guard list a session token or a bot token; an MFA ticket is listed on top of signing in, no longer as an alternative to it. Where a route asks for the ticket only in some cases (`PATCH /bots/{bot_id}`, `PATCH` and `DELETE /servers/{target}`, password and email change, `PUT /auth/mfa/ticket`), signing in alone is listed too.
- The Discover bot routes answer `404 NotFound` for someone else's bot, like every other bot route (was `403 NotOwner`).

## 26.3.0-beta.1 (2026-11-21)

### Looking after team volunteers (2026-11-21)

- Once an application to the team is accepted, the person becomes a volunteer of that team (`TeamMember`): a first period (`trial_days`, 30 by default), then active, away or inactive, and left. Every step is written to the membership's history. Off unless `[features.team_applications.volunteers]` sets `enabled = true`; the settings are `trial_days`, `monthly_minimum`, `away_max_days`, `identity_keep_days`, `activity_keep_days`, `rules_version`, `team_server`, `report_channel`, and `work_channels` per role.
- New routes under `/team/desk`, called by the team bot from the stored interaction: `POST /about` (the "About you" step: the volunteer rules, name, age and country), `POST /identity`, `POST /status` ("My team status"), `POST /away` (away up to `away_max_days`), `POST /leave`, and `GET /report?week=` (the weekly report). Outside the public OpenAPI document.
- Counting: a daily crond job counts helpful actions per volunteer and day in the role's work channels; counts only, no message content and no channel names are stored. Nobody is counted before they have received the one-time counting notice (what is counted, why, who sees it, how long it is kept, the right to object, the privacy link). An objection is recorded by staff (`POST /staff/team/{id}/counting`) and stops the counting for that person until it is resumed. A month below `monthly_minimum` gets one kind reminder; a row of low months sets the volunteer inactive. A month with days not counted is not judged. Counts are kept `activity_keep_days`.
- Name, age and country are required to apply, are seen only by the owner and the reviewers, are kept while the person is on the team and `identity_keep_days` after they leave, then deleted. Showing them needs a reviewer's single-use press and is written to the platform audit log; every refusal is logged too.
- The weekly report goes to a private channel: per team the number of volunteers, and for teams of three or more the counts of trial, active, away and inactive volunteers and of contributions.
- New staff routes for the admin console (Team page): `GET /staff/team`, `GET /staff/team/{id}`, `POST /staff/team/{id}/identity`, `/confirm`, `/counting`, `/away`, `/note`, `/remove`. Each needs the `TeamManage` staff permission; every act is written to the platform audit log before it happens.
- Texts for volunteers in seven languages (en, ar, de, fr, es, ru, zh-Hans) in `crates/delta/assets/team/<language>.json`. The account data export carries the person's own volunteer records.
- One database migration: indexes for the volunteer collections (`PENDING_TEAM_VOLUNTEER_INDEXES`), safe to run twice.

## 26.2.0 (2026-11-20)

The official release for everyone. It brings the work of 26.2.0-beta.1 and 26.2.0-beta.2 below.

### Arabic first: search finds what was typed (2026-11-19)

- Message search (`POST /channels/{id}/search`, `query`) widens each word typed to the spellings the same word is stored under: forms of alef, a final ta marbuta or ha, a final alef maqsura or ya, Persian kaf and ya, Arabic-Indic, Persian and Latin digits, and the word with and without «ال» and the joined particles (و، ب، ل). A quoted phrase or a `-word` is passed on as typed. At most 240 terms per search. Vowel marks and tatweel in what was typed are dropped. A stored word written with a tatweel is still found only as written (it would need a folded copy of the text in the index).
- `GET /servers/{id}/members_experimental_query` compares the nickname, the display name and the username, folded; it used to be case-sensitive and skipped the display name.
- Discover listings (`query`, `tag`) compare folded text.
- New module `revolt_database::util::arabic_fold` (`fold_text`, `text_matches`, `fold_regex`, `search_terms`). No field, index or stored text changes; no data migration.
### When someone joins: roles for everyone and a welcome message (2026-11-19)

- New `GET` and `PUT /servers/{id}/on_join`: `roles` (up to five, given to everyone who joins) and `welcome` (`channel`, `content`). Stored on the server as `on_join`; not part of the server object members receive. No data migration.
- Both are applied in `Member::create_from`, where every join ends: an invite, Discover, an approved join request, `POST /auth/account/join`. Bots are left out. A role deleted since it was chosen is skipped.
- Roles need Manage Roles. A role at or above the caller's own rank can be neither added nor taken off (the owner is not limited); a bot's own role is refused; a role that can manage the server or act on members is refused unless `confirm_powerful_roles` is true.
- The welcome message needs Manage Server: a text channel of the server and 1 to 1000 characters. `{member}` becomes a mention and `{server}` the server's name. It is posted as a `text` system message without notifications, at most ten a minute per server; the rest of a rush join without one.
- A role given to everyone never lifts the server's verification level: only a role of the member's own does.

### Joining from an invite link (2026-11-19)

- New `[features] join_by_link_enabled` (off by default) and `join_confirm_hours` (72). Reported as `features.join_by_link` by `GET /`, and only with SMTP configured.
- New `POST /auth/account/join`: name, e-mail address, month and year of birth and an invite code make an account that is signed in and joined to the invite in one request. Every sign-up check applies (captcha, sign-ups per source, minimum age before anything is stored, blocked addresses) and the join runs the checks of `POST /invites/{code}`. An address that already has an account gets the usual mail and the answer `CheckEmail`.
- Such an account has no password and an unconfirmed e-mail. Until it confirms it cannot open direct messages or groups, send friend requests, join or create another server, create invites or bots, or send attachments (`UnverifiedAccount`). crond removes it after `join_confirm_hours`: entirely when it did nothing, like any deleted account (messages included) when it wrote.
- `POST /auth/account/verify/{code}` takes `{ "password" }` for these accounts and refuses without one; new `GET /auth/account/verify/{code}` says whether a password is needed. Other accounts confirm as before, with no body.
- `GET /invites/{code}` adds `server_description`, `invites_paused`, `custom_link` and, with the flag on, `inside`: up to four channels everyone in the server can see, by name. Fields are added; none changed.
- `GET /auth/account` adds `confirm_by` while the account is unconfirmed.
- The account document gains `provisional`. No data migration: accounts without it are untouched.
### File server: small previews for people who save data (2026-11-19)

- `GET /{tag}/{file_id}?size=small` on the file server answers a smaller WebP (quality 70): at most 480 px for attachments, 96 px for avatars and icons, 640 px for banners, 640x360 for backgrounds, 64 px for emojis, 192 px for decorations, 128x228 for effects. It is always one still frame: a GIF, an animated banner or an animated emoji answers with its first frame instead of a redirect to the whole file. Pictures are never enlarged.
- Any other value of `size`, or none, is the ordinary preview: apps that do not know the query are served as before.
- `GET /` of the file server gains `small_previews: true`, so an app can tell a server that makes them from one that would ignore the query.
- A deleted file's small preview is purged from the CDN with its other links.
- No setting, no data migration.
### Applications to join the team (2026-11-19)

- New routes under `/team/desk` (`GET /config`, `GET /queue`, `GET /applications/{id}`, `POST /mine`, `POST /apply`, `POST /withdraw`, `POST /act`, `POST /settle`), outside the public OpenAPI document and switched off unless `[features.team_applications]` names a server, an apply channel, a staff channel, a reviewer role and a bot. People apply for a team role through that bot's form; holders of the reviewer role press Interview, Accept or Reject on a card in the staff channel. The bot can decide nothing and gives nothing: who applied, who pressed, the answers and the reason are read from the stored interaction, and a decision is written to the platform audit log (`team.application.interview`, `.accept`, `.reject`) under the reviewer's name before it applies.
- Accepting gives the server role named for the team role, and the server gives it, not the bot; a person who is not in that server yet receives it once they join. Never given by any path: a role marked `manual`, the role with the key `administrator`, and any server role that can manage the server, its roles or its permissions (or assign roles). Accepting one of those only records it and tells the owner of the server. Platform staff powers are not touched.
- One open application per person; a new one after `cooldown_days` (30) from a decision or `withdraw_wait_minutes` (60) from a withdrawal. A rejection needs a written reason (3 to 512 characters), sent to the person. A reviewer cannot decide their own application.
- Privacy: no age is asked or kept, and anyone with an account may apply (owner, 2026-11-19); a role can still be marked `adult = true`, and only then is the age group the account already states read. Withdrawing erases the answers at once; a rejected application is deleted after `retention_days` (30, never less than the cooldown), an accepted one after `accepted_retention_days` (90), by a new crond task and on every read. Applications are part of the data export (`applications.json`, without reviewer ids) and are deleted with the account.
- Seven languages for the people who apply (en, ar, de, fr, es, ru, zh-Hans): the messages from the official account and the name, description, rules and standard questions of each kind of role live in `crates/delta/assets/team/<language>.json`; `GET /team/desk/config` gives the bot every role in every language. A role takes its texts from its kind (its key, or the part before a `-suffix`); `name`, `description` and `rules` in the settings are optional English overrides. The reviewers read English.
- Every text says Zrzur is a volunteer project and the role a volunteer role, never a job.
- After review: an acceptance counts only from a private confirmation whose words the server writes (`confirm` on each queue entry) and reads back unchanged, with one "Yes, accept" button; a mislabelled card button accepts nobody, and the reviewer is told by the official account whom they accepted. Roles that can manage channels or webhooks, or that get any such power from a channel's or category's own rules, are never handed out either. A suspended or banned account is not accepted and gets no pending role. The rejection reason is no longer copied into the platform audit log (only its length), and words from people (names, the reason) are made harmless in official messages and cannot fill another blank. Two forms sent at once leave one application. An acceptance is stored with its role still to give, so a stop of the server before the grant is finished by the next look.
- `[features.team_applications]` is boxed in the settings: some long test walks had no stack left for another wide field.
- The roles, their English descriptions, rules and up to two questions each are settings (`[[features.team_applications.roles]]`). A role can name the server's "Zrzur Team" role (`team_role`, given with it), its own staff channel (`channel_review`, also in the role's own server) and its own reviewers (`reviewer_role`, a role of that server); holders of the common `reviewer_role` decide everything. Every role is a volunteer role and the texts say so.
- No data migration: the new collection `team_applications` is created on first use.
### Discover review desk: lists, lookup and a log (2026-11-19)

- New `POST /discover/review/ask`: the desk's bot hands back the interaction of a command or a press, and the server answers only while the person behind it holds the reviewer role or the removal role. It gives the open queue, what is listed on Discover (five to a page), the facts of any server or bot with what the desk recorded about it, and counts per reviewer over 30 days.
- New `GET /discover/review/log?after=<seq>`, for the desk's bot only: the desk's own audit records (`discover.review.*`, successful ones) in order, for a log channel. Without `after` it tells where the log stands.
- A removal form may be sent from any channel of the review server; a decision still belongs to its staff channel.
- Queue entries carry `owner_display_name`.
- The messages an owner receives about a rejection or a removal are reworded in English, Arabic and German: they always name the reason, and a removal names the address to write to.
- Platform audit queries can filter by the start of the action name.
- No data migration.
### Voice: weak connection settings, clips and recorded rooms (2026-11-19, flags off)

- `features.voice_weak_network_enabled` and `[features.voice_weak_network]` (loss and round trip thresholds, voice bitrates for a weak and a very weak connection, recovery seconds), sent to the app in `GET /` as `features.voice_weak_network`.
- `features.voice_clips_enabled`, `features.voice_recordings_enabled` and `[features.voice_capture]` (consent `opt_in` by default, clip length and size, clips per month with and without Feather, a trial for servers without Wings, recording hours by Wings level, longest episode, retention days). `GET /` carries `voice_clips` and `voice_recordings`.
- Routes under `/channels/{target}/capture` (state and allowance, clips switch, consent, share a clip, start, stop and finish a recording, list, delete). Server voice rooms only. A clip or an episode that names someone who did not agree is refused (`VoiceNotAgreed`). New errors `TooManyVoiceClips` and `RecordingTimeUsedUp`.
- Event `VoiceCaptureUpdate` (the whole state of a room). voice-ingress forgets a person's choice when they leave and ends a recording whose recorder left. crond removes episodes past their retention.
- Collection `voice_captures` (no migration).

### Discover review desk: richer notices, a channel for bots, removal (2026-11-19)

- `GET /discover/review/queue` entries carry `icon` and `banner` (as `<bucket>/<file id>` on the file server) and `marks` (`Official`, `Verified`).
- `[features.discover_review]` gains `channel_bots` (a second staff channel for requests of bots; empty = one channel for both) and `remove_role`. A server is decided in the servers' channel only and a bot in the bots' one.
- Holders of `remove_role` can take a listed server or bot off Discover from the desk: a form with a written reason (3 to 512 characters), the platform audit record `discover.review.remove` names the person, the listing is dropped only while the request is still approved, and the owner is told the reason. With `remove_role` empty nobody can.
- New `GET /discover/review/find?q=` (a server id, a bot id or a server's link name), for the desk's bot only.
- A reviewer who is already a member of the server gets no visiting invite (joining again would fail): the desk answers `already_member` and nothing is recorded.
- No data migration.

### Discover review desk (2026-11-19)

- New routes under `/discover/review` (`GET /queue`, `GET /requests/{kind}/{id}`, `POST /act`), outside the public OpenAPI document and switched off unless `[features.discover_review]` names a server, a channel, a role and a bot. They let one bot show open Discover requests in one staff channel; people holding the role approve or reject there. The bot can decide nothing: an action is read from the interaction the server stored, the person is checked again, the platform audit record names the person, and of two decisions at once one applies.
- A rejection needs a written reason (3 to 512 characters). A reviewer cannot decide their own request. A visiting invite is one-use, 30 minutes, once per press, and only for a server open to everyone.
- The rejection notice tells the owner they can change what is needed and apply again (en, ar, de).
- No data migration: `submitted_at`, `acted_by`, `acted_at` on a Discover request and `custom_id`, `modal_fields`, `used` on a stored interaction are optional.
### Events: reminders, notices, share links, invitation links (26.1.0-beta.5, 2026-11-19)

Added to what was there; an app from before these changes keeps working.

- **Reminders.** `ServerEvent.reminders`: up to 3, in minutes before the start (5 minutes to a week), set on create and edit and kept by a copy. crond sends each one once to the people who are going or maybe going: the event notes a reminder before it is sent (`reminders_sent`), so two crond processes never send it twice; a reminder more than 10 minutes late is only noted. A new start time lets every reminder go out again.
- **Notices.** A push when an event's time or place changes (one per edit), when it is called off, when someone is invited (create, edit, publish of a draft; never the person who invited), and when a seat comes free for someone on the waiting list. One tag per event, so a newer notice replaces the older one. At most 5000 people per notice.
- **Switches.** `PUT /servers/{target}/events/{event}/notices` (`{ muted }`) switches one person's notices of one event off; `ServerEventRsvp.muted` says so. Kinds of notices are switched off for every event in the synced setting `event_notices` (`{"reminders":false}`; also `changes`, `invitations`, `seats`).
- **Share link.** `PUT …/link` (`name`, `public`) and `DELETE …/link`; `ServerEvent.link { code, name }` and `link_public`. The link is `<invite host>/e/<name>-<code>`; the code (8 letters, unique index) finds the event, the name only makes it readable. `GET /invites/event/{path}` (signed in or not): a member who sees the event gets the title, time, cover, server name and the ids; anyone else gets title, time, cover and server name, only when an organiser opened the link and never for a mature server; otherwise NotFound. Never a count or a person. Opening a link to outsiders needs the new flag `features.server_event_public_links` (off by default; `GET /` says `server_event_public_links`).
- **Invitation links.** For events that are for invited people: `POST …/invitations` (`max_uses` 1 to 200, `expires_in` 5 minutes to 30 days), `GET …/invitations`, `DELETE …/invitations/{code}`; at most 10 per event. `POST /invites/event/{code}` adds the member who uses it to `invited` (use counted in the same write; `InvalidOperation` for someone outside the server, NotFound for a link that ran out). Fixed (2026-11-19): someone who already sees the event gets the event back from a used-up or expired link instead of NotFound; someone outside the server with such a link gets NotFound, not `InvalidOperation`.
- New indexes `link_code` and `invitation_code` (unique, partial), made with the other event indexes.

### Events: answers, seats, visibility, drafts, organisers (2026-11-18)

Everything is added to what was there; nothing is renamed or removed, and an app from before these changes keeps working.

- **Answers.** `PUT /servers/{target}/events/{event}/rsvp` (`DataServerEventRsvp`: `Going`, `Maybe`, `NotGoing`) and `DELETE …/rsvp`. One answer per person and event; a second replaces the first. `ServerEvent.counts` (`going`, `maybe`, `not_going`, `waiting`), `ServerEventResponse.rsvp` and `.organiser`, `ServerEventList.answers`. The `interested` routes stay and mean "maybe" (`PUT` never lowers a "going"; `DELETE` withdraws); `interested_count` is now going plus maybe; `interested` is true for going or maybe.
- **Seats and waiting list.** `ServerEvent.capacity` (1 to 10000) and `waitlist` (default on). "Going" takes a seat or a place in line; `rsvp.waiting` and `rsvp.position` say which. A freed seat goes to the first waiting person in the same database write (one seat-list document per event, changed only by single atomic updates). `ServerEventFull` (409) when there is no seat and no waiting list. More seats, or removing the limit (`remove: ["Capacity"]`), move people up at once; fewer seats take nobody's seat.
- **Organisers manage people.** `GET …/attendees` (`answer`, `after`, `limit`), `DELETE …/attendees/{user}`, `POST …/attendees/{user}/seat`. Seating someone when every seat is taken is refused with `ServerEventOverCapacity { capacity }` (409) unless the request says `?over_capacity=true`; the check and the seating are one database write. The audit entry then has `over_capacity: true`, and `ServerEvent.over_capacity` tells everyone who sees the event that more people are seated than it has seats (also after the limit was lowered).
- **Visibility.** `ServerEvent.visibility`: `Place` (as before, the default), `Roles` (`visible_roles`, up to 20) or `Invited` (`invited`, up to 200 members; returned to organisers only). A restricted event still needs the place to be visible. Lists, fetches, every sub-route, the clash check, "my events", the gateway and the push at the start follow it; who may not see an event gets NotFound. Restricted events go to each person's private gateway topic, never to the server or channel topic, and their start is not announced in the system channel.
- **Attendee list privacy.** `ServerEvent.attendee_list`: `Organisers` (the default of a new event), `Participants` or `Everyone` (what an event stored before this setting keeps). "Not going" answers, and the time someone first answered (`ServerEventAttendee.since`), are for organisers only. The older `GET …/interested` returns an empty page instead of an error to someone the list is not open to: **an app from before this change shows the count of a new event but no names to members who are not organisers.**
- **Mature servers.** Events of a server marked as mature are for declared adults only, whatever their place; an event at an outside place of such a server is sent to each person who may see it, not to the server topic.
- **Nobody stays in an event they cannot see.** An edit that narrows who an event is for (or moves it to a channel some cannot view) withdraws the answers of the people it shuts out. Someone who lost sight of an event another way (a role taken away) can still withdraw: `DELETE …/rsvp` and `DELETE …/interested` remove their answer and answer NotFound as before. A new occurrence of a series takes along only people who may see it.
- **The gateway no longer says who answered.** `ServerEventInterest` on shared topics has an empty `user`; the person's own devices get it with their id, and the new `ServerEventRsvpUpdate`, on their private topic. New `ServerEventCounts` carries the counts.
- **Drafts.** `draft: true` on create gives status `Draft`: seen by organisers only (`GET …/events?drafts=true`), not started by crond, not sent over the gateway, own limit of 25 (`TooManyServerEventDrafts`). `POST …/publish` schedules it. `POST …/copy` makes a draft from any event without its answers.
- **Daily repeat** (`ServerEventRepeat::Daily`), in the creator's wall-clock time like the others.
- **Clash check.** `GET /servers/{target}/events/clashes?starts_at&ends_at&channel&location&exclude`: events you may see that overlap; a warning, nothing is blocked.
- **Organisers.** Permission `ManageEvents` (bit 49; never a default, Manage Server implies it, owners and Administrator hold it) replaces Manage Server for events; `MissingPermission` now names `ManageEvents`. `ServerEvent.co_organisers` (up to 10 members, named by people with the permission) may edit, publish, start, end, cancel, manage the people and read the numbers of that event; creating, copying and deleting need the permission. Audit actions: `ServerEventPublish`, `ServerEventStart`, `ServerEventEnd`, `ServerEventAttendeeRemove`, `ServerEventAttendeeSeat` (with `over_capacity`), `ServerEventRetention` (start and end by hand had no entry before).
- **Statistics.** `GET …/events/{event}/stats`: counts, waiting list, answers by the day they were first given, and for a voice place how many people were in the channel at the start (a number; no names are stored).
- **My events.** `GET /users/@me/events?from&to&answer&limit`: events across your servers that you may see.
- `GET …/events?from&to` lists every event that starts in a time range, past and cancelled ones too.
- Errors: `ServerEventInvalid { field, reason }` for the new fields (`too_many`, `out_of_range`, `not_a_member`, `unknown_role`, `empty`, `not_a_time`, `unknown`), `ServerEventFull`, `ServerEventOverCapacity { capacity }`, `ServerEventReasonRequired`, `TooManyServerEventDrafts { max }`.
- Rate limits: every new route under `/servers/{id}/events` is in `server_events_read` (GET) or `server_events` (writes, per server); `/users/@me/events` is in `server_events_read`.
- **Retention.** 12 calendar months after an event really ended or was called off (each occurrence of a series by itself), crond removes who answered, the seat list, the invited list and the helpers' ids; the event, its counts and the number present at the start stay, and `ServerEvent.people_removed_at` says when. Nothing that has not ended is ever touched. It runs whether or not events are switched on. Audit action `ServerEventRetention { event, records, outcome }` (`PeopleRemoved` or `EventDeleted`): no title, no person; the audit log keeps its own expiry. An account that is deleted loses its answers at once, in every server.
- **A deleted server does not delete its events.** What was to come, running or a draft is called off at that moment, without any gateway event or push; nothing starts and no next occurrence is made; answers and seat lists stay as they were. When its retention time is up, an event of a server that no longer exists is deleted as a whole. An event whose people were already removed goes with the server.
- **Events that are over do not move.** When someone leaves a server or deletes their account, their answer to an ended event goes, but nobody is moved into their seat and nobody is told; the counts of the past event stay.
- **The audit log and private events.** Entries of drafts and of events limited to roles or invited people carry no title. Who an organiser seated or removed (`ServerEventAttendeeSeat`, `ServerEventAttendeeRemove`) is stored, and shown only to a reader who manages that event and sees its people; for other readers `user` and the entry's target are empty, and a query by target returns no such entry.
- **No automatic disclosure to people who are not named on a private event.** In an event limited to roles or invited people, someone who manages events but is neither its creator nor a helper (the server's owner too) gets `GET …/attendees` only with a written reason in `X-Audit-Log-Reason`: `ServerEventReasonRequired` (403) without one; with one the access is logged as `ServerEventPeopleAccess { event }`. The `invited` list is likewise sent only to the people named on the event.
- **A deleted server keeps its audit log** until each entry's own time is up (`api.audit_logs.expires_after`); it was deleted with the server on MongoDB.
- **Sending to many people.** `EventV1::private_broadcast` sends one queue message for every 100 people (a message naming thousands was refused by the broker, which then closed the channel). More than 50 people are told beside the request. Count changes of a restricted event are sent live only when at most 100 people see it.
- **Fixed.** A deleted account's answers are removed in every server and its seat in an event still to come goes to the next person (they stayed, with their counts). "Download my data" has `events.json` (own answers with the event's title, time and waiting-list place; events created, invited to and helped with). Lists an edit emptied (roles, invited, helpers) are cleared on MongoDB. Before, a deleted server left its events behind with no rule at all; now they fall under the retention above.
- **Migration** `PENDING_SERVER_EVENT_ANSWERS` (numbered at merge): every "Interested" mark becomes "maybe" and all counts are set again from the rows. **Nothing is deleted:** events of servers that no longer exist and marks of people who are no longer members are counted and reported (`ServerEventAnswerMigration`), and kept. New indexes: answers by event, events by server and start, by creator, by invited person, by helper, by status and end, seat lists by server. Events still scheduled, running or in draft whose server no longer exists are called off (not deleted) and counted. After writing the counts it counts the rows again and, if they changed, repeats; after three passes it stops with a message to stop every API process. A second run changes nothing (MongoDB test runs it three times).

### Tickets: a name for the entry (2026-11-17)

* `ServerTickets.label`: a server can name the "Open a ticket" entry its members see (at most 32 characters; empty goes back to the app's wording).
* Tests: a closed ticket is deleted by whoever manages channels, not by the ticket staff or its opener, and the count of opened tickets stays.

### Temporary voice rooms and tickets (2026-11-17)

- **Room maker.** `VoiceInformation.maker` marks a voice channel as a room maker (set with `PATCH /channels/{target}`, `voice.maker`; Manage Channel; not for a stage). New `POST /channels/{target}/rooms` (Connect in the room maker; not for bots) answers with a voice channel made for the member: placed right after the room maker, with its permissions and user limit, named after the member, who gets Manage Channel and Move Members in it. `VoiceInformation.temporary` holds that member's id and is set by the server only. A member has one room per server: asking again returns it. The room is removed when the last person leaves it (voice-ingress, `participant_left`), or a minute after it was made if nobody joined.
- **Tickets.** `Server.tickets` (`ServerTickets`: `enabled`, `category`, `staff_roles` up to 10, `message` up to 500 characters, `opened`) is set through `PATCH /servers/{target}` (Manage Server; `opened` is the server's to keep; removed with `remove: ["Tickets"]`). New `POST /servers/{target}/tickets` (`DataOpenServerTicket`: `subject`; members only, not bots) opens a text channel `ticket-0001` hidden from everyone except its opener, the staff roles and the people who see every channel; `Channel.ticket` (`TicketInformation`: `opener`, `number`, `closed`) marks it. One open ticket per member (asking again returns it) and at most 3 an hour (`RateLimited`). New `POST /servers/{target}/tickets/{channel}/close` (the opener or anyone else who sees the ticket): the opener loses access and the channel is renamed `closed-0001`; `.../reopen` (staff) gives it back. A closed ticket is deleted like any channel.
- **Room controls.** New `PATCH /channels/{target}/room` (`DataEditRoom`: `locked`, `hidden`, `owner`), for the member the room was made for or someone who manages permissions in it. Locked: the room denies Connect to everyone but the people let in; hidden: it denies View Channel. `PUT` / `DELETE /channels/{target}/room/guests/{member}` let a server member in or take that back (at most 25). `owner` hands the room to someone who is in it; when the room's member leaves while others are inside, voice-ingress hands it to whoever has been there longest. `VoiceInformation.locked` and `.hidden` show the state. A room maker names its rooms with `VoiceInformation.room_name` (`{name}` stands for the member; set with `PATCH /channels/{target}`, `voice.room_name`).
- **Ticket types, claim and rating.** `ServerTickets.types` (`TicketType`: `id`, `name`, `staff_roles`; at most 5): `DataOpenServerTicket.kind` picks one, its roles answer when it has any, and `TicketInformation.kind` keeps its name. `PUT` / `DELETE /servers/{target}/tickets/{channel}/claim` (ticket staff) set and clear `TicketInformation.claimed_by`. `POST .../close` takes `DataCloseTicket.rating` (1 to 5), kept only from the opener as `TicketInformation.rating`; reopening clears it.
- No data migration: all new fields are optional and absent on stored documents.

### Design pack (2026-11-16)

- The user style allow-lists accept the design pack: 125 decorations, 67 nameplates and 58 effects added to `USER_STYLE_DECORATIONS`, `USER_STYLE_NAMEPLATES` and `USER_STYLE_EFFECTS`, in the same order as the web client. No route, event or data shape changes.

### Category settings (2026-11-15)

- New `PATCH /servers/{target}/categories/{category_id}` (`DataEditCategory`: `title`, `description`; Manage Channels): changes a category's title and its note (one line, up to 120 characters; an empty one removes it). `Category.description` is new. A category list sent through `PATCH /servers/{target}` keeps the stored note, as it keeps the stored permissions.
- **A private category is not told to people outside it.** A category with permissions of its own is sent only to people who can view it, who can view a channel inside it, or who can manage channels or permissions in the server (owner and administrators included). This holds for `GET /servers/{target}`, every route that answers with the server, the invite join answer, `Ready`, `ServerCreate` and `ServerUpdate`. A category without permissions of its own is sent to every member, as before.
- Being given or losing a role, a role or default permission change and a channel coming into or going out of sight send `ServerUpdate` with the category list as that person may now know it.
- **Moving a channel or deleting a category no longer changes who can see a channel.** A channel that followed its category keeps a copy of the category's permissions and stops following; when the category it lands in says the same as what applied before, it follows that one. Before, a channel in a deleted private category became visible to everyone.

### Creator Studio (2026-11-15)

- **Every design is published with the creator's promise**, not only effects: `POST /community/{kind}` with `submit`, `PATCH /community/items/{id}` with `submit` and `POST /community/items/{id}/submit` answer `FailedValidation` (`rights: required`) for a decoration or nameplate without `rights` (Creator Terms: accepted before the first submission; parts by others and AI parts named in the submission). A new image still needs a new promise. Designs published before this stay up and a reviewer can still keep or reinstate them; their next version needs the promise.
- New `POST /community/items/{id}/restore`: the creator takes back a design they withdrew (Archived to Draft; logged as `Restore`). It counts against the creator's limit again. A design that staff withdrew, a suspended design, and anything while the creator is suspended are refused.
- Hidden data (EXIF, text chunks, XMP, colour profiles) is now removed from decoration and nameplate uploads as well; it was removed from effects only. Frames are not touched.
- The promise counts for a new publication only if it was given under the Creator Terms version in force; after new terms the creator is asked again. What is already public is not touched.
- `POST /community/items/{id}/archive` takes an optional `{ reason }`. A withdrawal by staff is a takedown whether the design was public or not: the creator gets a statement of reasons (with the reason given) and can appeal.
- `GET /community/items/{id}`: for the creator, history entries by anyone else come with an empty `actor_id`; their own keep it. Reviewers still see every id.
- autumn listens on `ZRZUR_AUTUMN_PORT` when set (default 14704).

### Calls ring, decline and miss (2026-11-15)

- DM and group calls ring for 45 seconds (`RING_TIMEOUT_SECS`). New gateway event `VoiceRingEnd { channel_id, user, reason }` (`Declined`, `Answered`, `Missed`, `Stopped`, `Ended`) to the channel when one person stops being rung. voice-ingress starts the ring with the call, ends it when the person joins (on any device), when the call ends, and when the time is up.
- `PUT /channels/{id}/end_ring/{user}`: the person being rung may end their own ring (decline) without being in the call; people outside the DM or group get `NotFound`; the route also sends `VoiceRingEnd`.
- `call_started` system messages get `missed` (true when nobody but the caller joined).
- Web push for calls: the end of a ring replaces the "is calling you" notification (same tag) and says why, instead of sending "is calling you" again when the call ended; the push opens the conversation. The blanket end push to everyone in the DM is now sent only to people still being rung.
- voice-ingress listens on `ZRZUR_VOICE_INGRESS_PORT` when set (default 8500).
- Fixed: a call started again right after one ended sometimes did not ring. The voice server could still list the caller's previous session, which counted as a second person; only other people count now.
- Fixed: people looking at a voice channel from outside the call never saw who was muted or deafened. LiveKit sends no webhook for a mute, so the `track_muted` branch of voice-ingress never ran. New `PATCH /channels/{id}/voice_state` `{ muted, deafened }`: the client reports its own choice, the channel gets `UserVoiceStateUpdate`.

### Linked accounts: consent that names the US transfer (2026-11-15)

- Consent version 3 for all four services (`LINKED_CONSENT_VERSION`): the card now names the transfer to the company in the United States (GDPR Art. 49(1)(a); GitHub: Art. 45) and what the copy may reveal (Art. 9(2)(a)). New links of Steam, Twitch, GitHub and Battle.net store it; Twitch, GitHub and Battle.net links made before stored 0.
- Twitch and GitHub links under an older version are hidden from other people until the person agrees (the old text had no Art. 9 line).
- Twitch links under an older version get nothing sent to Twitch: the hourly upkeep reads no name, checks no live status, and ends their live-status subscriptions; agreeing (`PATCH … { "consent": 3 }`) sets the subscriptions up again. Meanwhile removing Zrzur at Twitch can't unlink such a link (it is hidden, and its copy is still blanked after 24 hours).
- `PATCH /users/@me/connections/{twitch|github|battlenet}` accepts `consent` (exactly the version in use, else 400), like Steam. `ServiceAccount.consent_current` (additive).
- Steam and GitHub Refresh answer 400 until a link agreed under an older version agrees to the current one. Steam keeps its picture rule: a link agreed at version 2 or later still shows pictures and achievements (`LINKED_CONSENT_PICTURES`). Battle.net's "Confirm" re-link stores the new version.

### Steam details: pictures, latest achievements, unlock days (2026-11-14)

- Linked Steam accounts keep each game's icon file name and up to 60 latest unlocked achievements across the 10 kept games (name and description in English from `GetPlayerAchievements` + `GetSchemaForGame`, icon file name, and the UTC day it was unlocked; the time of day is dropped on arrival). Each Refresh replaces the list. About 20 Steam calls per link or Refresh, never on viewing.
- The API hands out Steam picture addresses built only from app ids and checked 40-hex file names (`icon_url`, `header_url`, `store_url`); clients load them through the media proxy only.
- Consent versions: `LinkedAccount.consent` (missing = 1). A link made under the first text shows no pictures and no achievements until the person agrees (`PATCH /users/@me/connections/steam { "consent": 2 }`); `consent_current` in the owner view.
- Achievement icons are left out for viewers who have not declared they are adults; names, descriptions and days stay.
- `[api.connections.steam] no_picture_appids`: games whose makers asked; their pictures are never handed out.
- Steam text goes through the platform word list (a matching game or achievement is dropped). Reports on a profile carry the shown Steam games and achievements in the evidence.

### Linked Steam accounts (2026-11-14)

- A person links Steam by signing in on Steam's own page (OpenID 2.0). `POST /users/@me/connections/steam/start` returns Steam's sign-in address and remembers the request for 10 minutes (Redis, one use, bound to the person); `POST /users/@me/connections/steam/finish` checks the fields Steam sent back (return address, endpoint, signed field list, SteamID form) and has Steam confirm them (`check_authentication`). Steam gives no token, so none is kept.
- Only an allowlist is copied, on the person's request only (link, Refresh once every 10 minutes): Steam name, profile link, game count, total hours, the 10 most played games with hours and unlocked/total achievements. Real name, country, avatar, friends, online state and every time stamp are dropped on arrival. New collection `linked_accounts` (one Steam account links to one Zrzur account; `LinkedAccountTaken`, 409).
- A new link starts hidden; `PATCH /users/@me/connections/steam` shows it and hides single games. `DELETE` removes the link and the copy at once; both account-deletion paths remove it too, and "Download my data" gains `linked_accounts.json`.
- Profiles: new section id `games`; `UserProfile.games` is filled only when the person shows it, the section is not hidden, and the viewer may see personal sections (on a profile under 18 or of unknown age, friends only). A private Steam account shows `private: true` without name or link.
- Config `[api.connections.steam]` (`web_api_key`, `return_origins`); empty key = off (`FeatureDisabled { feature: "steam" }`). New errors `LinkedSignInFailed`, `LinkedServiceFailed`, `LinkedRefreshTooSoon`. Rate-limit bucket `connections` (10). Migration: indexes on `linked_accounts` (`PENDING_LINKED_ACCOUNT_INDEXES`).

### Events off by default per server

- `DELETE /servers/{id}/events/{event}?series=true` deletes this occurrence of a repeating event and schedules nothing next, so the series ends (before, every delete wrote the next weekly occurrence and the card looked unchanged). Without `series` one occurrence goes and the series goes on, as before.
- Servers have `events_enabled` (missing = off, so no migration). A manager turns it on with `PATCH /servers/{id}` `{ "events_enabled": true }` (Manage Server); `ServerUpdate` carries it. While a server has events off, `POST /servers/{id}/events` answers `FeatureDisabled { feature: "server_events_server" }`; events made before stay stored.
- While a server has events off, every `/servers/{id}/events` route (list, fetch, interested, edit, start, ...) answers the same `FeatureDisabled`, so bots and clients see what the app shows. crond leaves those events alone: nothing starts, is announced or gets a next occurrence; an event whose whole time passed while off (scheduled or active) is marked Completed quietly (no gateway event, message or push; `PassReport.missed`). Turned on again, an event still inside its time starts as usual and a repeating series gets its next occurrence.
### Sound library

- 115 ready-made soundboard sounds from Kenney's CC0 packs, built into the binaries (`crates/core/database/assets/sound-library`, 1.16 MB of Ogg Vorbis, each at most 5.0 s measured by ffprobe, brought to one loudness with a peak limit at -1 dBFS; names in en, ar, de, fr, es, ru, zh-Hans; `LIBRARY.md` lists the source pack of each, the pack licences are in `licenses/`).
- `GET /soundboard/library` (signed in; written once, `Cache-Control: private, max-age=3600`) lists categories and sounds; `GET /soundboard/library/{id}/audio?v=<version>` serves a file with `public, max-age=31536000, immutable`.
- `PUT /servers/{target}/soundboard/library/{library_id}` (Manage Soundboard, `servers` rate-limit bucket like uploads) adds one to a server: autumn stores every library sound once as a file hash at start, so delta only makes a new `sounds` file of that hash and the sound goes through the same path as an upload (measured length, per-server limit checked before and after saving, `SoundboardSoundCreate` event, audit log). The name defaults to the library name in the adder's language (`User.locale`). A library sound is on a server at most once (`SoundboardSound.library_id`; new error `SoundAlreadyAdded`, 409). crond keeps library file hashes when the last server sound using one is removed. Autumn checks again every 10 minutes (one `$in` query on `attachment_hashes._id`, `fetch_stored_attachment_hash_ids`) and stores any library sound that went missing; until then adding one answers `SoundLibraryUnavailable` (503, try again).

### Server insights

- `GET /servers/{target}/insights?days=7|30|90&time_zone=&utc_offset=` (Manage Server; own rate-limit bucket `server_insights`, 10): daily joins, leaves, members, messages, active members and voice minutes in the viewer's time zone (IANA zone with daylight saving, else the offset rounded to whole hours; half-hour zones put the hour around local midnight on one day), range totals, join sources (invite, Discover, custom link, other), the 10 busiest channels the viewer can see and the day counting started (`counted_since`). Privacy: messages count only in channels the viewer can see, daily totals included; per-channel counts, active members and voice minutes of 1 to 4 are `null`. One indexed find on `server_insights` (server, day) per view plus one read of the channels with messages; nothing reads messages or members then.
- Counts: joins and leaves (kicks, bans and deleted accounts too) are one `$inc` upsert as they happen (`Member::create_from` names the source; invite join passes invite, Discover or custom link), and drop the cached member count so it is read again. Voice time: every way out of a server voice seat goes through `delete_voice_state`, which takes `joined_at` in one Redis step (read and delete) and notes the seconds; crond moves them into the rows. Messages are never counted on the send path: crond rolls each finished UTC hour up once, 5 minutes after its end (clock skew), with one aggregation over that hour's id range of `messages`, then one update per server; an hour already in a row is skipped, and an insert that meets a row made at the same moment retries as a plain update. Active members (bots left out) are a Redis HyperLogLog per server and day (48 h expiry); MongoDB keeps only the number. Each crond step (retention, voice, closing days, the roll-up) runs on its own, so one failing never holds up the others; a server whose update fails is logged and skipped. Rows older than 90 days (plus one for time zones) are deleted; a deleted server's rows go with it, and nothing is counted for a server that is gone.
- Migration `PENDING_SERVER_INSIGHTS = -1`: indexes `server_insights` (server, day), (day) and a partial (open, day) for the open days. Safe to run twice.
- Time zones: unknown zone names are no longer cached (they came from requests).
- Second review: every count but the member total is `null` for 1 to 4 (joins, leaves, messages, join sources too). Visibility is checked for every channel with messages, in batches of 1,000 (a thread follows its parent, worked out once). Bulk reads go 1,000 ids at a time, servers and bots by `_id` only. An hour keeps its 100 busiest channels by name and sums the rest into `_other` (counted for nobody), and each row has a per-day channel map for range totals, so a row stays far under 16 MB. An hour that fails five passes is skipped (error log); crond warns when it is over six hours behind, and days older than two days close whatever the cursor. A seat's voice time is split over the hours it covered and capped at 12 hours; bots' voice time is left out; a finished room counts its seats too. Voice time waiting for a server that is gone (NotFound) is dropped; on a write error it goes back to Redis. The HyperLogLog is added, kept and counted in one Lua step, after the hour is in the row. Duplicate keys are matched by code 11000. A deleted account's leaves count once even if the deletion is tried again.
- `CROND_ONLY=server_insights` runs only the insights roll-up beside another crond (a warning says so at start). A second delta for tests takes `ZRZUR_DELTA_PORT` (as on integration).

### Security

- Sign-in: wrong passwords are counted per e-mail address (normalised, hashed in Redis), whether or not an account has it. From the tenth within fifteen minutes the address needs a captcha (`DataLogin` gains optional `captcha`; answer `CaptchaFailed`) when one is configured, else it waits one minute, doubling up to an hour (`LockedOut`), on the network the wrong passwords came from only (IPv4 /24, IPv6 /64). A password reset ends the wait. Keys are an HMAC-SHA256 of the address under `files.encryption_key`; counting is one MULTI step. Unknown and known addresses answer alike, so the lock no longer tells who has an account; the old per-account lock (three tries) is no longer used and is cleared on the next sign-in.
- Two-factor at sign-in counts wrong codes with `PUT /auth/mfa/ticket`: five in fifteen minutes pause the account's two-factor step (`LockedOut`).
- `PATCH /bots/{id}` refuses an `interactions_url` that is not a plain http(s) address (no `javascript:`, `data:`, credentials, and no empty string: clear it with `"remove": ["InteractionsURL"]`).

### Features

- Custom status emoji and expiry: `UserStatus` gains optional `emoji` (one unicode emoji from the supported list, or a custom emoji from a server you are in) and `expires_at` (in the next 7 days, needs a text or emoji). A new text replaces the old emoji and expiry; removing `StatusText` removes all three. A status past its expiry is never returned, and crond clears expired ones every minute and sends `UserUpdate` with `clear: ["StatusText"]`. `GET /users/{id}/mutual` is covered by tests (only servers both are in, nobody blocked). Migration `PENDING_STATUS_MUTUAL_INDEXES` adds the `status_expires_at` and `relations_id_status` indexes on `users`.
- Voice extras. **Stage channels**: `VoiceInformation.stage`; live topic, speakers (at most 50) and raised hands (oldest first, at most 200) in Redis, sent whole as gateway `StageUpdate`; `GET/PATCH/DELETE /channels/{id}/stage` (topic, end: everyone leaves), `PUT /channels/{id}/stage/hand`, `DELETE …/stage/hands/{user}`, `PUT/DELETE …/stage/speakers/{user}`; moderators are members with Mute Members; the audience gets join tokens that publish nothing, an invite or move updates LiveKit at once; leaving the call drops the stage seat. **Soundboard**: sounds per server (`soundboard_sounds`, files through autumn tag `sounds`: MP3/OGG/WAV, 512 KB, up to 5.2 s), 8 per server and 8 more per Wings level (`TooManySounds`), `GET/PUT/PATCH/DELETE /servers/{id}/soundboard[/{sound}]`, `POST /channels/{id}/soundboard/{sound}` sends `SoundboardPlay` to the call (not kept for a resume; bucket `soundboard_play`, 4 per 10 s per user), new permissions Use Soundboard (bit 47, in new servers' defaults) and Manage Soundboard (bit 48); events `SoundboardSoundCreate/Update/Delete`. **Stream previews**: `PUT /channels/{id}/stream_preview` (JPEG/WebP up to 20 KB from the sharer's client, one per 25 s, kept 75 s in Redis), `GET /channels/{id}/stream_preview/{user}`. New error `TooManySpeakers`. Migration `PENDING_SOUNDBOARD` (index, and Use Soundboard for existing servers). Review round 1: editing a voice channel keeps fields left out (`DataEditVoiceInformation`; a user limit change no longer drops the stage), and a stage turns on or off only while the call is empty; closing a stage needs Move Members too and is in the audit log; giving or taking the floor of someone else needs a higher rank (`NotElevated`); one raised hand per person every 20 s (`RateLimited`); `StageUpdate` carries only what changed (`StageChange`) and is also sent when a speaker or a raised hand leaves the call; speaker and hand limits are checked atomically (Lua); a speaker sent back is taken out of the call when LiveKit can't be told. Previews need Listen and an active share, and go with the share and the seat. Autumn measures each sound with ffprobe (`Metadata` Audio `duration_ms`), refuses over 5.2 s, and the API uses that length; sound limits are checked after the insert (rolled back over the limit); adding and removing sounds is in the audit log; `SoundboardPlay` goes only to the people in the call. The soundboard migration saves a cutoff, so servers made later are never touched. Review round 2: `max_users: 0` clears a voice channel's user limit; removing the voice info builds on the stored one, so it can't drop an occupied stage; a speaker LiveKit can't be told about is taken out of the call directly, and when that fails too the moderator gets `LiveKitUnavailable` (database errors take nobody out); lowering someone else's hand needs a higher rank; autumn runs ffprobe as a child process with a 10 s limit (killed when it runs over), checks the length against the file size, and the API refuses a sound autumn did not measure.
- `GET /bots/{id}/commands` (a bot's page in Discover and its profile) answers from memory for a minute for **public** bots (private bots are never kept; at most 4096 bots and about 64 MB of commands): no database query for a cached bot. On the same API instance, registering, syncing, editing or deleting a global command, editing the bot (its public flag) and deleting it drop the cached list at once, and a read already under way does not store it again. Other instances, and changes that bypass these routes (account deletion by crond), show after at most a minute. `/commands` has its own bucket `bot_public` (40 per 10 s); `GET /bots/{id}/invite` stays in `bots` (10).
- **Privacy:** `User.bot.owner` (the owner's account id) is sent only to the owner, the bot itself and platform staff; everyone else gets `bot` without `owner` (now optional in the schema), including `UserUpdate` events and ban and join-request lists. Who made a bot stays the owner's choice (`show_creator`, by name on `GET /bots/{id}/invite`).
- Polls in messages (behind `features.polls_enabled`, off by default): `DataMessageSend.poll` takes a question (1–300 characters), 2–10 answers (1–55 characters, optional emoji the author may use) and a duration of 1, 4, 8 or 24 hours, 3 days, 1 or 2 weeks, single or multiple choice. Posting one needs Send Messages and Send Embeds (no new permission bit). `PUT`/`DELETE /channels/{c}/messages/{m}/polls/answers/{a}/votes/@me` vote and take back a vote (React, the channel's write rules and the age rules apply; people only, own rate-limit bucket `poll_votes`, 10 per channel); `GET …/answers/{a}/voters` pages through who chose an answer for anyone who can read the message; `POST …/polls/end` ends it early (author, or Manage Messages). Readers get counts and their own choice (`poll.me`), never voter lists; gateway events `MessagePollVoteAdd`/`MessagePollVoteRemove` (counts), `MessagePollChoice` (to the voter only) and `MessagePollEnd`. crond closes polls past their end within a minute and a `poll_ended` system reply names the winner; a vote on an expired poll is refused (`PollEnded`, 409) and closes it at once. Poll messages can't be edited; deleting one deletes its votes; forwarding sends the question and answers as text. Votes are one conditional write each (a vote racing the close loses with `PollEnded`; on multiple choice only the chosen answer changes); events carry `version` and `total`. AutoMod reads the question and answers. A closed poll always gets its reply (crond retries; the reply id is fixed per poll). Push previews use the poll text. Data export: own polls with counts, not voters, and `poll_votes.json`. Migration `PENDING_POLL_INDEX` (index `polls_to_close`).
- Scheduled server events (behind `features.server_events_enabled`, off by default): `/servers/{id}/events` to create, edit, start, end, cancel and delete events (Manage Server, or the event's creator for everything but delete), place = voice channel, text channel or an external place, optional cover (`banners` tag), weekly / every two weeks / monthly repeat (the next one is made when one completes). Members see only events whose place they can view. "Interested" marks with a public count and a paged list. crond starts and completes events on time; at the start interested members get a push and the join message channel gets an `event_started` system message when everyone can see the place. Gateway: `ServerEventCreate`, `ServerEventUpdate`, `ServerEventDelete`, `ServerEventInterest`. Audit log: create, edit, cancel, delete. New error `TooManyServerEvents` (100 upcoming per server). Indexes in migration `PENDING_SERVER_EVENT_INDEXES`. Review round: every manager action needs Manage Server (not while timed out; the creator has no own rights); the start is announced in the system channel only when no default, role, member or category setting and no mature flag could keep anyone from the place; repeats are computed from the series start in the creator's IANA time zone (stored as `time_zone`, read from the system zone database), so local times survive daylight saving changes and monthly dates clamp per month (31 Jan, 28/29 Feb, 31 Mar); the next occurrence has an id derived from the one before and is written before the one before ends, and crond gives finished series without a next one theirs; cancelling or deleting one occurrence keeps the series (`cancel?series=true` stops it); crond cancels events whose channel is gone and managers can still see and delete them; members who leave lose their marks; external places refuse non-web links; each occurrence has its own use of the cover, released on delete; `ServerEventInterest` carries the place channel.

### Fixes

- voice-ingress: a join is announced as soon as the voice state is stored. Before, a failed LiveKit permission sync stopped the `VoiceChannelJoin` event, so people already in the room never saw the newcomer (they saw them only after a reload). The sync now retries once; if it still fails, the user is taken out of the room instead of keeping LiveKit's default permissions.

## Zrzur 26.0.0 (unreleased)

Zrzur's first release, built on Stoat 0.15.5 (below).

### Features

* **Copy a server setup:** `POST /servers/import/preview` reads the layout of a server (categories, channels with topics, order and slow mode, roles with colours and order, the permissions that have a match, role overrides on channels and categories) out of a Discord server template, from its link or from its pasted text, and returns it with a report of what is left out. `POST /servers/import` builds a layout into a new server in one request, or into an empty server of the caller (`into`); it checks the layout from scratch. Messages and members are never read. A role never gets control of the server (administrator, manage server, roles, permissions, webhooks); such roles are named in the report. Nothing about the person who made the template is kept or logged. Behind `features.server_import.enabled` (off); reading by link, the one call this server makes to Discord, has its own switch `link_fetch` (off), a fixed address, no redirects, 5 s, 1 MB and a count per account per hour. New errors `ImportInvalid`, `ImportTemplateNotFound`, `ImportSourceUnavailable`, `ImportTargetNotEmpty`; root config `features.server_import` and `server_import_link`.
* **Message search filters and server-wide search:** `POST /channels/:id/search` takes `authors`, `mentions`, `has` (link, file, image, video, embed, sound), `before_date` / `after_date` and may combine `pinned` with `query`. `POST /servers/:id/messages/search` searches only the channels the user may view and read the history of (mature channels for declared adults only), never reporting the others; one rate-limit bucket `message_search` per person shared by every server (5), and MongoDB stops a search after 5 s. The reference database now applies sort, limit, time windows and the pinned filter as MongoDB does (pinned was inverted).
* **A ban that deletes messages removes their files:** banning with `delete_message_seconds` now removes the deleted messages' attachments and rich-embed images with their forwarded copies, like a single delete. Bans and account erasure read the messages 500 at a time instead of all at once.
* **Bulk deletion and account erasure remove files:** moderator bulk delete (`DELETE /channels/{id}/messages/bulk`) and account erasure now remove the messages' attachments and rich-embed images exactly like a single delete, forwarded copies included, and purge them from the CDN. Before, bulk delete left every file reachable, and erasure only found files through the legacy `message_id` field (kept for old records). The in-memory test database also deleted every message of the channel on a bulk delete.
* **Message forwarding:** `POST /channels/{target}/messages/forward` (`{ channel, message, note? }`) sends a copy of a message the person can read (View Channel and Read Message History on its channel) into a place they can write, through every rule of a normal send (Send Message, Upload Files for files, Send Embeds for rich embeds, slow mode, AutoMod on the text and the note, the messaging rate limit). Files get a new record of the same stored file, so the original stays with its message and deleted or reported files are left out. The copy records `forwarded` (message, `sent_at`, note; channel and server only for a server channel whose message was not masqueraded), pings nobody and cannot be edited. Official messages are refused. Copies record `forwarded_from` (the first record of the file): deleting the original removes every copy; a safety removal of the original or of a copy removes the whole family and purges it from the CDN; a report keeps every record as evidence. Old files without a hash are left out, and the copies of a refused forward are marked deleted at once. A message from a mature channel goes only into another mature channel (`ForwardMatureContent`). Behind `features.message_forwarding_enabled` (off; root config `features.forwarding`).
* **Gateway resume:** every event after `Ready` (except `Pong`) carries `seq`, counted per session from 1. A client that sends `Authenticate { resumable: true }` gets `session_id` in `Ready`, and its session is kept for 2 minutes after a drop (up to 1000 events). `Resume { token, session_id, seq }` on a new socket sends the missed events again, in order, then `Resumed`; events the session can no longer see (a channel hidden, a server left, webhooks no longer managed) are left out. A gone session, another login's token or too many missed events get `InvalidSession { resumable: false }`, and the client may authenticate on the same socket. A revoked login is refused as on `Authenticate`. Clients that never ask for it work as before. Limits: 4 MB of events per session (typing gets a `seq` but is not kept), 3 dropped sessions per user and 10,000 per process (a new drop ends the oldest), and a dropped session that missed more than it keeps ends at once. While dropped the user counts as offline (a bot gets `ApplicationOffline` for interactions) until the resume. Writes to a socket time out after 10 s (the session then counts as dropped); a login the session timer finds closed ends the session at once; a socket may try `Resume` twice. `GET /gateway/schema` lists the close codes (1013, 4014, none after `Logout`), which events carry `seq`, and the resume limits.
* **Bot intents:** a refusal for a privileged intent now says what the gateway read as switched on (`Disallowed intents: MESSAGE_CONTENT (switched on: none)`), and a bot that can't be read is no longer refused with 4014 (`InternalError`, close 1013, try again). A bot can read its own settings, privileged intents included, with `GET /bots/{its id}`.
* **Thread first message:** `POST /channels/{target}/threads` takes `embeds` and `components` for the first message, which is sent as an ordinary message of its author (same permissions and mention rules; refused means no thread).
* **Channel not found:** `GET /channels/{id}` answers `NotFound` to someone outside the channel's server, as `GET /servers/{id}` does; members without access still get `MissingPermission`.
* **Command permissions per server:** `GET/PUT /servers/{id}/applications/{app}/permissions` and `PUT /servers/{id}/applications/{app}/commands/{command}/permissions` store allow/deny rules for roles, members and channels (the server id means everyone or every channel). Command rules come before app rules, a member rule before role rules, a deny on any role beats an allow on another, and with no matching rule `default_member_permissions` decides; the server owner passes member and role rules. Interactions and `GET /servers/{id}/commands` (new optional `?channel=`) apply them. Editing needs Manage Server and Manage Roles and writes `CommandPermissionsUpdate` to the audit log; uninstalling an app removes its rules. An Allow needs the editor to hold the command's `default_member_permissions` (for the whole app: every command's), and rules about roles at or above the editor or members who outrank them are refused (the owner may write any rule). Ids must be roles, channels or members of the server; stored rules whose target is gone are dropped on save, a deleted command or channel takes its rules with it, and the audit entry keeps the rules before and after. A thread or forum post follows its channel's rules, and buttons and menus on a command's answer follow that command's rules. New `GET /servers/{id}/applications/{app}/commands` lists every command of the app for managers, unfiltered. Collection `command_permissions` with an index on server and app (`PENDING_COMMAND_PERMISSIONS_INDEX`).
* **Commands in a DM with the bot:** a bot's global commands without `default_member_permissions` can be used in the person's DM with it.
* **Public bot page data:** `GET /bots/{id}/invite` adds `verified`, `server_count` and, only when the owner turns on the new bot setting `show_creator` (off by default), `creator` (username and display name, never the id); new `GET /bots/{id}/commands` lists a public bot's global commands (also for its owner and anyone with a DM with it).
* **Administrator permission:** `Administrator` (bit 46) on a role gives every right in every channel of the server, whatever the channels' overrides say; a channel override can neither give nor take it, but a higher role can deny it. It is never a default and only someone who has it (or the owner) can give it; role ranks still decide which roles and members an administrator can change; a server mute or deafen still holds, and a timeout on an administrator is refused (`InvalidOperation`). Server templates can't create it.

* **Server default notifications:** optional `default_notifications` on the server (`All` or `Mentions`; missing reads as `Mentions`), set by `PATCH /servers/:id` with Manage Server and kept in the server's audit log edit entry. Additive: old clients ignore it.
* **Account erasure removes designs:** `Account::mark_deleted` and `erase_completely` delete all of a person's community items with their favourites, use counts and review log, release every file the items claimed, take them off everyone wearing them, and clear the person's platform role.
* **Creator rank is 18+:** `grant`, `reinstate` and `POST /admin/permissions` refuse anyone who has not declared an age of 18 or over (`FailedValidation`, rule `adult`), and `platform_permissions` drops `cosmetics.submit` for them, so someone under 18 who already has the rank cannot upload, create, edit or publish.
* **Takedowns of every design:** decorations and nameplates taken down get the same Safety case, statement of reasons and appeal as effects, and their wearers are told.
* **Plugins off for launch:** `features.plugins_enabled` (off) closes the whole plugin system. While off, the 25 `/plugins` routes and the four `/staff/plugins` routes answer `PluginsDisabled` (403) before the session or the body are read, and the root config says `features.plugins: false`, so the app loads no plugin. No code or stored data is removed; turning it on and restarting delta brings everything back.
* **Creator effects:** a third community kind, `Effect`, next to decorations and nameplates: a transparent animated file that plays once over a profile, kept in the `effects` file tag. At upload autumn removes hidden file data and refuses files that flash (WCAG 2.3.1, tested over black and white), repeat, run over 4 s or 120 frames, show a frame under 33 ms, are not 9:16, or cover more than half of the picture. Effects go to a reviewer before they go live; the promise about rights is stored with each one. A takedown of an effect gives the creator a statement of reasons and an appeal, and its wearers a notice. `features.community.effects_enabled` (on) stops them all; it takes a restart. Their files stay in caches for at most 5 minutes.
* **WebM for designs:** the `decorations` and `effects` tags take a WebM of up to 8 MB. autumn turns it into an animated WebP with ffmpeg (VP8 or VP9, libvpx decoders for the alpha layer, no sound, no metadata) and it then goes through every check an image gets. The WebM is never stored. ffmpeg runs without a shell, with one demuxer, two decoders, pixel and allocation limits, and is killed after 20 s.
* **Flash check on decorations and nameplates:** animated files on the `decorations` tag, converted WebM included, are checked for flashing as effects are, at the largest size each is shown: 312 x 312 for a decoration, 1600 x 300 for a nameplate, told apart by shape. Still files are not read. Designs already published are not re-checked.
* **Previews of designs:** an animated decoration or nameplate now has a still preview at `/decorations/<id>`, the fullest frame in its own proportions, and design files redirect only temporarily.
* **Design takedowns and caches:** decorations and nameplates leave CDN caches within five minutes. A takedown takes a design off its wearers first and sends the notices in the background.
* **Creator promise on MongoDB:** `update_community_item` now writes `rights`. Before, a promise given by an edit was lost and a new file kept the old one.
* **Review of designs:** the console sends the version it showed, and `design_decide` requires it. `version` is on `CommunityItem` for creators and reviewers. Changing a design in review sends it back to Draft, and an effect without the creator's promise can't be approved.
* **Creators:** the `creations` section respects a hidden section for the designs route and the `creator` flag. Holders of the rank who aren't declared adults are not named or shown as creators.
* **Nameplates:** the eleven flags the app offers are on the allowed list again.
* **Flash check holes closed:** a looping file is tested for at least 2 seconds and two passes, so the step back to its first frame counts; files smaller than the test grid are scaled up to it; unknown chunks no longer hide later WebP frames; a file whose frames do not match what it declared is refused, and an APNG can no longer pass as a still by declaring one frame.
* **Flash check on animated WebP:** frames are composed the way a browser does. The decoder used before never cleared what a frame disposes of, so a strobe read as a still picture.
* **Designs are decided in the admin console:** `GET /staff/designs/review`, `POST /staff/designs/review/<id>` and `GET /staff/designs/review/<id>/file`, with `platform.reports.*`, a fresh step-up for decisions, and the audit log.
* **Creators on profiles:** `/users/<id>/profile` gains `creator`: true for an active creator, on a minor's profile only for friends. `GET /community/creators/<id>/designs` gives what they published with no name data, also with their page closed.
* **Export:** the data export gains `creations.json`.
* **Errors in plain words:** `FailedValidation` gains `fields`, a list of `{ field, rule, min, max }`, so the app can say "Between 20 and 1000 characters" in the reader's language; `error` is one plain English line, never the text of a validation library. Production answers carry no source path and no database operation or collection name; these go to the log.
* **Bot limit says when it resets:** `TooManyBotCreations` gains `resets_at`, in milliseconds: when the account can make a bot again.
* **Word list:** it reads public profile text: display name, pronouns, status, about and links. A second starter list drops four entries and adds three (`update_platform_word_seed`, one run, never touches what staff did). AutoMod and the word list ignore a bare ء and repeated letters, and match more Arabizi.
* **Word list in a file:** the starter entries of the platform word list moved to `crates/core/database/assets/platform_words_seed.txt`. The seed writes them once and never again: an entry staff removed stays removed.
* **A server from a template in one request:** `POST /servers/create` takes an optional `blueprint`: categories, text and voice channels in order, roles with colour and allowed rights, read-only channels, the rules channel, member invites off, and the icon. It is saved whole: channels first, closed from their first write, then the server with its roles and categories in one document; if a step fails, what was saved is removed and the request fails. Same name and colour rules as the separate routes, at most 50 channels, 20 categories and 20 roles, within the server limits. Roles can hold everyday and moderation rights only; server settings, permissions, roles, bans, webhooks, audit log and join requests are refused with `FailedValidation`.
* **Official accounts:** the server keeps an `official` mark on the account instead of one fixed id, and seeds Zrzur, Zrzur Support, Zrzur Safety and Zrzur Updates on every start, with names in ar, en and de. The mark changes only through `/staff/official` with two people. Tickets come from Support, moderation from Safety, news from Updates. Reacting to, answering, pinning, reporting, befriending or blocking an official account is refused with `OfficialAccount` (403). Names containing Zrzur or زرزور, lookalikes included, are refused for usernames, display names, nicknames, bots, webhooks and masquerade; old names stay until changed. The official picture is cached for an hour, not a year. Grant notices no longer carry the staff reason.
* **Official bots:** bots Zrzur runs itself carry their own `official_bot` mark, given only by `zrzur-staff` on the server, for names on a fixed list. Reserved names stay refused for everyone else.
* **Example secrets stop a release run:** every example value in the default configs is marked as one. The start check now covers the files key, the VAPID key, the S3 keys, the RabbitMQ password, MongoDB and Redis URLs without a password, and example LiveKit secrets. It names the setting and never prints the value. It applies with `production = true`, with an API host that is not local, and in any release build; a release build on a local host passes only with `ZRZUR_ALLOW_EXAMPLE_SECRETS=local`, which those first two ignore. Debug builds on a local host, so the dev stack and tests, start as before.
* **Files at Cloudflare:** autumn separates browser and edge times. Public images `public, max-age=3600, s-maxage=2592000`, attachments `public, max-age=3600, s-maxage=86400`, drafts, private files and the moderation route `private, no-store`, and any answer that doesn't say otherwise (errors, redirects) too. Until purge is configured `s-maxage` is one hour. No `Set-Cookie` on files. Purge by URL (`[files.purge]`, token limited to Cache Purge, from `Revolt.overrides.toml` or `REVOLT__FILES__PURGE__API_TOKEN`) covers the preview, `/original` and the named file: at once for a removed message and a design leaving the public, within a minute through crond for every other deletion, reported files kept as evidence included. Up to 4 tries in the background; no token means no purge and a log line, never a failure.
* **Upload limit 95 MB:** Cloudflare's free plan stops uploads at 100 MB. File limits are at most 95 MB; `body_limit_size` is 96 MB, which leaves room for the multipart envelope.
* **Suspended creators:** their designs leave the gallery, search, favourites, their page and everyone wearing them, and the files answer only them and reviewers, until they are reinstated. Suspending needs a reason. It is recorded as a `SuspendCreator` action on the person's standing, so `POST /safety/standing/{action_id}/appeal` works as for any action. New: `GET /community/creators/@me/standing` (reason, since, appeal) and `GET /community/items/{id}/download` (own copy, in any state). A suspended creator can't publish, edit or archive. A design suspended on its own stays down after reinstating.
* **Creator privacy:** `/community` answers never carry `username` or `discriminator`: `CommunityCreator` has `display_name` (the username, without the tag, if there is none) and `open_page`. Staff views keep them in `CreatorAccount`. `GET /community/creators/{id}`, `?creator=` and finding a creator by name work only if they turned `show_public_name` on, for everyone but the creator.
* **Bot verification:** `privacy_policy_url` and `terms_of_service_url` are required, https only, and saved on the request and the bot.
* **Eight new badges:** One Year, Three Years, Community Builder (a server reaches 100 members), Big Community (1000), Verified Server Owner, Verified Bot Developer, Saha Champion, Community Helper. Time badges come from a paced walk over accounts, the first of which also reaches earlier accounts. Server size is checked on join, from the hourly cached count. Saha Champion is given by staff until a record of wins exists.
* **Faster lists:** indexes for popular designs, Discover and file deletion.

* **Profile upgrade:** `UserProfile` gains links, interests and section order, and no route can send a profile without passing `for_viewer`, so hidden sections and a minor's links and interests stay on the server. `style.effect` takes Zrzur's own effects. A platform-wide word list kept in the admin console, with AutoMod's matching; `ProfileTextFiltered` when it refuses a text.
* **Closed sessions stay closed:** renaming a session or changing its push subscription writes only that field, so a request in flight during a logout can no longer bring the session and its token back. Push unsubscribe now works on MongoDB.
* **Session limits:** an account keeps at most 10 sessions; a new sign-in over that closes the least recently used, with its `DeleteSession` event and push subscription. A session unused for 30 days stops working at once and crond removes it daily. `last_seen` now also counts HTTP use and pings on an open socket, written at most once an hour per session. Sessions from before this get a full 30 days from the migration (`idle_grace_until`), and until the migration has reached a session it does not expire. An expired session gets no push, and a socket whose session is closed or expired ends with `Logout`: it looks once after its first subscribe, so a delete just before that is not missed, and then once an hour on a timer, with pings counted as use. A socket that never pings lets its session expire. `GET /auth/session/all` and `PATCH /auth/session/{id}` add `last_seen` and the list leaves out expired sessions. Both numbers are `[api.sessions]` in `Revolt.toml`.
* **Buttons in DMs with a bot:** buttons, string menus and forms on a bot's own messages work in the person's DM with that bot. The interaction has no `server_id` and no `member`; commands stay in server channels. Clicks used to fail with "interactions are only available in server channels".
* **AutoMod, Arabic:** keyword rules also catch letters split by spaces or dots (three or more) and Arabizi spellings of Arabic words (three or more consonants).
* **AutoMod alerts and intents:** a bot without the Message Content intent no longer gets the quoted text of an AutoMod alert.
* **Webhooks audit:** tokens start with `zrzur_wh_`, are stored as hashes and shown once (`POST /webhooks/{id}/token` makes a new one, the old one stops at once; webhooks made earlier keep working). `GET /webhooks/{id}` needs Manage Webhooks; webhook gateway events reach managers only; a token can no longer change a webhook's permissions or channel, and `GET` with a token leaves out the creator. Mass mentions need the webhook's permission and `allowed_mentions`, on no account by flag. Names: 1 to 80 characters, nothing hidden, nothing that passes for Zrzur, its staff or a verified bot. 5 messages in 2 seconds per webhook, 30 a minute per channel, 20 wrong tokens a minute per address, with `429`, `retry_after` and `X-RateLimit-*`. AutoMod reads webhook messages and embeds, on edits too. Execute answers 204 unless `?wait=true`, takes `username`, `avatar_url`, `embeds` with `color`/`image.url`/`thumbnail.url`, `allowed_mentions`, `thread_id`, `thread_name`, `applied_tags`, Discord-style `flags` and multipart `files[n]` with `payload_json`. New routes: `GET /servers/{id}/webhooks`, `POST /webhooks/{id}/test`, `/slack`, `/messages/{id}`. `/github` covers 19 events and answers 204 for the rest. Caps: 15 per channel, 1000 per server. `UploadFiles` joins the default webhook permissions for new webhooks. Without `features.webhooks_enabled`, creating and listing are refused too.

* **Sessions vs. database trouble:** a token nobody knows is `InvalidSession`; a failing database answers `DatabaseError`/`InternalError` (HTTP 500, JSON) from the request guards, the gateway and `GET /onboard/hello`, so clients retry instead of logging a good session out. `X-Ratelimit-Global` is exposed to browsers (CORS). Saving a read no longer depends on one Redis key: the newest message and "a queue message is out" are kept apart, and the second expires after 120 s however many acks follow, so a lost queue message or a leftover `acker:*` key blocks a channel for two minutes at most. Leftover keys need no clean-up.
* **Release integrity:** with `production = true` (or a public `hosts.api`) the server refuses to start while the VAPID private key or the files encryption key is still the default shipped with the source; the message explains how to generate new ones and warns that changing the files key without re-encrypting makes stored files unreadable. Shield, easypwned and Sentry stay off until configured; `features.age_safety.mature_content` now defaults to `blocked`. The API root reports an optional `commit` (the build's git commit; build arg `ZRZUR_GIT_COMMIT`).
* **Category permissions:** a category has default and role permissions; channels synced with it use them instead of their own (`PUT /servers/{id}/categories/{category}/permissions/default|{role}`). Channels made in a category start synced, unless they start private. Editing a synced channel's permissions keeps a copy of the category's and stops following it; `POST /channels/{id}/permissions/sync` follows it again. Existing channels are not changed.
* **Threads and forums:** start a thread from a message or with a first message (`POST /channels/{id}/threads`), list them (`GET /channels/{id}/threads`), rename, archive, lock and tag them (`PATCH /channels/{id}/thread`). A thread uses its channel's permissions and mature setting; a locked thread is for moderators, a message reopens an archived one. Forum channels (type `Forum`) hold posts only: each post is a thread with a title, a first message and tags (`forum_tags`, `require_tag`). Threads are not in the server's channel list; Ready carries the active ones.
* New databases also get the staff, support and thread indexes (they were only created by migrations 61-65, which a new database skips).
* Server safety: verification levels, pause invites and DMs, raid alerts, AutoMod with Arabic normalisation, rules screening.
* Invites expire after 7 days unless "never" is chosen, with a maximum number of uses; paged member lists.
* **Public Discover and Designs pages:** `GET /discover/servers/{id}` also takes the server's custom link name; server cards carry `link` and an approximate, cached `online_count` (none for servers over 5,000 members). Published community designs, a design's page and a creator's page (`GET /community/{kind}`, `/community/items/{id}`, `/community/creators/{id}`) can be read without an account; drafts, favourites and everything else still need one.
* Platform staff API for the admin console, official messages and support tickets; one bot verification flow.
* Bot security: gateway intents, DM privacy, per-bot rate limits, hashed tokens, masquerade rules, audit log entries.
* **Bots, review and legal fixes:** a disabled bot, or the bot of a suspended or banned owner, gets no interaction on any path, and its endpoint routes are closed. People picked in commands, selects and forms are shown from the bot's view, so friends and blocks never reach it, and the Presence rule covers selects and forms too. Bots have no mentions inbox. Saving an endpoint needs an MFA ticket; its two test requests vary in order and have their own ids. Signing keys under 32 bytes are refused, one bot may have 32 deliveries on their way out, and nothing is pooled, so each delivery looks the name up again. Without the Servers intent direct message and group channels still arrive, and without Voice States `Ready` looks up no voice participants. Past 100 servers an unapproved privileged intent is left out of the connection instead of closing it.
* **Interactions over HTTP:** a bot can receive interactions at its own HTTPS address instead of the gateway (`POST /bots/{id}/interactions/secret`, `PUT` and `DELETE /bots/{id}/interactions/endpoint`). Requests are signed (`X-Zrzur-Signature`, `X-Zrzur-Timestamp`, `X-Zrzur-Delivery`); an address is saved only after it refused a wrong signature and answered a signed `Ping`. The address rule is the link proxy's, now one list in `revolt_config::net`, checked again on every DNS lookup; no redirects, three-second timeout, capped answer. Signing secrets are derived from `api.interactions.signing_key` (empty switches the feature off), so the database holds only a generation number. Signing is in `util::signed_request` and sending in delta's `util::signed_delivery`, for any later feature that calls out.
* **Thread and forum permissions:** three channel permissions, read on the parent channel with its overrides and needed on top of Send Messages: `CreateThreads` (bit 43), `SendMessageInThreads` (44, also for forum posts, which are threads) and `CreateForumPosts` (45). Sending, editing, interaction answers and webhooks all obey them; a webhook also needs them from the person or bot that made it. `threads_disabled` and `forums_disabled` on a server (needs Manage Server) stop anything new: `ThreadsDisabled`, `ForumsDisabled` (403). What exists stays readable. A migration gives every server and sending webhook the new bits, so nothing changes until an owner changes it. A bot command counts as writing in the thread it runs in: the person using it is refused where they can't write, as in a locked thread. A switched-off thread takes nothing new, but people can still delete their messages and remove reactions; webhooks can delete their own messages there. Gateway `BeginTyping` now follows the same rule as `POST /channels/{id}/typing`, so nobody shows as typing where they can't write. The migration marks what it has handled (`thread_permissions_done`, `migration_claims`), so a later run or a second delta can't undo an owner's change: stop every old delta before it runs.
* **Rate limits and error codes:** a 429 body gains `global` next to `retry_after` (milliseconds, as before) and stays without `type`, which is how clients tell it from a typed 429 such as `InSlowmode`. Every error `type` is listed in `crates/core/result/error-codes.txt`; a test refuses renaming or removing one.
* **Bots over REST:** `POST /channels/{id}/typing` shows typing without a gateway connection (5 requests per 10 s per channel), and bots may list pinned messages with `POST /channels/{id}/search` and `pinned: true`; text search stays closed to them. Threads, forum posts, invites and reactions are covered by tests that use a bot's own token.
* **Gateway intents, complete:** the Servers intent now really decides whether a bot gets server, channel, role, emoji and webhook events (its connection keeps following them, so permission checks stay right), and Ready leaves out what a bot's intents don't cover. Without Presence no message author, interaction user or REST answer shows who is online, and `exclude_offline` can't reveal it. The member search needs Server Members like the member list. The gateway uses the API's rule for bots past 100 servers (approved privileged intents only). Edits of a message that mentions the bot stay readable without Message Content; system text no longer slips through it. `GET /gateway/schema` publishes every gateway event and client message as JSON Schema, generated from the types in use.

* **Reports (DSA Art. 16, 17, 20):** `POST /safety/public-reports` takes reports without an account; every decision carries a statement of reasons; reporters are told how a case ended, also when an action is revoked without an appeal; contact details of public reporters go 12 months after a case closes.

### Bug Fixes

* **Private channels arrive at once:** a channel made private while someone could see it stayed in their gateway connection's list of channels, so a role change that let them in again was never announced and the channel showed only after reconnecting. The gateway now drops a channel from the connection as soon as it is hidden, and ignores updates for a channel it cannot load.
* **Your own messages are read for you:** sending a message acks it for the author (not for bots), as `PUT /channels/:id/ack/:message` does, so `/sync/unreads` after a reconnect and every other session start after it. The author is never a mention recipient: replying to your own message, naming yourself, or your own `@everyone` or role mention adds no mention and sends no push to you (`Message::mention_recipients`, and the mass-mention consumer in pushd).
* **Official account:** it shows the logo, the controller face version B, instead of the old bird. Nobody can start a conversation with it or report it when signed in; the conversation it started still opens read-only.
* **Public reports by custom link:** a report that names a server or a Discover page by its custom link now finds the server and its owner, so the case can be acted on, also when the name looks like an id. Only our own sites or a bare name count, and an id in the query names nothing.
* **CORS on files and GIFs:** the file and GIF services answer only the Zrzur sites, this server's app, the admin console and `ZRZUR_CORS_ORIGINS`, from one list shared with the API; never `*`. The API now sends `Vary: Origin` too.
* **CORS preflights and rate limits:** `OPTIONS` requests are no longer counted or limited on the API, files and GIF services. They used up the bucket of the call they announce, and a 429 on a preflight has no CORS headers, so browsers reported a failed fetch. The `/auth/*` limit is now a setting, `api.security.ratelimits.auth` (default 15 per 10 seconds per address), for test stacks where all traffic comes from one address.
* Deleting a channel left its webhooks alive: the cleanup named the wrong collection and field.
* GitHub text with Arabic letters panicked the route (slicing by bytes).
* Webhook tokens no longer reach the logs: Rocket's request lines are off and the rate limit line masks them. Errors on webhook routes carry no source path.

* **Account invites on MongoDB:** an unused invite was always refused (`InvalidInvite`), so invite-only registration let nobody in. The claim looked for `used: false`, a field that is not stored while it is false.
* **Admin console in production:** `api.security.staff_origins` also passes CORS. Since CORS was limited to the listed sites, the console at admin.zrzur.com was refused with 403 before the staff check.
* A taken custom link answers `VanityTaken` in debug builds too (it was a 500).
* Tests on MongoDB run on `database-test` (in memory, port 27018) and remove their databases; a full run takes under 4 minutes.

* An account invite works once, also when two sign-ups use it at the same moment.
* Password reset and verification mail: one mail per address every five minutes, five a day, twenty an hour per source address. Over the limit the answer is the same and no mail goes out.
* `/auth` rate limits are counted per address, whichever session is used.
* Wrong passwords no longer extend a running lockout, and attempts older than an hour are forgotten. Login says nothing about an account until the password is right.
* Link proxy: reads at most 25 MiB of any remote file, 16 downloads and 4 image decodes at a time, 200 requests per 10 seconds per address. `/embed` can require `january.embed_key` (empty keeps it open).
* File previews are cached and at most 4 render at a time.
* Browser notification endpoints must be a known push service; saved ones that are not are dropped when next used.
* `PUT /auth/mfa/ticket` pauses for 15 minutes after 5 wrong answers, and a time code works once.
* A password change ends every other session, and a password reset ends all of them (`remove_sessions` is ignored).
* A report about a file finds its message from `used_for`, and the uploader when there is no message; a message from a webhook or bot is answered for by the webhook's creator or the bot's owner.
* Public reports: an address over its limit never reaches hCaptcha, and a site key without a secret accepts no token.
* A legal hold on a public report copies servers, listings, bots, designs and files too, and records when no copy exists.
* Statements of reasons of old actions show the rule of their category, and say nothing about what they followed when the case does not tell.
* Two-factor codes from the 30-second step before or after are accepted (clock drift, typing at the boundary), as the staff sign-in already did.
* Timeouts follow rank and are at most 28 days; a refused join no longer uses up an invite; a locked server is never joined by its id or listed.

## [0.15.5](https://github.com/stoatchat/stoatchat/compare/v0.15.4...v0.15.5) (2026-09-11)


### Bug Fixes

* support mp4 audio mime ([#987](https://github.com/stoatchat/stoatchat/issues/987)) ([6f68a2a](https://github.com/stoatchat/stoatchat/commit/6f68a2a0c845838734847bd43da424a655afd201))

## [0.15.4](https://github.com/stoatchat/stoatchat/compare/v0.15.3...v0.15.4) (2026-09-01)


### Bug Fixes

* don't write quotes to db on last_message_id ([#968](https://github.com/stoatchat/stoatchat/issues/968)) ([eb64f2f](https://github.com/stoatchat/stoatchat/commit/eb64f2f8ff46e193ff7fa854c12b60e86934ef39))

## [0.15.3](https://github.com/stoatchat/stoatchat/compare/v0.15.2...v0.15.3) (2026-08-26)


### Bug Fixes

* discover errors and Removed field ([#944](https://github.com/stoatchat/stoatchat/issues/944)) ([ee4d998](https://github.com/stoatchat/stoatchat/commit/ee4d9986929def624da7b7bee9fcf383f0beb29c))
* Minor fixes for discover routes ([#942](https://github.com/stoatchat/stoatchat/issues/942)) ([509ebbf](https://github.com/stoatchat/stoatchat/commit/509ebbfc1511a2390fe45657f634b183b0bcb573))
* propagate NotFound instead of 500, rename fields to their proper values without affecting database insertion ([509ebbf](https://github.com/stoatchat/stoatchat/commit/509ebbfc1511a2390fe45657f634b183b0bcb573))
* saturate ratelimits to not cause overflows ([#957](https://github.com/stoatchat/stoatchat/issues/957)) ([5b69677](https://github.com/stoatchat/stoatchat/commit/5b696776cbaf4b2fe8a4f4dc5e87efdb30415a00))

## [0.15.2](https://github.com/stoatchat/stoatchat/compare/v0.15.1...v0.15.2) (2026-08-24)


### Features

* implement discover endpoints ([#940](https://github.com/stoatchat/stoatchat/issues/940)) ([7930616](https://github.com/stoatchat/stoatchat/commit/793061685ddd1648dc492dab61a5544ee287c450))


### Bug Fixes

* allow spaces in role colours ([#913](https://github.com/stoatchat/stoatchat/issues/913)) ([e956de9](https://github.com/stoatchat/stoatchat/commit/e956de923ecf5849ba7d5b2b6273e355d4a2f25a))
* audit log test failures when events sort out-of-order ([#933](https://github.com/stoatchat/stoatchat/issues/933)) ([aea1e75](https://github.com/stoatchat/stoatchat/commit/aea1e75ed050440d8fcddcabca0067469e697b53))
* Correct parsing of relative/absolute paths in embed images & fix ImageSize being overwritten ([#930](https://github.com/stoatchat/stoatchat/issues/930)) ([365572c](https://github.com/stoatchat/stoatchat/commit/365572ccd8f1e63f435b53cbda1d550a7ab46d06))
* deleting messages renders channels unackable until a new message is posted ([df2e140](https://github.com/stoatchat/stoatchat/commit/df2e1408a17b39e2ff27452c80e9e1ec05a2d149))
* deleting newest message renders channels unackable until a new message is posted ([#899](https://github.com/stoatchat/stoatchat/issues/899)) ([df2e140](https://github.com/stoatchat/stoatchat/commit/df2e1408a17b39e2ff27452c80e9e1ec05a2d149))
* internal server error when uploading AVIF ([#937](https://github.com/stoatchat/stoatchat/issues/937)) ([8e2eacb](https://github.com/stoatchat/stoatchat/commit/8e2eacb1b06797346d98757e6079d681a124dd0c))
* joined_at is set in ms not seconds ([#900](https://github.com/stoatchat/stoatchat/issues/900)) ([ee0e65f](https://github.com/stoatchat/stoatchat/commit/ee0e65ff57ac7decd04e61480f5ee150e6d8dcc8))
* Make SuppressNotifications actually suppress notifications ([#931](https://github.com/stoatchat/stoatchat/issues/931)) ([99b3ea1](https://github.com/stoatchat/stoatchat/commit/99b3ea140f7c35359d8f54dbf1c5376ce5749beb))
* **permissions:** reposition channel default permissions below role overrides ([#847](https://github.com/stoatchat/stoatchat/issues/847)) ([9ab2ae9](https://github.com/stoatchat/stoatchat/commit/9ab2ae92a667946ac95e2e36cbb101c744ed26ae))
* pin maildev version to 2.2.1 ([#896](https://github.com/stoatchat/stoatchat/issues/896)) ([f354dee](https://github.com/stoatchat/stoatchat/commit/f354dee0ce162158829f478a16a10574838ed6ee))
* pin maildev version to 2.2.1; latest now points to 3.x ([f354dee](https://github.com/stoatchat/stoatchat/commit/f354dee0ce162158829f478a16a10574838ed6ee))
* select specific audit log entries to fix nondeterministic sorting of audited events submitted too quickly ([aea1e75](https://github.com/stoatchat/stoatchat/commit/aea1e75ed050440d8fcddcabca0067469e697b53))
* send relationship info for bots ([#935](https://github.com/stoatchat/stoatchat/issues/935)) ([405d9bd](https://github.com/stoatchat/stoatchat/commit/405d9bdb8c82b84364c6a9efb573bea82d5ec1fb))
* store ratelimits in redis ([#934](https://github.com/stoatchat/stoatchat/issues/934)) ([0bce9b3](https://github.com/stoatchat/stoatchat/commit/0bce9b332e94238698ce9532ce88ca405fa177d5))
* tell klipy that we're discordbot ([#902](https://github.com/stoatchat/stoatchat/issues/902)) ([17036d2](https://github.com/stoatchat/stoatchat/commit/17036d234ca1b1079d93c6f743ba477dbae2b0b9))

## [0.15.1](https://github.com/stoatchat/stoatchat/compare/v0.15.0...v0.15.1) (2026-08-07)


### Features

* moderation API for pulling reported images. ([#880](https://github.com/stoatchat/stoatchat/issues/880)) ([fe331f0](https://github.com/stoatchat/stoatchat/commit/fe331f0dcb1704e0bb99a42f89bd8bfe263069d2))


### Bug Fixes

* Give first instance of a meta tag preference when creating website embed. ([4feeeb1](https://github.com/stoatchat/stoatchat/commit/4feeeb11f316a8ace0f3029424585c541e3310f6))
* Prefer first instance of a meta property in create_website_embed ([#895](https://github.com/stoatchat/stoatchat/issues/895)) ([4feeeb1](https://github.com/stoatchat/stoatchat/commit/4feeeb11f316a8ace0f3029424585c541e3310f6))

## [0.15.0](https://github.com/stoatchat/stoatchat/compare/v0.14.3...v0.15.0) (2026-08-05)


### Features

* add an approximate member count to servers ([#884](https://github.com/stoatchat/stoatchat/issues/884)) ([ccfb5f1](https://github.com/stoatchat/stoatchat/commit/ccfb5f1a62f1be9177ac9c1c09fd18ea8e9c7f18))
* add call event ([#873](https://github.com/stoatchat/stoatchat/issues/873)) ([457c770](https://github.com/stoatchat/stoatchat/commit/457c7709c75060cd8519cc45df3badcfc1b629ea))


### Bug Fixes

* rewrite youtube embedder to use youtube oembed ([#878](https://github.com/stoatchat/stoatchat/issues/878)) ([0369451](https://github.com/stoatchat/stoatchat/commit/03694512b90be90367299c3ebfb072ebbc8a681d))

## [0.14.3](https://github.com/stoatchat/stoatchat/compare/v0.14.2...v0.14.3) (2026-07-22)


### Bug Fixes

* increase cache size to hopefully deduplicate events ([#865](https://github.com/stoatchat/stoatchat/issues/865)) ([44c35bf](https://github.com/stoatchat/stoatchat/commit/44c35bfb519143f1726f7eba9636a87dbef04800))

## [0.14.2](https://github.com/stoatchat/stoatchat/compare/v0.14.1...v0.14.2) (2026-07-18)


### Bug Fixes

* bonfire session deletion redis key is incorrect ([#863](https://github.com/stoatchat/stoatchat/issues/863)) ([764a4dc](https://github.com/stoatchat/stoatchat/commit/764a4dc81c8eab22dfea6a1723f8ffaef1de40bc))

## [0.14.1](https://github.com/stoatchat/stoatchat/compare/v0.14.0...v0.14.1) (2026-07-18)


### Bug Fixes

* presence key was incorrect after other related update ([#861](https://github.com/stoatchat/stoatchat/issues/861)) ([81648db](https://github.com/stoatchat/stoatchat/commit/81648dbf9511b32a6e023d556061bb8b74fe41ed))

## [0.14.0](https://github.com/stoatchat/stoatchat/compare/v0.13.7...v0.14.0) (2026-07-15)


### Features

* add pronouns to user and server members field ([#811](https://github.com/stoatchat/stoatchat/issues/811)) ([ffab236](https://github.com/stoatchat/stoatchat/commit/ffab2369ab5c9b88e007dcd74b91bb48e1988d26))
* Audit Logs ([#466](https://github.com/stoatchat/stoatchat/issues/466)) ([502203d](https://github.com/stoatchat/stoatchat/commit/502203d37c63e486c32e33078e0021bf6390fe97))
* replace tenor with gifbox ([#844](https://github.com/stoatchat/stoatchat/issues/844)) ([59f6e01](https://github.com/stoatchat/stoatchat/commit/59f6e012f827ab08a8e326354bfe6c9026e2cb2d))


### Bug Fixes

* allow removing channel slowmode ([#836](https://github.com/stoatchat/stoatchat/issues/836)) ([21daf3a](https://github.com/stoatchat/stoatchat/commit/21daf3aec693beae55bff51235e5e4b7d90f2362))
* allow true server owner to bypass rank check on channel role-permission overrides ([0af376c](https://github.com/stoatchat/stoatchat/commit/0af376c26b149a5a0286608ebe3869587780a949))
* channel role permissions fail with 400 InvalidOperation for server owners/admins ([#802](https://github.com/stoatchat/stoatchat/issues/802)) ([a7af24b](https://github.com/stoatchat/stoatchat/commit/a7af24b38d0a38d6f04187464a89e67d459d1708))
* channel role permissions fail with InvalidOperation for owners/admins ([a7af24b](https://github.com/stoatchat/stoatchat/commit/a7af24b38d0a38d6f04187464a89e67d459d1708))
* **docs:** update react version ([#842](https://github.com/stoatchat/stoatchat/issues/842)) ([a22378c](https://github.com/stoatchat/stoatchat/commit/a22378c35c2c6c84f8897ce897b9c4df420871d9))
* migration script would panic on fresh installations since it couldn't find invites collection ([#820](https://github.com/stoatchat/stoatchat/issues/820)) ([784f35e](https://github.com/stoatchat/stoatchat/commit/784f35ebfa8568593812683b5fa399ca87af2d6b))
* openapi using old naming ([#777](https://github.com/stoatchat/stoatchat/issues/777)) ([c70459b](https://github.com/stoatchat/stoatchat/commit/c70459b10ce107611b9d478add26db372361baf2))
* point docs favicon to correct location ([#789](https://github.com/stoatchat/stoatchat/issues/789)) ([bebfe34](https://github.com/stoatchat/stoatchat/commit/bebfe349227d8cc555e1b488eb343f2c28b28b88))
* server owner should bypass rank check on channel role-permission overrides ([#805](https://github.com/stoatchat/stoatchat/issues/805)) ([0af376c](https://github.com/stoatchat/stoatchat/commit/0af376c26b149a5a0286608ebe3869587780a949))
* voice system messages and call notifs by fetching participant list ([#846](https://github.com/stoatchat/stoatchat/issues/846)) ([0b53db9](https://github.com/stoatchat/stoatchat/commit/0b53db9921f5ee5992d57a6316cd4e75d241726a))

## [0.13.7](https://github.com/stoatchat/stoatchat/compare/v0.13.6...v0.13.7) (2026-05-21)


### Bug Fixes

* sanitize emoji input to handle variation selectors ([#774](https://github.com/stoatchat/stoatchat/issues/774)) ([2d308e0](https://github.com/stoatchat/stoatchat/commit/2d308e03d58c19f27b5b4d65dc2a15ef20b56190))
* update mention count badge for channel acks ([#769](https://github.com/stoatchat/stoatchat/issues/769)) ([0d9ae50](https://github.com/stoatchat/stoatchat/commit/0d9ae508d9d2199f0e408b8ca634d20489be6f61))

## [0.13.6](https://github.com/stoatchat/stoatchat/compare/v0.13.5...v0.13.6) (2026-05-18)


### Features

* Update FCM payload for android notifications ([#766](https://github.com/stoatchat/stoatchat/issues/766)) ([acbc087](https://github.com/stoatchat/stoatchat/commit/acbc087982e9aeb05cabc5ab4c9b1291f67490ad))
* user slowmode events ([#760](https://github.com/stoatchat/stoatchat/issues/760)) ([af0d8aa](https://github.com/stoatchat/stoatchat/commit/af0d8aad14dc68d88159d0e1c714077d362e21e4))


### Bug Fixes

* include `minio` region as tests need it ([#761](https://github.com/stoatchat/stoatchat/issues/761)) ([298742d](https://github.com/stoatchat/stoatchat/commit/298742dbad4eafae356f976c56b9db23904b0c3a))
* set env var for publishing crates ([#768](https://github.com/stoatchat/stoatchat/issues/768)) ([018afaf](https://github.com/stoatchat/stoatchat/commit/018afaf38f6330d92dad2a68b640c0cb3f6b639a))
* Use proper headers to determine IP when not behind cloudflare ([#764](https://github.com/stoatchat/stoatchat/issues/764)) ([494c8b7](https://github.com/stoatchat/stoatchat/commit/494c8b7cabaae2a51039a7a5b559d5e2e5279554))
* voice ingress crashing due to new Result in AMQP::new_auto() ([#765](https://github.com/stoatchat/stoatchat/issues/765)) ([2871632](https://github.com/stoatchat/stoatchat/commit/2871632382395cb20cbe0047c542d3ac31ff3f03))


### Miscellaneous Chores

* switch to lapin ([#767](https://github.com/stoatchat/stoatchat/issues/767)) ([5b19853](https://github.com/stoatchat/stoatchat/commit/5b1985381ae829a92c80a19e91a414cd9dc4de93))

## [0.13.5](https://github.com/stoatchat/stoatchat/compare/v0.13.4...v0.13.5) (2026-05-17)


### Bug Fixes

* dont panic on hash missing when deleting files ([#755](https://github.com/stoatchat/stoatchat/issues/755)) ([c902077](https://github.com/stoatchat/stoatchat/commit/c902077cf51076fee11712eb732dc8a8f786fc4b))

## [0.13.4](https://github.com/stoatchat/stoatchat/compare/v0.13.3...v0.13.4) (2026-05-16)


### Bug Fixes

* add TLS feature to livekit-api crate ([#753](https://github.com/stoatchat/stoatchat/issues/753)) ([6cfee1f](https://github.com/stoatchat/stoatchat/commit/6cfee1f601c1e084df7c8f1e7a5e8a560d1dd514))

## [0.13.3](https://github.com/stoatchat/stoatchat/compare/v0.13.2...v0.13.3) (2026-05-15)


### Bug Fixes

* don't automatically set up rabbitmq in delta ([#749](https://github.com/stoatchat/stoatchat/issues/749)) ([7647cfc](https://github.com/stoatchat/stoatchat/commit/7647cfc8d93aba99f5faef13eb3d970097540d76))
* don't declare queues which seem to cause the backend to crash in prod ([7647cfc](https://github.com/stoatchat/stoatchat/commit/7647cfc8d93aba99f5faef13eb3d970097540d76))

## [0.13.2](https://github.com/stoatchat/stoatchat/compare/v0.13.1...v0.13.2) (2026-05-11)


### Bug Fixes

* update default exchange to `revolt.default` ([#746](https://github.com/stoatchat/stoatchat/issues/746)) ([fcb8091](https://github.com/stoatchat/stoatchat/commit/fcb8091cd7a00d7f26c798daa33aae4b923b2a8b))

## [0.13.1](https://github.com/stoatchat/stoatchat/compare/v0.13.0...v0.13.1) (2026-05-10)


### Bug Fixes

* amqprs startup bug ([#744](https://github.com/stoatchat/stoatchat/issues/744)) ([1100eaf](https://github.com/stoatchat/stoatchat/commit/1100eaf46f849f2509ae01ac497556ca33bde778))

## [0.13.0](https://github.com/stoatchat/stoatchat/compare/v0.12.1...v0.13.0) (2026-05-08)


### Features

* add embed support for YouTube Shorts ([#734](https://github.com/stoatchat/stoatchat/issues/734)) ([d46c7f7](https://github.com/stoatchat/stoatchat/commit/d46c7f7f3c04524c0639c3e0a122626f8e0b3bf7))
* add emoji rename endpoint ([#714](https://github.com/stoatchat/stoatchat/issues/714)) ([23ad135](https://github.com/stoatchat/stoatchat/commit/23ad1359834bb7d07a460b8678d6a6ebffc73eb0))
* add legal links to root payload ([#733](https://github.com/stoatchat/stoatchat/issues/733)) ([21d8201](https://github.com/stoatchat/stoatchat/commit/21d82018cf84ab0fdd10613d254b9562aea8eea3))
* add role icon support ([#724](https://github.com/stoatchat/stoatchat/issues/724)) ([841985d](https://github.com/stoatchat/stoatchat/commit/841985d3b994df1c6eefab2fc7ecbd77ab22c493))
* Add webhook endpoints for editing and deleting messages ([#682](https://github.com/stoatchat/stoatchat/issues/682)) ([6f3441c](https://github.com/stoatchat/stoatchat/commit/6f3441cf4acac2a8e6e1bf07a279a153b80f7956))
* automatically sanitise usernames on create/update ([#689](https://github.com/stoatchat/stoatchat/issues/689)) ([e937697](https://github.com/stoatchat/stoatchat/commit/e93769786c7669485a659ee471630740d3cea702))
* blacklist private ip ranges and add january domain blocklist ([#731](https://github.com/stoatchat/stoatchat/issues/731)) ([6b41db9](https://github.com/stoatchat/stoatchat/commit/6b41db984bb491b2e58324309cc70d8c14e0b814))
* Rewrite acks ([#741](https://github.com/stoatchat/stoatchat/issues/741)) ([ab5bd47](https://github.com/stoatchat/stoatchat/commit/ab5bd47a39ee889de0b5ae6e7b560620853daead))


### Bug Fixes

* add new_user_hours to configuration limits ([#729](https://github.com/stoatchat/stoatchat/issues/729)) ([279f5d5](https://github.com/stoatchat/stoatchat/commit/279f5d5fd7af2df55902c706859ec07f569cdb1e))
* add reconnection policy to Redis subscriber to prevent ghost state ([#708](https://github.com/stoatchat/stoatchat/issues/708)) ([057f2bb](https://github.com/stoatchat/stoatchat/commit/057f2bb8b359f8b942741a30ff54eeb8fbe3e0b1))
* docker compose file had personal url in it ([#742](https://github.com/stoatchat/stoatchat/issues/742)) ([0719985](https://github.com/stoatchat/stoatchat/commit/0719985ac5636590f91e6f9ec4b68f3eded70c13))
* don't strip ICC from exif ([#735](https://github.com/stoatchat/stoatchat/issues/735)) ([d76a711](https://github.com/stoatchat/stoatchat/commit/d76a71141f3e508f6308ba52fa28eaeb56fb3438))
* dont send notification in fcm ([#721](https://github.com/stoatchat/stoatchat/issues/721)) ([89171e9](https://github.com/stoatchat/stoatchat/commit/89171e9bd0f15711157e78c6eec0fe7b480de93a))
* encode filenames in redirects ([#737](https://github.com/stoatchat/stoatchat/issues/737)) ([9fd7128](https://github.com/stoatchat/stoatchat/commit/9fd7128f800badbd184baf943d4f799e601201e4))
* january ip redirects & domain resolver ([#738](https://github.com/stoatchat/stoatchat/issues/738)) ([356491e](https://github.com/stoatchat/stoatchat/commit/356491e934b274f9e895df883dd63ef0b3123510))
* update message length validation to remove upper limit ([#723](https://github.com/stoatchat/stoatchat/issues/723)) ([ed4fd5e](https://github.com/stoatchat/stoatchat/commit/ed4fd5ebfe6d0ea534a0898da4afdc1f4e2cd6c5))
* use correct response for NoEffect errors ([#732](https://github.com/stoatchat/stoatchat/issues/732)) ([5378cd2](https://github.com/stoatchat/stoatchat/commit/5378cd22b4c7d85f44c31a6af0dda00941b80d5c))

## [0.12.1](https://github.com/stoatchat/stoatchat/compare/v0.12.0...v0.12.1) (2026-04-10)


### Bug Fixes

* add migration to update existing files to be animated ([#705](https://github.com/stoatchat/stoatchat/issues/705)) ([f2c056a](https://github.com/stoatchat/stoatchat/commit/f2c056a1515be493b195f3f5db5886c2ddf36700))
* don't send self dm notifications ([#706](https://github.com/stoatchat/stoatchat/issues/706)) ([f30b729](https://github.com/stoatchat/stoatchat/commit/f30b729ca90d0be6853c57ab4935694e5e59ae56))
* mise start + missing docker image ([#564](https://github.com/stoatchat/stoatchat/issues/564)) ([fb8fe16](https://github.com/stoatchat/stoatchat/commit/fb8fe1655776791421284a6a093e86f0320c258a))
* test failure due to wrong assertion ([#707](https://github.com/stoatchat/stoatchat/issues/707)) ([f81e329](https://github.com/stoatchat/stoatchat/commit/f81e3291bdd57af9ceedb2987b111acc7051d69c))

## [0.12.0](https://github.com/stoatchat/stoatchat/compare/v0.11.5...v0.12.0) (2026-03-28)


### Features

* add bug report template for issue tracking ([#627](https://github.com/stoatchat/stoatchat/issues/627)) ([f777e28](https://github.com/stoatchat/stoatchat/commit/f777e2863c6ca50057c8b5d0a5be14915d287724))
* Add slowmode functionality to text channels ([#680](https://github.com/stoatchat/stoatchat/issues/680)) ([6107f24](https://github.com/stoatchat/stoatchat/commit/6107f242fd3ebaff71a15f9a16330ffbcb4f2d7b))
* Allow restricting server creation to specific users ([#685](https://github.com/stoatchat/stoatchat/issues/685)) ([edfa97d](https://github.com/stoatchat/stoatchat/commit/edfa97db108c9c81828547f98a1db5315cb5ba4a))
* compute thumbhash for images ([#596](https://github.com/stoatchat/stoatchat/issues/596)) ([c2d4369](https://github.com/stoatchat/stoatchat/commit/c2d4369e160f32d79bce0a0b0f14677f89de3669))
* Detect animation in image files for fetch_preview ([#574](https://github.com/stoatchat/stoatchat/issues/574)) ([3fa0abf](https://github.com/stoatchat/stoatchat/commit/3fa0abf47f5f42ddd8ee041fe4c44fbc5ba800c1))
* expose global and user limits in root API response ([#644](https://github.com/stoatchat/stoatchat/issues/644)) ([0b522eb](https://github.com/stoatchat/stoatchat/commit/0b522ebddc17f2e3f792ff5e2347793e9849fa23))
* implement time based message sweep on user ban ([#670](https://github.com/stoatchat/stoatchat/issues/670)) ([98c7b1b](https://github.com/stoatchat/stoatchat/commit/98c7b1b5a5b9fdac5c0ab83be10f0e23114dbfc9))
* load config from env vars ([#576](https://github.com/stoatchat/stoatchat/issues/576)) ([5191bd1](https://github.com/stoatchat/stoatchat/commit/5191bd16b2a905b8409838e34eb0baca96f08580))
* parse message push notification content and replace internal formatting ([#693](https://github.com/stoatchat/stoatchat/issues/693)) ([d1e72ce](https://github.com/stoatchat/stoatchat/commit/d1e72cee42c54e16f4e49af569897528b10a28ca))
* Transfer ownership ([#396](https://github.com/stoatchat/stoatchat/issues/396)) ([735d644](https://github.com/stoatchat/stoatchat/commit/735d644e043793cb86e74aab5b88bb4b8bc17ba2))
* update livekit ([#698](https://github.com/stoatchat/stoatchat/issues/698)) ([f181edc](https://github.com/stoatchat/stoatchat/commit/f181edc8f2ff3ce4b6d48938dfc73931ecfa2279))


### Bug Fixes

* add flag for disabling events instead of commenting them out ([#695](https://github.com/stoatchat/stoatchat/issues/695)) ([a5cd08a](https://github.com/stoatchat/stoatchat/commit/a5cd08a655dece4269f3ac84fa2387ae356709a5))
* add masquerade permission to default direct message settings ([#665](https://github.com/stoatchat/stoatchat/issues/665)) ([ab52569](https://github.com/stoatchat/stoatchat/commit/ab525699bd6663333f0e9fed6d2455e482e6a09f))
* Check for appropriate permission for removing other users avatar ([#657](https://github.com/stoatchat/stoatchat/issues/657)) ([d56135e](https://github.com/stoatchat/stoatchat/commit/d56135e0cbc713884c9378832952f7ad490fa315))
* default video resolution is a non-existent size ([#601](https://github.com/stoatchat/stoatchat/issues/601)) ([0698e11](https://github.com/stoatchat/stoatchat/commit/0698e115e8d003d615e468c4fb9654e6bbc9107f)), closes [#588](https://github.com/stoatchat/stoatchat/issues/588)
* **docs:** Update GitHub links ([#647](https://github.com/stoatchat/stoatchat/issues/647)) ([b830631](https://github.com/stoatchat/stoatchat/commit/b830631bd25a546844b7bdd30386084bb365e4de))
* don't use a bitop for OR ([#676](https://github.com/stoatchat/stoatchat/issues/676)) ([5701b5c](https://github.com/stoatchat/stoatchat/commit/5701b5c18c513f796af365169ceaea372a22638c))
* Fix typo for p256dh in vapid notification flow ([#622](https://github.com/stoatchat/stoatchat/issues/622)) ([a80ad1c](https://github.com/stoatchat/stoatchat/commit/a80ad1cbe58b8af5e45751e51d94d93c1cea1c9f))
* improve generated openapi.json ([#584](https://github.com/stoatchat/stoatchat/issues/584)) ([52ed510](https://github.com/stoatchat/stoatchat/commit/52ed5100c2446e0b261085639e123e7e124cab2c))
* no node state set on channel creation ([#653](https://github.com/stoatchat/stoatchat/issues/653)) ([24d0d2b](https://github.com/stoatchat/stoatchat/commit/24d0d2b7266f6f8a692d0a52704acfecf517674c))
* only show first line on commit messages ([#696](https://github.com/stoatchat/stoatchat/issues/696)) ([91783b9](https://github.com/stoatchat/stoatchat/commit/91783b906697fc85305dee683f7c15dda55f0c50))
* pass &str to Reference ([#697](https://github.com/stoatchat/stoatchat/issues/697)) ([ccda6f5](https://github.com/stoatchat/stoatchat/commit/ccda6f5c53ee043705f7ff6b5f6c393f020781de))
* redis_url vs redis_uri in config ([#666](https://github.com/stoatchat/stoatchat/issues/666)) ([b0b728f](https://github.com/stoatchat/stoatchat/commit/b0b728fb0dbc9ee28360301de1c3ea501bbbff1d))
* replace some links and Revolt mentions to current Stoat ([#515](https://github.com/stoatchat/stoatchat/issues/515)) ([d629e89](https://github.com/stoatchat/stoatchat/commit/d629e89304be2f0011e189293b278f07d346aa7d))
* send push notifications for DM and group messages ([#660](https://github.com/stoatchat/stoatchat/issues/660)) ([52c0d2f](https://github.com/stoatchat/stoatchat/commit/52c0d2f266b76d8975bba2d5e75c62bb30149c45))
* store server id in redis and in room metadata to be able to delete voice state in all scenarios ([#656](https://github.com/stoatchat/stoatchat/issues/656)) ([49c6289](https://github.com/stoatchat/stoatchat/commit/49c628958070e4f0a5edc764d3b48158589219d9))
* uname is missing from crond ([#675](https://github.com/stoatchat/stoatchat/issues/675)) ([dc4438b](https://github.com/stoatchat/stoatchat/commit/dc4438bc3c7b2cad8d442b3cd438afb9ed566a5e))

## [0.11.5](https://github.com/stoatchat/stoatchat/compare/v0.11.4...v0.11.5) (2026-02-17)


### Reverts

* disable user update events ([#593](https://github.com/stoatchat/stoatchat/issues/593)) ([1c98ead](https://github.com/stoatchat/stoatchat/commit/1c98ead69579b4700be0b51c9020bb8402336cc6))

## [0.11.4](https://github.com/stoatchat/stoatchat/compare/v0.11.3...v0.11.4) (2026-02-16)


### Bug Fixes

* add separate config option for redis events replica url ([#590](https://github.com/stoatchat/stoatchat/issues/590)) ([a75e4ea](https://github.com/stoatchat/stoatchat/commit/a75e4eabfc4b34aba7620c82ba77558a32d9e10a))

## [0.11.3](https://github.com/stoatchat/stoatchat/compare/v0.11.2...v0.11.3) (2026-02-13)


### Bug Fixes

* cut presence traffic too while we engineer a new events architecture ([#561](https://github.com/stoatchat/stoatchat/issues/561)) ([1f8ea96](https://github.com/stoatchat/stoatchat/commit/1f8ea963ad742f693f405e6438f1c343c81e6579))

## [0.11.2](https://github.com/stoatchat/stoatchat/compare/v0.11.1...v0.11.2) (2026-02-13)


### Bug Fixes

* cut events traffic while we engineer a new events architecture ([#559](https://github.com/stoatchat/stoatchat/issues/559)) ([a11986b](https://github.com/stoatchat/stoatchat/commit/a11986ba1ad16b672ff1080913a684567d88adbb))

## [0.11.1](https://github.com/stoatchat/stoatchat/compare/v0.11.0...v0.11.1) (2026-02-13)


### Bug Fixes

* bots in multiple voice channel logic ([#544](https://github.com/stoatchat/stoatchat/issues/544)) ([94cb916](https://github.com/stoatchat/stoatchat/commit/94cb916231b9b8befb2e94065917ff40815bec52))

## [0.11.0](https://github.com/stoatchat/stoatchat/compare/v0.10.3...v0.11.0) (2026-02-10)


### Features

* appeal to the almighty Spamhaus ([#524](https://github.com/stoatchat/stoatchat/issues/524)) ([5132270](https://github.com/stoatchat/stoatchat/commit/5132270f2edd6df25ce414daa42ed1b2aa6fa7a9))

## [0.10.3](https://github.com/stoatchat/stoatchat/compare/v0.10.2...v0.10.3) (2026-02-07)


### Bug Fixes

* update `Revolt` -&gt; `Stoat` in email titles/desc. ([#508](https://github.com/stoatchat/stoatchat/issues/508)) ([84483ce](https://github.com/stoatchat/stoatchat/commit/84483cee7af3e5dfa16f7fe13e334c4d9f5abd60))

## [0.10.2](https://github.com/stoatchat/stoatchat/compare/v0.10.1...v0.10.2) (2026-01-25)


### Bug Fixes

* thumbnailification requires rgb8/rgba8 ([#505](https://github.com/stoatchat/stoatchat/issues/505)) ([413aa04](https://github.com/stoatchat/stoatchat/commit/413aa04dcaf8bff3935ed1e5f31432e11a03ce6f))

## [0.10.1](https://github.com/stoatchat/stoatchat/compare/v0.10.0...v0.10.1) (2026-01-25)


### Bug Fixes

* use Rust 1.92.0 for Docker build ([#503](https://github.com/stoatchat/stoatchat/issues/503)) ([98da8a2](https://github.com/stoatchat/stoatchat/commit/98da8a28a0aa2fee4e8eee1d86bd7c49e3187477))

## [0.10.0](https://github.com/stoatchat/stoatchat/compare/v0.9.4...v0.10.0) (2026-01-25)


### Features

* allow kicking members from voice channels ([#495](https://github.com/stoatchat/stoatchat/issues/495)) ([0dc5442](https://github.com/stoatchat/stoatchat/commit/0dc544249825a49c793309edee5ec1838458a6da))
* repository architecture for files crate w. added tests ([#498](https://github.com/stoatchat/stoatchat/issues/498)) ([01ded20](https://github.com/stoatchat/stoatchat/commit/01ded209c62208fc906d6aab9b08c04e860e10ef))


### Bug Fixes

* expose ratelimit headers via cors ([#496](https://github.com/stoatchat/stoatchat/issues/496)) ([a1a2125](https://github.com/stoatchat/stoatchat/commit/a1a21252d0ad58937e41f16e5fb86f96bebd2a51))

## [0.9.4](https://github.com/stoatchat/stoatchat/compare/v0.9.3...v0.9.4) (2026-01-10)


### Bug Fixes

* checkout repo. before bumping lock ([#490](https://github.com/stoatchat/stoatchat/issues/490)) ([b2da2a8](https://github.com/stoatchat/stoatchat/commit/b2da2a858787853be43136fd526a0bd72baf78ef))
* persist credentials for git repo ([#492](https://github.com/stoatchat/stoatchat/issues/492)) ([c674a9f](https://github.com/stoatchat/stoatchat/commit/c674a9fd4e0abbd51569870e4b38074d4a1de03c))

## [0.9.3](https://github.com/stoatchat/stoatchat/compare/v0.9.2...v0.9.3) (2026-01-10)


### Bug Fixes

* pipeline fixes ([#487](https://github.com/stoatchat/stoatchat/issues/487)) ([aeeafeb](https://github.com/stoatchat/stoatchat/commit/aeeafebefc36a43a656cf797c9251ca50292733c))

## [0.9.2](https://github.com/stoatchat/stoatchat/compare/v0.9.1...v0.9.2) (2026-01-10)


### Bug Fixes

* disable publish for services ([#485](https://github.com/stoatchat/stoatchat/issues/485)) ([d13609c](https://github.com/stoatchat/stoatchat/commit/d13609c37279d6a40445dcd99564e5c3dd03bac1))

## [0.9.1](https://github.com/stoatchat/stoatchat/compare/v0.9.0...v0.9.1) (2026-01-10)


### Bug Fixes

* **ci:** pipeline fixes (marked as fix to force release) ([#483](https://github.com/stoatchat/stoatchat/issues/483)) ([303e52b](https://github.com/stoatchat/stoatchat/commit/303e52b476585eea81c33837f1b01506ce387684))

## [0.9.0](https://github.com/stoatchat/stoatchat/compare/v0.8.8...v0.9.0) (2026-01-10)


### Features

* add id field to role ([#470](https://github.com/stoatchat/stoatchat/issues/470)) ([2afea56](https://github.com/stoatchat/stoatchat/commit/2afea56e56017f02de98e67316b4457568ad5b26))
* add ratelimits to gifbox ([1542047](https://github.com/stoatchat/stoatchat/commit/154204742d21cbeff6e2577b00f50b495ea44631))
* include groups and dms in fetch mutuals ([caa8607](https://github.com/stoatchat/stoatchat/commit/caa86074680d46223cebc20f41e9c91c41ec825d))
* include member payload in ServerMemberJoin event ([480f210](https://github.com/stoatchat/stoatchat/commit/480f210ce85271e13d1dac58a5dae08de108579d))
* initial work on tenor gif searching ([b0c977b](https://github.com/stoatchat/stoatchat/commit/b0c977b324b8144c1152589546eb8fec5954c3e7))
* make message lexer use unowned string ([1561481](https://github.com/stoatchat/stoatchat/commit/1561481eb4cdc0f385fbf0a81e4950408050e11f))
* ready payload field customisation ([db57706](https://github.com/stoatchat/stoatchat/commit/db577067948f13e830b5fb773034e9713a1abaff))
* require auth for search ([b5cd5e3](https://github.com/stoatchat/stoatchat/commit/b5cd5e30ef7d5e56e8964fb7c543965fa6bf5a4a))
* trending and categories routes ([5885e06](https://github.com/stoatchat/stoatchat/commit/5885e067a627b8fff1c8ce2bf9e852ff8cf3f07a))
* voice chats v2 ([#414](https://github.com/stoatchat/stoatchat/issues/414)) ([d567155](https://github.com/stoatchat/stoatchat/commit/d567155f124e4da74115b1a8f810062f7c6559d9))


### Bug Fixes

* add license to revolt-parser ([5335124](https://github.com/stoatchat/stoatchat/commit/53351243064cac8d499dd74284be73928fa78a43))
* allow for disabling default features ([65fbd36](https://github.com/stoatchat/stoatchat/commit/65fbd3662462aed1333b79e59155fa6377e83fcc))
* apple music to use original url instead of metadata url ([bfe4018](https://github.com/stoatchat/stoatchat/commit/bfe4018e436a4075bae780dd4d35a9b58315e12f))
* apply uname fix to january and autumn ([8f9015a](https://github.com/stoatchat/stoatchat/commit/8f9015a6ff181d208d9269ab8691bd417d39811a))
* **ci:** publish images under stoatchat and remove docker hub ([d65c1a1](https://github.com/stoatchat/stoatchat/commit/d65c1a1ab3bdc7e5684b03f280af77d881661a3d))
* correct miniz_oxide in lockfile ([#478](https://github.com/stoatchat/stoatchat/issues/478)) ([5d27a91](https://github.com/stoatchat/stoatchat/commit/5d27a91e901dd2ea3e860aeaed8468db6c5f3214))
* correct shebang for try-tag-and-release ([050ba16](https://github.com/stoatchat/stoatchat/commit/050ba16d4adad5d0fb247867aa3e94e3d42bd12d))
* correct string_cache in lockfile ([#479](https://github.com/stoatchat/stoatchat/issues/479)) ([0b178fc](https://github.com/stoatchat/stoatchat/commit/0b178fc791583064bf9ca94b1d39b42d021e1d79))
* don't remove timeouts when a member leaves a server ([#409](https://github.com/stoatchat/stoatchat/issues/409)) ([e635bc2](https://github.com/stoatchat/stoatchat/commit/e635bc23ec857d648d5705e1a3875d7bc3402b0d))
* don't update the same field while trying to remove it ([f4ee35f](https://github.com/stoatchat/stoatchat/commit/f4ee35fb093ca49f0a64ff4b17fd61587df28145)), closes [#392](https://github.com/stoatchat/stoatchat/issues/392)
* github webhook incorrect payload and formatting ([#468](https://github.com/stoatchat/stoatchat/issues/468)) ([dc9c82a](https://github.com/stoatchat/stoatchat/commit/dc9c82aa4e9667ea6639256c65ac8de37a24d1f7))
* implement Serialize to ClientMessage ([dea0f67](https://github.com/stoatchat/stoatchat/commit/dea0f675dde7a63c7a59b38d469f878b7a8a3af4))
* newly created roles should be ranked the lowest ([947eb15](https://github.com/stoatchat/stoatchat/commit/947eb15771ed6785b3dcd16c354c03ded5e4cbe0))
* permit empty `remove` array in edit requests ([6ad3da5](https://github.com/stoatchat/stoatchat/commit/6ad3da5f35f989a2e7d8e29718b98374248e76af))
* preserve order of replies in message ([#447](https://github.com/stoatchat/stoatchat/issues/447)) ([657a3f0](https://github.com/stoatchat/stoatchat/commit/657a3f08e5d652814bbf0647e089ed9ebb139bbf))
* prevent timing out members which have TimeoutMembers permission ([e36fc97](https://github.com/stoatchat/stoatchat/commit/e36fc9738bac0de4f3fcbccba521f1e3754f7ae7))
* relax settings name regex ([3a34159](https://github.com/stoatchat/stoatchat/commit/3a3415915f0d0fdce1499d47a2b7fa097f5946ea))
* remove authentication tag bytes from attachment download ([32e6600](https://github.com/stoatchat/stoatchat/commit/32e6600272b885c595c094f0bc69459250220dcb))
* rename openapi operation ids ([6048587](https://github.com/stoatchat/stoatchat/commit/6048587d348fbca0dc3a9b47690c56df8fece576)), closes [#406](https://github.com/stoatchat/stoatchat/issues/406)
* respond with 201 if no body in requests ([#465](https://github.com/stoatchat/stoatchat/issues/465)) ([24fedf8](https://github.com/stoatchat/stoatchat/commit/24fedf8c4d9cd3160bdec97aa451520f8beaa739))
* swap to using reqwest for query building ([38dd4d1](https://github.com/stoatchat/stoatchat/commit/38dd4d10797b3e6e397fc219e818f379bdff19f2))
* use `trust_cloudflare` config value instead of env var ([cc7a796](https://github.com/stoatchat/stoatchat/commit/cc7a7962a882e1627fcd0bc75858a017415e8cfc))
* use our own result types instead of tenors types ([a92152d](https://github.com/stoatchat/stoatchat/commit/a92152d86da136997817e797c7af8e38731cdde8))
