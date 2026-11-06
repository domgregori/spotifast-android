# ListenBrainz feature opportunities

Grounded in what's live on https://listenbrainz.org right now (checked against
the real API/UI, not guessed), scoped to what's realistic to build into the
Android app. Every feature that touches tracks/artists/releases needs the
same underlying trick: ListenBrainz data is keyed by MusicBrainz ID (MBID),
Spotify needs a Spotify id, so each item has to be matched back via
MusicBrainz's own `url-rels` (the same mechanism already shipped for Similar
Artists). That match-back is the main cost/risk on every item below — it's
exact when the MusicBrainz relation exists, and cleanly "not available" when
it doesn't (no fuzzy name guessing).

## Already shipped

- **Scrobbling** — local playback submitted to `POST /1/submit-listens`.
- **Similar artists** — artist context menu, gated on ListenBrainz connection.
  Seed artist resolved to MBID via MusicBrainz reverse URL lookup, similar
  artists from `labs.api.listenbrainz.org/similar-artists`, each result
  lazily matched back to a Spotify artist id on tap.
- **ListenBrainz page** (Your Library → ListenBrainz button, gated on
  connection) — tabbed, currently Profile (username, scrobble toggle, link
  to public profile) and Recent listens (`GET /1/user/{user}/listens`,
  read-only).

## Feasible next

### 1. Weekly Jams / Weekly Exploration tab
**What**: ListenBrainz generates four playlists per user, refreshed every
Monday morning — Weekly Jams (familiar tracks, a comfortable replay mix),
Weekly Exploration (genuine discovery via collaborative filtering), and
archived Last Week's versions of both. This is the real, working equivalent
of Spotify's `/recommendations` endpoint, which 404s for this app's client id.

**Mechanism**: `GET /1/user/{user}/playlists/createdfor` lists them; each is a
real playlist fetchable by MBID. Inspected the raw JSPF live — every track
carries `recording_mbid`, `artist_mbid`, and a `caa_release_mbid` (Cover Art
Archive release id). No Spotify id anywhere in the payload.

**Cost**: ~50 tracks per playlist, each needing its own MusicBrainz
recording→Spotify lookup (same per-item match-back as Similar Artists, just
at the recording level instead of the artist level — unverified whether
recording pages have Spotify url-rels as reliably as artist pages do, worth
a quick live check before committing). `caa_release_mbid` is a free bonus —
real album art straight from Cover Art Archive, no Spotify call needed for
images, useful even for unmatched tracks.

**Effort**: Medium. Natural third tab on the existing ListenBrainz page.

### 2. Richer Stats tab (ListenBrainz-sourced)
**What**: ListenBrainz's own Stats page (`/user/{user}/stats/`) has several
visualizations the app's existing Spotify-backed "Your Stats" screen doesn't:
Daily Activity (hour × day-of-week listening heatmap), Artist Evolution (a
streamgraph of top artists across the week), Music By Decade, Genre Activity
(a bubble/radial chart), Artist Origins (a world map of where listened
artists are from), and an Album Collage. All scoped by This Week/Month/Year/
All-time, same as the existing screen.

**Mechanism**: `GET /1/stats/user/{user}/...` family of endpoints (artists,
releases, recordings, listening-activity, daily-activity — need to confirm
exact paths per chart against the OpenAPI spec before building). No Spotify
matching needed at all for most of these — they're visualizations of the
user's own listening data, not lists of tappable tracks.

**Effort**: Medium-high if building several charts; could ship incrementally
(Daily Activity heatmap first — most novel, no equivalent in the app today).

### 3. Fresh Releases, "For You" mode
**What**: `/explore/fresh-releases/` has an "All" / "For You" toggle — "For
You" filters the global new-releases calendar down to only artists the user
has actually listened to before. This is a genuinely different feed from the
app's existing New Releases screen (which is Spotify's global editorial
`/browse/new-releases`, not personalized).

**Mechanism**: unconfirmed exact endpoint — likely a `created_for`-style
filter on top of `GET /1/explore/fresh-releases/`. Each release still needs
MBID→Spotify matching (at the release/album level this time — not yet tested
live whether MusicBrainz release pages carry Spotify url-rels as reliably as
artist pages do).

**Effort**: Medium, contingent on verifying the release-level match-back
works before committing to it.

### 4. LB Radio prompt playlists (genre radio, stats radio)
**What**: `/explore/lb-radio/` is a query-based playlist generator — prompts
like `tag:(deep house)`, `artist:(Daft Punk)`, `stats:username`, each with an
easy/medium/hard mode trading familiarity for obscurity. The `tag:` entity in
particular fills a real gap: Spotify's genre-seeded recommendations are one
of the endpoints that 404s for this app, so a **genre radio** feature (pick a
genre → get a playlist) has no other path right now.

**Mechanism**: `troi` prompt syntax documented at
troi.readthedocs.io/en/latest/lb_radio.html; likely surfaced via the same
playlist-generation API as Weekly Jams/Exploration under the hood (unverified
— needs the actual LB Radio API endpoint, not just the prompt-syntax docs).
Same per-track match-back cost as #1.

**Effort**: Medium-high — the prompt syntax is flexible but building a good
UI on top of it (vs. just hardcoding a "genre radio" entity:tag prompt) is
the real design work, not the API call.

### Lower priority
- **Loved/Hated (Taste tab)** — surfacing the heart/X feedback the app could
  already submit via `POST /1/feedback/recording-feedback`; mostly a vanity
  view, nice-to-have rather than filling a gap.
- **Pins** — pinning a listen with a note; a social/personal-journal feature,
  not core to what the app currently does.
- **Similar Users** — needs substantially more listening history to populate
  (empty on every account checked); not useful yet.

## Suggested order

1. Weekly Jams / Weekly Exploration tab — biggest gap it fills (real
   recommendations, where Spotify's own endpoint is dead), and the
   match-back mechanism is already proven.
2. Daily Activity heatmap on a new Stats tab — most novel chart, no Spotify
   matching needed at all, lowest risk.
3. Genre radio via LB Radio's `tag:` entity — fills the other dead Spotify
   endpoint (genre-seeded recommendations), but needs the real API confirmed
   first (prompt docs don't specify the actual request shape).
4. Fresh Releases "For You" — nice complement to the existing New Releases
   screen, but lowest priority since it's the most speculative on the
   release-level match-back actually working well.
