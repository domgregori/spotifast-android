//! Spotify sign-in with the Authorization Code + PKCE flow.
//!
//! Three independent grants may exist because Spotify treats them differently:
//!
//! - The shared and optional personal **Web API grants** use separate
//!   registered application identities, protected entries, and request sessions.
//! - The **playback grant** uses Spotify's desktop client identity, the one
//!   librespot streams with. Its access token is exchanged once for a
//!   reusable credential kept in the platform credential store.
//!
//! The browser does the password entry; this process only ever sees the
//! one-time authorization code that Spotify sends back. How the code is
//! captured is the front end's [`AuthRedirectHandler`]: the desktop listens
//! on a loopback port, Android completes a channel from `onNewIntent`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

/// Spotify's own desktop client identity, the one librespot streams with.
pub const PLAYBACK_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const PLAYBACK_REDIRECT_PORT: u16 = 8898;

/// The public Web API application shared by spotify-player, ncspot, and
/// Omarchy Spotify.
pub const DEFAULT_WEB_CLIENT_ID: &str = "d420a117a32841c2b3474932e49fb54b";
pub const WEB_REDIRECT_PORT: u16 = 8989;

/// Playback: what librespot needs to stream and join Spotify Connect.
pub const PLAYBACK_SCOPES: &[&str] = &["streaming"];

/// Web API: what visible features use, plus `user-read-private` for the
/// plan (Free or Premium), which decides whether local playback is offered
/// at all; no email.
pub const WEB_SCOPES: &[&str] = &[
    "ugc-image-upload",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read-collaborative",
    "playlist-read-private",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify-playback-state",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// Refresh this long before the access token expires.
const REFRESH_MARGIN: Duration = Duration::from_secs(90);

/// One OAuth application identity and what it is asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub client_id: String,
    pub redirect_port: u16,
    pub scopes: &'static [&'static str],
}

impl Grant {
    pub fn playback() -> Self {
        Self {
            client_id: PLAYBACK_CLIENT_ID.to_string(),
            redirect_port: PLAYBACK_REDIRECT_PORT,
            scopes: PLAYBACK_SCOPES,
        }
    }

    pub fn shared_web_api() -> Self {
        Self {
            client_id: DEFAULT_WEB_CLIENT_ID.to_string(),
            redirect_port: WEB_REDIRECT_PORT,
            scopes: WEB_SCOPES,
        }
    }

    pub fn personal_web_api(client_id: &str) -> Result<Self> {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            bail!("a personal Spotify Client ID is required");
        }
        Ok(Self {
            client_id: client_id.to_string(),
            redirect_port: WEB_REDIRECT_PORT,
            scopes: WEB_SCOPES,
        })
    }
}

/// Captures Spotify's redirect and returns the authorization code.
///
/// The desktop listens on a loopback port; Android completes a channel when
/// Kotlin's `onNewIntent` hands in the captured code.
pub trait AuthRedirectHandler: Send + Sync {
    /// The URI Spotify sends the code to for this grant. The desktop's is a
    /// loopback URL; Android's is a custom scheme.
    fn redirect_uri(&self, grant: &Grant) -> String;

    /// Reserve the redirect mechanism before the browser is offered, so a
    /// busy loopback port reports before the browser opens.
    fn prepare(&self, grant: &Grant) -> Result<Box<dyn AuthRedirect + Send>>;
}

/// A reserved redirect mechanism awaiting the code.
pub trait AuthRedirect: Send {
    fn wait_for_code(
        self: Box<Self>,
        expected_state: &str,
        cancel: watch::Receiver<bool>,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>;
}

/// A redirect handler that never captures a code, for headless runs and for
/// tests that bypass browser sign-in.
#[derive(Default)]
pub struct NoRedirect;

impl AuthRedirectHandler for NoRedirect {
    fn redirect_uri(&self, grant: &Grant) -> String {
        format!("http://127.0.0.1:{}/login", grant.redirect_port)
    }

