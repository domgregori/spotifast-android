//! Small helpers shared across the application.



/// `3:45` for track lengths, `1:02:03` past an hour.
pub fn format_duration_ms(ms: u32) -> String {
    let total = ms / 1000;
    let hours = total / 3600;
    let minutes = (total / 60) % 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(character);
    }
    out
}

/// Tears the id out of `spotify:track:abc` and friends.
pub fn uri_id(uri: &str) -> Option<&str> {
    uri.rsplit(':').next().filter(|id| !id.is_empty())
}

pub fn uri_kind(uri: &str) -> Option<&str> {
    let mut parts = uri.split(':');
    parts.next()?;
    parts.next()
}

/// Spotify's radio station seeded by a song, playlist, album, or artist.
pub fn station_uri(seed: &str) -> Option<String> {
    let kind = uri_kind(seed)?;
    let id = uri_id(seed)?;
    (seed == format!("spotify:{kind}:{id}")
        && matches!(kind, "track" | "playlist" | "album" | "artist"))
    .then(|| format!("spotify:station:{kind}:{id}"))
}

/// The song, playlist, album, or artist a radio station is seeded by.
pub fn station_seed(station: &str) -> Option<String> {
    let seed = format!("spotify:{}", station.strip_prefix("spotify:station:")?);
    station_uri(&seed).is_some().then_some(seed)
}

pub fn open_spotify_url(uri: &str) -> Option<String> {
    let kind = uri_kind(uri)?;
    let id = uri_id(uri)?;
    Some(format!("https://open.spotify.com/{kind}/{id}"))
}

/// The menu-bar shape for macOS: the circle with the play triangle punched
/// out. macOS template images use only the alpha channel and paint the
/// shape themselves, black in a light menu bar and white in a dark one.
pub fn tray_template_rgba(size: usize) -> Vec<u8> {
    let mut rgba = mark_rgba(size, false);
    for pixel in rgba.as_chunks_mut::<4>().0 {
        // The triangle is the dark colour; make it a hole instead.
        if pixel[1] < 128 {
            pixel[3] = 0;
        }
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
    }
    rgba
}

/// The mark rasterised to pixels: the window icon, the trays and the logo
/// drawn in the app (`theme::logo`) all use this one picture.
///
/// It is the polished disc of `packaging/icons` at every size: a darker rim
/// around a lit face.
pub fn app_icon_rgba(size: usize) -> Vec<u8> {
    mark_rgba(size, true)
}

/// Mixes two colours, `t` of the way from `a` to `b`.
fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// How far `p` is from the triangle `a`, `b`, `c`: zero inside it.
fn triangle_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    let edge = |a: (f32, f32), b: (f32, f32)| {
        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
        let (px, py) = (p.0 - a.0, p.1 - a.1);
        let along = ((px * ex + py * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        let (dx, dy) = (px - ex * along, py - ey * along);
        ((dx * dx + dy * dy).sqrt(), ex * py - ey * px)
    };
    let (d1, s1) = edge(a, b);
    let (d2, s2) = edge(b, c);
    let (d3, s3) = edge(c, a);
    let inside = (s1 >= 0.0 && s2 >= 0.0 && s3 >= 0.0) || (s1 <= 0.0 && s2 <= 0.0 && s3 <= 0.0);
    if inside { 0.0 } else { d1.min(d2).min(d3) }
}

/// The mark on a 128-unit square, as `packaging/icons/spotifast.svg` draws
/// it: a disc of radius 62 and a play triangle with corners rounded by 5,
/// set a little left of its box so it looks centred. `polished` adds the
/// darker rim, the lit face and the bright edge between them.
fn mark_rgba(size: usize, polished: bool) -> Vec<u8> {
    const GREEN: [f32; 3] = [30.0, 215.0, 96.0];
    const INK: [f32; 3] = [11.0, 14.0, 12.0];
    let mut rgba = vec![0u8; size * size * 4];
    // The disc keeps two pixels of margin, so its edge is never clipped.
    let unit = (size as f32 / 2.0 - 2.0) / 62.0;
    let origin = size as f32 / 2.0 - 64.0 * unit;
    for y in 0..size {
        for x in 0..size {
            // The pixel's centre in the mark's own units.
            let u = (x as f32 + 0.5 - origin) / unit;
            let v = (y as f32 + 0.5 - origin) / unit;
            let distance = ((u - 64.0).powi(2) + (v - 64.0).powi(2)).sqrt();
            let coverage = ((62.0 - distance) * unit + 0.5).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let mut colour = if polished {
                let rim = mix([24.0, 192.0, 85.0], [12.0, 138.0, 58.0], (v - 2.0) / 124.0);
                let lit = (v - 8.0) / 112.0;
                let face = if lit < 0.55 {
                    mix([92.0, 240.0, 149.0], GREEN, lit / 0.55)
                } else {
                    mix(GREEN, [21.0, 182.0, 80.0], (lit - 0.55) / 0.45)
                };
                let on_face = ((54.4 - distance) * unit + 0.5).clamp(0.0, 1.0);
                let mut colour = mix(rim, face, on_face);
                // The bright edge where the face meets the rim: light at
                // the top, shaded at the bottom.
                let edge = (1.0 - (distance - 55.0).abs() / 0.9).clamp(0.0, 1.0);
                let (tone, strength) = if lit < 0.5 {
                    ([217.0, 255.0, 232.0], 1.0 - 1.3 * lit)
                } else {
                    ([10.0, 110.0, 46.0], 0.35 + 1.1 * (lit - 0.5))
                };
                colour = mix(colour, tone, edge * strength.clamp(0.0, 1.0));
                colour
            } else {
                GREEN
            };
            let triangle = triangle_distance((u, v), (49.2, 43.5), (49.2, 84.5), (86.1, 64.0));
            let glyph = ((5.0 - triangle) * unit + 0.5).clamp(0.0, 1.0);
            colour = mix(colour, INK, glyph);
            let index = (y * size + x) * 4;
            rgba[index] = colour[0].round() as u8;
            rgba[index + 1] = colour[1].round() as u8;
            rgba[index + 2] = colour[2].round() as u8;
            rgba[index + 3] = (coverage * 255.0) as u8;
        }
    }
    rgba
}

/// Strips the HTML Spotify embeds in playlist descriptions.
pub fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(character),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&#x2F;", "/")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// The temporary files a replacement (fastframe-file) left in `dir`, which
/// a finished or failed one never does.
pub fn temporary_files(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".tmp"))
                .collect()
        })
        .unwrap_or_default()
}

