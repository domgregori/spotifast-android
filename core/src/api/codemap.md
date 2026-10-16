# src/api/

## Responsibility

A REST client and data access layer for the Spotify Web API
(`https://api.spotify.com/v1`): typed wrappers over search, library, user
data, playlists, artists, albums, shows, radio/recommendations and player
commands. It is entirely separate from librespot's streaming protocol; it
drives only the HTTP Web API and the Pathfinder (`api-partner.spotify.com`)
query endpoint. Its job is to turn an authorised account's bearer token into
typed responses, absorbing Spotify's rate limits, capability differences and
evolving response shapes.

Substantiated by: `src/api/mod.rs:1`, `src/api/client.rs:1`, `src/api/gateway.rs`,
`src/api/models.rs:1`.

## Design

Four layers, one file each.

- **Module root** (`mod.rs`) declares the submodules, defines the
  `ApiSource { Shared, Personal }` selector (two grants can be live at once),
  and re-exports the public surface: `ApiClient`, `ApiError`, `NetActivity`,
  `PlayRequest`, `TokenProvider`, `WebTokens`, `AccountId`, `ApiGateway`,
  `Operation`, `PlaylistAccess`, `PlaylistId`, `SessionState`.

- **Transport: `ApiClient`** (`client.rs:362`) is a typed call surface
  (`me`, `search`, `my_playlists`, `playlist_items`, `save`, `play`, ... ~60
  `pub async fn`s) over a single shared transport helper. Every call funnels
  through `send` / `send_body` (`client.rs:520`), which owns bearer auth,
  concurrency limits, retries and error formatting; `get`, `get_optional` and
  `write` (`client.rs:647`-`697`) decode the text body into models.
  Concurrency is bounded by two semaphores: foreground `limiter`
  (`MAX_IN_FLIGHT = 6`) and `background_limiter` (`MAX_BACKGROUND_IN_FLIGHT =
  4`), so a throttled shared app never occupies the personal app's slots.
  Rate-limit policy lives in two cooldown clocks, `cooldown_until` and the
  separate `partner_cooldown_until` (`client.rs:370`), plus a `queue_writes`
  mutex that keeps HTTP 429 queue appends in write order. `TokenProvider`
  (`client.rs:92`) is the auth abstraction; `WebTokens` (`client.rs:110`) is
  its sole variant, holding a `crate::auth::StoredToken` behind a
  `tokio::sync::Mutex` and persisting refreshed grants through a
  `crate::credentials::Lease`. `PlayRequest` (`client.rs:220`) is a builder
  that maps intent to the Web API's `play` body (a lone track must be sent as
  `uris`, not `context_uri`). `NetActivity` (`client.rs:288`) is an
  `AtomicUsize`/`AtomicU64` live traffic counter shared with the interface;
  `unseen` + the `UNSEEN` task-local + `ActivityGuard` (`client.rs:330`-`360`)
  let background requests run without raising the "busy" signal.

- **Routing: `ApiGateway`** (`gateway.rs:186`) is the capability/routing
  facade. It owns two `Session`s (`shared`, `personal`, `gateway.rs:152`),
  each a `SessionState` watch channel plus an `RwLock<Arc<ApiClient>>`. A
  `Session`'s `ApiProfile` (`gateway.rs:57`) sets per-grant request limits
  (`SHARED`: search 20, artist albums 50; `PERSONAL`: 10 / 10). `Operation`
  (`gateway.rs:86`) names the capability being asked for; pure functions
  `plan` (`gateway.rs:118`) choose the `ApiSource`, and `session_serves`
  (`gateway.rs:106`) reports whether the streaming session answers it. The
  state machine is `SessionState { Unavailable, Authorizing, Ready { account } }`
  (`gateway.rs:41`), carried in a `(generation, state)` watch value so a
  sign-out invalidates requests that were waiting. `PlaylistAccess`
  (`gateway.rs:77`) classifies a playlist (Owned / Collaborative / External /
  Unknown) and is cached per `PlaylistId` to route later reads/writes.
  `AccountId` and `PlaylistId` (`gateway.rs:9`, `gateway.rs:22`) are string
  newtypes.

- **Models** (`models.rs`) are serde response shapes, defensively decoded.
  Pagination has three forms: `Page<T>` (`models.rs:31`, with
  `next_offset()`), `CursorPage<T>` + `Cursors` (`models.rs:93`), and the
  null-slot-preserving `PositionedPage<T>` (`models.rs:55`) used for
  positional track endpoints where dropping a null changes every later
  offset. Custom deserializers `skip_nulls` and `null_default`
  (`models.rs:11`, `models.rs:20`) let omitted/null/renamed fields degrade to
  blanks instead of failing a page, and the 2026 endpoint changes (`item`
  vs `track`, `items` vs `tracks`) are accepted alongside classic shapes
  (`Playlist.items_count` with `rename = "items"`, `models.rs:512`). Domain
  types include `Track`, `Album`, `Artist`/`ArtistRef`, `Playlist`, `Owner`,
  `Episode`, `Show`, `PlayableItem`/`PlaylistItem`, the `Saved*` wrappers,
  `PlaybackState`, `Device`/`DeviceList`, `Queue`, `SearchResults`,
  `Recommendations`, and the error envelope `ApiErrorBody`/`ApiErrorDetail`.
  Image helpers `art_target` and `pick_image` (`models.rs:136`, `models.rs:151`)
  choose a cover by target pixel size.

Substantiated by: the file/line anchors above.