    fn prepare(&self, _grant: &Grant) -> Result<Box<dyn AuthRedirect + Send>> {
        Err(anyhow!("no redirect handler"))
    }
}

/// A started sign-in: the URL to open and the secrets needed to finish it.
#[derive(Clone, Debug)]
pub struct Flow {
    pub verifier: String,
    pub state: String,
    pub url: String,
}

fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

pub fn begin(grant: Grant, redirect: &dyn AuthRedirectHandler) -> Flow {
    let verifier = random_token(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(18);
    let show_dialog = if grant.client_id == PLAYBACK_CLIENT_ID {
        "&show_dialog=true"
    } else {
        ""
    };
    let redirect_uri = redirect.redirect_uri(&grant);
    let url = format!(
        "{AUTHORIZE_URL}?client_id={}&response_type=code&redirect_uri={}&code_challenge_method=S256&code_challenge={challenge}&state={state}&scope={}{show_dialog}",
        grant.client_id,
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&grant.scopes.join(" "))
    );
    Flow {
        verifier,
        state,
        url,
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn exchange_code(
    http: &reqwest::Client,
    grant: &Grant,
    code: &str,
    verifier: &str,
    redirect: &dyn AuthRedirectHandler,
) -> Result<TokenResponse> {
    let redirect_uri = redirect.redirect_uri(grant);
    token_request(
        http,
        &[
            ("client_id", grant.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", verifier),
        ],
    )
    .await
    .map_err(|error| anyhow!("{error}"))
}

pub async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> std::result::Result<TokenResponse, TokenEndpointError> {
    token_request(
        http,
        &[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum TokenEndpointError {
    /// Spotify refused the grant itself; only the browser can mint a new one.
    #[error("Spotify rejected the token request ({status}): {detail}")]
    Rejected { status: u16, detail: String },
    /// The endpoint could not answer; the grant may still be valid.
    #[error("token request failed: {0}")]
    Unreachable(String),
}

async fn token_request(
    http: &reqwest::Client,
    form: &[(&str, &str)],
) -> std::result::Result<TokenResponse, TokenEndpointError> {
    let response = http
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(|error| TokenEndpointError::Unreachable(error.to_string()))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    decode_token_response(status, &text)
}

fn decode_token_response(
    status: reqwest::StatusCode,
    text: &str,
) -> std::result::Result<TokenResponse, TokenEndpointError> {
    if status.is_client_error() {
        let value = serde_json::from_str::<serde_json::Value>(text).ok();
        // OAuth descriptions and malformed bodies may contain usable grants.
        // Report only known classifications, never authorization response text.
        let detail = match value.as_ref().and_then(|value| value["error"].as_str()) {
            Some("invalid_grant") => "The grant expired or was revoked. Sign in again.",
            Some("invalid_client") => "Check the Spotify application's Client ID.",
            Some("invalid_scope") => "Spotify did not accept the requested permissions.",
            Some("access_denied") => "Spotify authorization was declined.",
            _ => "Sign in again and check the Spotify application settings.",
        };
        return Err(TokenEndpointError::Rejected {
            status: status.as_u16(),
            detail: detail.into(),
        });
    }
    if !status.is_success() {
        return Err(TokenEndpointError::Unreachable(format!(
            "Spotify answered {status}"
        )));
    }
    serde_json::from_str(text).map_err(|_| {
        TokenEndpointError::Unreachable("Spotify returned an unreadable token response".into())
    })
}

/// The Web API grant, serialized only inside protected credential storage.
/// Deliberately has no Debug implementation: it contains usable secrets.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredToken {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: u64,
    #[serde(default)]
    pub scope: String,
}

impl StoredToken {
    pub fn from_response(
        client_id: &str,
        response: TokenResponse,
        previous_refresh: Option<&str>,
    ) -> Result<Self> {
        let refresh_token = response
            .refresh_token
            .or_else(|| previous_refresh.map(str::to_string))
            .ok_or_else(|| anyhow!("Spotify did not return a refresh token"))?;
        Ok(Self {
            client_id: client_id.to_string(),
            access_token: response.access_token,
            refresh_token,
            expires_at: now_secs() + response.expires_in.unwrap_or(3600),
            scope: response.scope.unwrap_or_default(),
        })
    }

    pub fn needs_refresh(&self) -> bool {
        now_secs() + REFRESH_MARGIN.as_secs() >= self.expires_at
    }

    pub fn expired(&self) -> bool {
        now_secs() >= self.expires_at
    }

    /// Whether the grant covers every scope in `scopes`. A grant cannot be
    /// widened by a refresh, only by the browser.
    pub fn has_scopes(&self, scopes: &[&str]) -> bool {
        let granted: Vec<&str> = self.scope.split_whitespace().collect();
        scopes.iter().all(|scope| granted.contains(scope))
    }

    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    // Legacy file fixtures only. Production persistence lives in credentials.rs.
    #[cfg(test)]
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let temporary = path.with_extension("json.tmp");
        write_private(&temporary, text.as_bytes())?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }

    // Legacy file fixtures only. Production persistence lives in credentials.rs.
    #[cfg(test)]
    pub fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    // Legacy file fixtures only. Production persistence lives in credentials.rs.
    #[cfg(test)]
    pub fn migrate_legacy(legacy: &Path, shared: &Path, personal: &Path) -> Result<()> {
        let Some(token) = Self::load(legacy) else {
            return Ok(());
        };
        let target = if token.client_id == DEFAULT_WEB_CLIENT_ID {
            shared
        } else {
            personal
        };
        if let Some(existing) = Self::load(target) {
            if existing != token {
                return Ok(());
            }
        } else {
            token.save(target)?;
        }
        Self::remove(legacy);
        Ok(())
    }
}

#[cfg(test)]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.flush()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn token_errors_never_include_authorization_response_contents() {
        for (status, body) in [
            (
                400,
                r#"{"error":"invalid_grant","error_description":"dummy-refresh-secret"}"#,
            ),
            (400, r#"{"error":"dummy-refresh-secret"}"#),
            (503, "dummy-refresh-secret"),
            (
                200,
                r#"{"access_token":"dummy-refresh-secret","expires_in":"dummy-refresh-secret"}"#,
            ),
        ] {
            let error =
                super::decode_token_response(reqwest::StatusCode::from_u16(status).unwrap(), body)
                    .unwrap_err();
            assert!(!format!("{error:?} {error}").contains("dummy-refresh-secret"));
        }
    }

    use super::*;

    /// A loopback redirect URI for the PKCE tests, without a real listener.
    #[derive(Default)]
    struct TestRedirect;

    impl AuthRedirectHandler for TestRedirect {
        fn redirect_uri(&self, grant: &Grant) -> String {
            format!("http://127.0.0.1:{}/login", grant.redirect_port)
        }

        fn prepare(&self, _grant: &Grant) -> Result<Box<dyn AuthRedirect + Send>> {
            unimplemented!("the PKCE tests never capture a redirect")
        }
    }

    #[test]
    fn old_web_grants_require_image_upload_consent() {
        let token = StoredToken {
            scope: WEB_SCOPES
                .iter()
                .filter(|scope| **scope != "ugc-image-upload")
                .copied()
                .collect::<Vec<_>>()
                .join(" "),
            ..Default::default()
        };
        assert!(!token.has_scopes(WEB_SCOPES));
        let renewed = StoredToken {
            scope: WEB_SCOPES.join(" "),
            ..token
        };
        assert!(renewed.has_scopes(WEB_SCOPES));
    }

    #[test]
    fn begin_produces_valid_pkce_material() {
        let redirect = TestRedirect;
        let flow = begin(Grant::shared_web_api(), &redirect);
        assert!(flow.verifier.len() >= 43);
        assert!(flow.url.contains("code_challenge_method=S256"));
        assert!(flow.url.contains(&format!("state={}", flow.state)));
        assert!(
            flow.url
                .contains(&format!("client_id={DEFAULT_WEB_CLIENT_ID}"))
        );
        assert!(flow.url.contains("8989"));
        assert!(!flow.url.contains("show_dialog=true"));
        let playback = begin(Grant::playback(), &redirect);
        assert!(
            playback
                .url
                .contains(&format!("client_id={PLAYBACK_CLIENT_ID}"))
        );
        assert!(playback.url.contains("8898"));
        assert!(playback.url.contains("show_dialog=true"));
    }

    #[test]
    fn playback_scopes_conform_to_spotify_streaming_contract() {
        assert_eq!(PLAYBACK_SCOPES, &["streaming"]);
        let playback = Grant::playback();
        assert_eq!(playback.scopes, vec!["streaming".to_string()]);
        let personal = Grant::personal_web_api("test-id").unwrap();
        let flow = begin(personal, &TestRedirect);
        assert!(!flow.url.contains("show_dialog=true"));
    }

    #[test]
    fn personal_client_id_is_validated_at_the_boundary() {
        assert_eq!(Grant::personal_web_api("  abc ").unwrap().client_id, "abc");
        assert!(Grant::personal_web_api("  ").is_err());
    }

    #[test]
    fn stored_token_round_trips_and_tracks_expiry() {
        let response = TokenResponse {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_in: Some(3600),
            scope: Some("x".into()),
        };
        let token = StoredToken::from_response("id", response, None).unwrap();
        assert!(!token.needs_refresh());
        assert!(token.has_scopes(&["x"]));
        assert!(!token.has_scopes(&["x", "y"]));
        let expired = StoredToken {
            expires_at: now_secs() + 10,
            ..token.clone()
        };
        assert!(expired.needs_refresh());
        let dir = std::env::temp_dir().join(format!("spotifast-token-{}", std::process::id()));
        let path = dir.join("token.json");
        token.save(&path).unwrap();
        assert!(StoredToken::load(&path) == Some(token));
        StoredToken::remove(&path);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_tokens_move_to_the_matching_session() {
        let dir = std::env::temp_dir().join(format!(
            "spotifast-token-migration-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let legacy = dir.join("legacy.json");
        let shared = dir.join("shared.json");
        let personal = dir.join("personal.json");
        for (client_id, target) in [
            (DEFAULT_WEB_CLIENT_ID, &shared),
            ("personal-client", &personal),
        ] {
            let token = StoredToken {
                client_id: client_id.into(),
                access_token: "access".into(),
                refresh_token: "refresh".into(),
                expires_at: now_secs() + 3600,
                ..StoredToken::default()
            };
            token.save(&legacy).unwrap();
            StoredToken::migrate_legacy(&legacy, &shared, &personal).unwrap();
            assert!(StoredToken::load(target) == Some(token));
            assert!(!legacy.exists());
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
