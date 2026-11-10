# Changelog

## Unreleased

### Features

- Polls in messages (behind `features.polls_enabled`, off by default): `DataMessageSend.poll` takes a question (1–300 characters), 2–10 answers (1–55 characters, optional emoji the author may use) and a duration of 1, 4, 8 or 24 hours, 3 days, 1 or 2 weeks, single or multiple choice. Posting one needs Send Messages and Send Embeds (no new permission bit). `PUT`/`DELETE /channels/{c}/messages/{m}/polls/answers/{a}/votes/@me` vote and take back a vote (React, the channel's write rules and the age rules apply; people only, own rate-limit bucket `poll_votes`, 10 per channel); `GET …/answers/{a}/voters` pages through who chose an answer for anyone who can read the message; `POST …/polls/end` ends it early (author, or Manage Messages). Readers get counts and their own choice (`poll.me`), never voter lists; gateway events `MessagePollVoteAdd`/`MessagePollVoteRemove` (counts), `MessagePollChoice` (to the voter only) and `MessagePollEnd`. crond closes polls past their end within a minute and a `poll_ended` system reply names the winner; a vote on an expired poll is refused (`PollEnded`, 409) and closes it at once. Poll messages can't be edited; deleting one deletes its votes; forwarding sends the question and answers as text. Votes are one conditional write each (a vote racing the close loses with `PollEnded`; on multiple choice only the chosen answer changes); events carry `version` and `total`. AutoMod reads the question and answers. A closed poll always gets its reply (crond retries; the reply id is fixed per poll). Push previews use the poll text. Data export: own polls with counts, not voters, and `poll_votes.json`. Migration `PENDING_POLL_INDEX` (index `polls_to_close`).
- Scheduled server events (behind `features.server_events_enabled`, off by default): `/servers/{id}/events` to create, edit, start, end, cancel and delete events (Manage Server, or the event's creator for everything but delete), place = voice channel, text channel or an external place, optional cover (`banners` tag), weekly / every two weeks / monthly repeat (the next one is made when one completes). Members see only events whose place they can view. "Interested" marks with a public count and a paged list. crond starts and completes events on time; at the start interested members get a push and the join message channel gets an `event_started` system message when everyone can see the place. Gateway: `ServerEventCreate`, `ServerEventUpdate`, `ServerEventDelete`, `ServerEventInterest`. Audit log: create, edit, cancel, delete. New error `TooManyServerEvents` (100 upcoming per server). Indexes in migration `PENDING_SERVER_EVENT_INDEXES`. Review round: every manager action needs Manage Server (not while timed out; the creator has no own rights); the start is announced in the system channel only when no default, role, member or category setting and no mature flag could keep anyone from the place; repeats are computed from the series start in the creator's IANA time zone (stored as `time_zone`, read from the system zone database), so local times survive daylight saving changes and monthly dates clamp per month (31 Jan, 28/29 Feb, 31 Mar); the next occurrence has an id derived from the one before and is written before the one before ends, and crond gives finished series without a next one theirs; cancelling or deleting one occurrence keeps the series (`cancel?series=true` stops it); crond cancels events whose channel is gone and managers can still see and delete them; members who leave lose their marks; external places refuse non-web links; each occurrence has its own use of the cover, released on delete; `ServerEventInterest` carries the place channel.

### Fixes

- voice-ingress: a join is announced as soon as the voice state is stored. Before, a failed LiveKit permission sync stopped the `VoiceChannelJoin` event, so people already in the room never saw the newcomer (they saw them only after a reload). The sync now retries once; if it still fails, the user is taken out of the room instead of keeping LiveKit's default permissions.

## Zrzur 26.0.0 (unreleased)

Zrzur's first release, built on Stoat 0.15.5 (below).

### Features

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