## Flow

A concrete catalogue read, e.g. loading an album:

1. `src/backend.rs` maps an `ApiRequest` to an `Operation` with
   `operation_for` (`backend.rs:4233`) and asks the gateway:
   `ApiGateway::client_for(operation)` (`gateway.rs:335`).
2. `client_for` calls `plan(operation, self.personal_ready())` to pick
   `ApiSource::Personal` or `Shared`, takes that `Session`, subscribes to its
   `(generation, SessionState)` watch, and loops: `Ready { .. }` returns the
   session's `Arc<ApiClient>`; `Unavailable` returns `ApiError::NotSignedIn`;
   `Authorizing` awaits `state.changed()`. A generation bump (sign-out) while
   waiting returns `NotSignedIn`.
3. The caller invokes a typed method, e.g. `ApiClient::album(id)`
   (`client.rs:1230`), which calls `get("/albums/{id}", query)`.
4. `get` calls `send` → `send_body` (`client.rs:520`). There:
   `ActivityGuard::begin` raises `NetActivity`, `limiter.acquire()` takes a
   foreground permit, `wait_for_cooldown()` sleeps until any active
   `cooldown_until`, then `provider.access_token()` (`client.rs:152`) yields a
   token: it locks `WebTokens`, returns `SignInExpired` if the credential
   lease is no longer current, else refreshes via `crate::auth::refresh` when
   `needs_refresh()` or when a 401 marked the token rejected, persisting the
   new grant through the lease and spawning the save.
5. A `reqwest` request is built on `crate::http::Http::client()` with
   `.bearer_auth(&token)` and `.query(...)`, and sent.
6. Response transitions: `401 UNAUTHORIZED` on attempt 1 → drop permit,
   `provider.invalidate(&token)`, retry once with a fresh token.
   `429 TOO_MANY_REQUESTS` → parse `Retry-After`, detect
   `QUOTA_EXCEEDED` via `is_quota_exhausted` (→ `ApiError::QuotaExhausted`),
   `extend_cooldown(wait)` bounded by `MAX_RETRY_AFTER = 30s` (queue appends
   keep the full wait), retry up to `RATE_LIMIT_RETRIES = 3`, else
   `ApiError::RateLimited`. `5xx` on a GET attempt 1 sleeps 800ms and retries.
7. Success returns the raw text; `get` deserialises it with
   `serde_json::from_str::<T>` into the requested model (`Album`), mapping
   parse failures to `ApiError::Decode`. Empty/`null` bodies go through
   `get_optional` (204 = nothing playing). `write` treats any status success
   as success and only reads the body when it parses.

Authentication is thus per-source bearer tokens refreshed on demand; rate
limiting is a two-semaphore concurrency cap plus a shared `Retry-After`
cooldown clock, with Pathfinder on its own cooldown.

Substantiated by: `src/api/client.rs:152`, `:362`, `:520`; `src/api/gateway.rs:335`; `src/backend.rs:4233`.

## Integration

**Consumers.** `src/backend.rs` owns the `Arc<ApiGateway>` (`backend.rs:1601`,
constructed at `:1694`), maps `ApiRequest` → `Operation` (`operation_for`),
calls `client_for` then typed `ApiClient` methods, and observes playlist
responses via `ApiGateway::observe_playlists`. `src/model.rs` re-uses
`api::models::*` (e.g. `Page<T>` alias, `Image`). The `src/ui/*` views
consume the models read-only for rendering: `ui/search.rs`, `ui/library.rs`,
`ui/widgets.rs`, `ui/topbar.rs`, `ui/artist.rs`, `ui/settings.rs`,
`ui/prerelease.rs`, `ui/winamp/playlist.rs` (all via
`crate::api::models::*`, notably `pick_image`, `PlayableItem`,
`SearchResults`). `src/made_for_you.rs` uses `ApiError` and
`models::{Image, Owner, Playlist}`. `src/sonos.rs` uses
`client::{PlayRequest, ApiError}` and `models::Device`. Views never call the
network themselves; work runs on the runtime in `backend.rs`.

**Dependencies.** External crates: `reqwest` (HTTP, `StatusCode`, headers),
`serde` / `serde_json` (de- and serialisation), `thiserror` (the `ApiError`
enum), `tokio` (`sync::{Mutex, Semaphore, watch, task_local}`, `time`, `spawn`).
Internal modules: `crate::http::Http` (the shared reqwest client, wrapped so
`ApiError::Network` is produced), `crate::auth` (`StoredToken`, `refresh`,
`TokenEndpointError`), `crate::credentials` (`Lease`, `Grant`, `Error`) for
persisting refreshed grants, `crate::util::uri_kind` (PlayRequest body
shaping), and `crate::i18n::Locale` (`Copyright::kind_label`). Nothing here
depends on egui, eframe, winit or fastframe.

**Portability.** The whole of `src/api/` is portable (a grep for
`egui|fastframe|eframe|winit` returns no matches), so it can move to a future
Android core crate unchanged. Its internal couplings are all portable too
(`http`, `auth`, `credentials`, `util`, `i18n`); only `models::art_target`
takes a `pixels_per_point: f32` UI hint, which is a plain numeric parameter,
not a UI-framework dependency. No desktop coupling exists in this directory.

Substantiated by: `src/backend.rs:16`, `:1694`, `:4233`, `:4304`; `src/model.rs:7`; `src/ui/search.rs:7`; `src/made_for_you.rs:16`; `src/sonos.rs:24`.