/// The volume as a percentage, for the interface and Connect.
pub fn volume_to_percent(volume: u16) -> u8 {
    ((u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u8
}

/// The 0..=65535 volume a percentage sets, for the mixer and the engine.
pub fn percent_to_volume(percent: u8) -> u16 {
    ((u32::from(percent.min(100)) * u32::from(u16::MAX)) / 100) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let index = (y * size + x) * 4;
        [
            rgba[index],
            rgba[index + 1],
            rgba[index + 2],
            rgba[index + 3],
        ]
    }

    /// The icon wears the polished disc at every size, and the tray
    /// template keeps its punched-out triangle.
    #[test]
    fn the_icon_is_polished_at_every_size() {
        // #given the icon at a dock size and at a tray size
        let (large, small) = (app_icon_rgba(128), app_icon_rgba(32));

        // #then both have a darker rim around a lighter face
        let rim = pixel(&large, 128, 64, 6);
        let face = pixel(&large, 128, 64, 20);
        assert!(
            face[1] > rim[1],
            "face {face:?} should be lighter than rim {rim:?}"
        );
        assert!(pixel(&small, 32, 16, 6)[1] > pixel(&small, 32, 16, 2)[1]);
        // #and a lit top fading to a deeper bottom
        let low = pixel(&large, 128, 64, 108);
        assert!(face[1] > low[1]);

        // #and both carry the dark triangle, a little right of centre
        for (icon, size) in [(&large, 128), (&small, 32)] {
            let centre = pixel(icon, size, size / 2 + size / 16, size / 2);
            assert!(centre[1] < 40, "triangle missing at {size}: {centre:?}");
        }

        // #and the corners stay clear
        assert_eq!(pixel(&large, 128, 1, 1)[3], 0);

        // #and the menu-bar template is the disc with the triangle cut out
        let template = tray_template_rgba(44);
        assert_eq!(pixel(&template, 44, 24, 22)[3], 0);
        assert_eq!(pixel(&template, 44, 8, 22), [0, 0, 0, 255]);
    }

    #[test]
    fn radio_stations_map_to_their_seeds_and_back() {
        for kind in ["track", "playlist", "album", "artist"] {
            let seed = format!("spotify:{kind}:4uLU6hMCjMI75M1A2tKUQC");
            let station = station_uri(&seed).expect("a station");
            assert_eq!(
                station,
                format!("spotify:station:{kind}:4uLU6hMCjMI75M1A2tKUQC")
            );
            assert_eq!(station_seed(&station).as_deref(), Some(seed.as_str()));
        }
        for seed in [
            "spotify:show:abc",
            "spotify:episode:abc",
            "spotify:user:me:collection",
            "spotify:track:",
            "track:abc",
        ] {
            assert_eq!(station_uri(seed), None, "{seed}");
        }
        assert_eq!(station_seed("spotify:playlist:abc"), None);
    }

    #[test]
    fn durations_and_counts() {
        assert_eq!(format_duration_ms(225_000), "3:45");
        assert_eq!(format_duration_ms(3_723_000), "1:02:03");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(12), "12");
    }

    #[test]
    fn uris() {
        assert_eq!(uri_id("spotify:track:abc"), Some("abc"));
        assert_eq!(uri_kind("spotify:playlist:x"), Some("playlist"));
        assert_eq!(
            open_spotify_url("spotify:album:z").as_deref(),
            Some("https://open.spotify.com/album/z")
        );
    }

    #[test]
    fn html_is_stripped() {
        assert_eq!(
            strip_html("Hi <a href=\"x\">there</a> &amp; you"),
            "Hi there & you"
        );
        assert_eq!(strip_html("ONE&#x2F;TWO&#x2F;THREE"), "ONE/TWO/THREE");
    }
}
