---
sidebar_position: 1
---

# Endpoints

Configure clients against **your** Zarzour instance. Do not use public Stoat/Revolt hosts as defaults.

## Local development (this tree)

Values from `backend/Revolt.toml` in a typical local layout:

| Role | URL |
| --- | --- |
| HTTP API (Delta) | `http://127.0.0.1:14702` |
| Events (Bonfire WebSocket) | `ws://127.0.0.1:14703` |
| Files (Autumn) | `http://127.0.0.1:14704` |
| Proxy (January) | `http://127.0.0.1:14705` |
| LiveKit (configured public URL) | `ws://127.0.0.1:14706` |

Web Vite during `mise dev` is commonly `http://127.0.0.1:5173`.

## Planned production (owner DNS required)

These are configuration targets only — **not** verified live in Phase 01:

| Role | Planned URL |
| --- | --- |
| App | `https://app.zarzour.cc` |
| API | `https://api.zarzour.cc` |
| WebSocket | `wss://ws.zarzour.cc` |
| CDN / files | `https://cdn.zarzour.cc` |
| Voice | `https://voice.zarzour.cc` |

OpenAPI publication for Zarzour is **pending**. Do not fetch `https://stoat.chat/api/openapi.json` for Zarzour tooling.
