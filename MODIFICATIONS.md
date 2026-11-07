# Modifications

This file records the changes Zrzur made to the upstream software, as required by
section 5(a) of the GNU Affero General Public License. Upstream: stoatchat/stoatchat at
`483c1779ee1cf6079b9ff294a8607fae4af633e8` (2026-10-19). Latest summary: 2026-11-07.

Size of the change against that commit, before publication cleaning: 671 files changed, 123114 insertions(+), 6080 deletions(-).

- Product identity changed to Zrzur; internal names (delta, bonfire, autumn, january, `revolt-*` crates, `Revolt.toml`) kept for compatibility.
- New: GIF service changes, Discover, Designs, paid features (Feather, Wings), trust and safety reports and retention, interactions and bot platform, OAuth, account data export, privacy settings, age rules, session limits, staff API routes.
- Security: webhook and proxy request hardening, fail-closed configuration, no default upstream hosts.

Individual files generally carry no upstream header, so this file is the record of changes.
Git history was not carried over; this repository starts with a single initial commit.
