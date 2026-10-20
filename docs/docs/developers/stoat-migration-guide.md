# Stoat migration guide (historical upstream)

:::warning

This page documents **upstream** Revolt → Stoat endpoint history for developers migrating old clients.  
It is **not** the Zarzour production endpoint map. Zarzour clients must use local or Zarzour-owned hosts (see [Endpoints](./endpoints.md)).

:::

## Endpoint changes (Stoat platform history)

| Service    | Old URL                             | New URL (Stoat)                         |
| ---------- | ----------------------------------- | --------------------------------------- |
| **API**    | `https://api.revolt.chat`           | `https://stoat.chat/api`                |
|            | `https://app.revolt.chat/api`       | `https://stoat.chat/api`                |
|            | `https://revolt.chat/api`           | No equivalent                           |
| **Events** | `wss://ws.revolt.chat`              | `wss://stoat.chat/events`               |
|            | `wss://app.revolt.chat/events`      | `wss://stoat.chat/events`               |
|            | `wss://revolt.chat/events`          | No equivalent                           |
| **Files**  | `https://autumn.revolt.chat`        | `https://cdn.stoatusercontent.com`      |
|            | `https://cdn.revoltusercontent.com` | `https://cdn.stoatusercontent.com`      |
| **Proxy**  | `https://jan.revolt.chat`           | `https://external.stoatusercontent.com` |
| **Voice**  | `https://vortex.revolt.chat`        | Superseded by Voice Chats v2            |

Zarzour planned hosts (when DNS is verified): `api.zarzour.cc`, `ws.zarzour.cc`, `cdn.zarzour.cc`, `voice.zarzour.cc`.
