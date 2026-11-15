package rocks.spotifast.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class DeepLinkTest {

    @Test
    fun ownSchemeOpensEachKind() {
        assertEquals(DeepLink.Track("4uLU6hMCjMI75M1A2tKUQC"), DeepLink.parse("spotifast://track/4uLU6hMCjMI75M1A2tKUQC"))
        assertEquals(DeepLink.Album("6cBdLtktEMZP9m3e0jKytE"), DeepLink.parse("spotifast://album/6cBdLtktEMZP9m3e0jKytE"))
        assertEquals(DeepLink.Playlist("5iwkYfnHAGMEFLiHFFGnP4"), DeepLink.parse("spotifast://playlist/5iwkYfnHAGMEFLiHFFGnP4"))
        assertEquals(DeepLink.Artist("0OdUWJ0sBjDrqHygGUXeCF"), DeepLink.parse("spotifast://artist/0OdUWJ0sBjDrqHygGUXeCF"))
    }

    @Test
    fun spotifyUrisOpenTheSameKinds() {
        assertEquals(DeepLink.Track("4uLU6hMCjMI75M1A2tKUQC"), DeepLink.parse("spotify:track:4uLU6hMCjMI75M1A2tKUQC"))
        assertEquals(DeepLink.Playlist("5iwkYfnHAGMEFLiHFFGnP4"), DeepLink.parse("spotify:playlist:5iwkYfnHAGMEFLiHFFGnP4"))
    }

    @Test
    fun webLinksDropTheQueryAndLocalePrefix() {
        assertEquals(
            DeepLink.Track("4uLU6hMCjMI75M1A2tKUQC"),
            DeepLink.parse("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=abc123"),
        )
        assertEquals(
            DeepLink.Album("6cBdLtktEMZP9m3e0jKytE"),
            DeepLink.parse("https://open.spotify.com/intl-de/album/6cBdLtktEMZP9m3e0jKytE"),
        )
        assertEquals(
            DeepLink.Playlist("5iwkYfnHAGMEFLiHFFGnP4"),
            DeepLink.parse("https://open.spotify.com/user/dom/playlist/5iwkYfnHAGMEFLiHFFGnP4"),
        )
    }

    @Test
    fun otherAddressesAreIgnored() {
        assertNull(DeepLink.parse(null))
        assertNull(DeepLink.parse(""))
        assertNull(DeepLink.parse("https://example.com/track/4uLU6hMCjMI75M1A2tKUQC"))
        assertNull(DeepLink.parse("https://open.spotify.com/"))
        assertNull(DeepLink.parse("https://open.spotify.com/track/"))
        assertNull(DeepLink.parse("spotifast://callback?code=abc"))
        assertNull(DeepLink.parse("spotify:episode:4uLU6hMCjMI75M1A2tKUQC"))
        assertNull(DeepLink.parse("spotifast://album/../../etc"))
    }

    @Test
    fun aTrackLinkCarriesItsSpotifyUri() {
        assertEquals("spotify:track:4uLU6hMCjMI75M1A2tKUQC", DeepLink.Track("4uLU6hMCjMI75M1A2tKUQC").uri)
    }
}
