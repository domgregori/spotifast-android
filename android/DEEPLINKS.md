# Spotifast links

The app answers three spellings of the same link. All of them open the same
screens, ignore query strings (`?si=...`), and play a track straight away.

| Opens | Spotifast | Spotify | Web |
| --- | --- | --- | --- |
| Playlist | `spotifast://playlist/<id>` | `spotify:playlist:<id>` | `https://open.spotify.com/playlist/<id>` |
| Album | `spotifast://album/<id>` | `spotify:album:<id>` | `https://open.spotify.com/album/<id>` |
| Artist | `spotifast://artist/<id>` | `spotify:artist:<id>` | `https://open.spotify.com/artist/<id>` |
| Track | `spotifast://track/<id>` | `spotify:track:<id>` | `https://open.spotify.com/track/<id>` |

`<id>` is Spotify's own id, the part after the last `/` or `:` in a shared
link. The web form also accepts Spotify's localised prefix
(`https://open.spotify.com/intl-de/album/<id>`) and the older
`https://open.spotify.com/user/<user>/playlist/<id>` shape.

Anything else is ignored: other hosts, other kinds (episodes, shows), and the
authorization callback `spotifast://callback`, which stays private to
sign-in.

## How the app hears them

`AndroidManifest.xml` declares the `spotifast` scheme for the four kinds, the
`spotify` scheme, and `http`/`https` for `open.spotify.com`. `MainActivity`
passes every VIEW intent to `SpotifyCore.receiveDeepLink`, which parses it
with `DeepLink.parse` and keeps it until the app is connected; `ConnectedApp`
then closes any open panel and opens the screen, or plays the track. A link
that arrives while the app is running is handled the same way through
`onNewIntent`.

When the Spotify app is also installed, Android asks which app should open a
`spotify:` or `open.spotify.com` link the first time; choosing Spotifast
(and "always") makes it swallow them from then on. The app does not declare
`autoVerify`, because it cannot host `assetlinks.json` for `open.spotify.com`;
there is no silent takeover.

## Testing a link

```sh
adb shell am start -a android.intent.action.VIEW \
  -n rocks.spotifast.android/.MainActivity \
  -d "spotifast://album/6cBdLtktEMZP9m3e0jKytE"
adb shell am start -a android.intent.action.VIEW \
  -n rocks.spotifast.android/.MainActivity \
  -d "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=abc"
```

Without `-n`, Android resolves the link through the manifest, which is also
how another app or the browser sends it.
