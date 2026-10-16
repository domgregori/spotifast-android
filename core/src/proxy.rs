//! Proxy configuration types, shared by the desktop binary and the Android
//! library. The resolved proxy policy and its parsing are portable; the
//! localized label and the `Settings`-scoped helpers stay in the desktop
//! crate's `settings` module.

use serde::{Deserialize, Serialize};

/// A stored proxy password, bound to one network endpoint and username.
///
/// No `Debug` implementation: the password is usable, and the username is
/// private. It lives here (not in the credential `Store`) because the resolved
/// proxy policy produces and consumes it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyPassword {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl ProxyPassword {
    /// True when the host, port, and password round-trip through the proxy
    /// parser unchanged: the stored endpoint is still usable.
    pub fn valid(&self) -> bool {
        if self.password.is_empty() {
            return false;
        }
        ManualProxy::parse(
            ManualKind::Http,
            &self.host,
            &self.port.to_string(),
            &self.username,
            &self.password,
        )
        .ok()
        .and_then(|manual| manual.password_record())
        .as_ref()
            == Some(self)
    }
}

/// How outbound HTTP traffic reaches the network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// No proxy, ignoring environment and OS proxy settings.
    Off,
    /// Environment variables and, on macOS and Windows, the OS proxy.
    #[default]
    System,
    /// A configured HTTP proxy.
    Http,
    /// A configured SOCKS5 proxy.
    Socks,
}

impl ProxyMode {
    pub const ALL: [ProxyMode; 4] = [Self::Off, Self::System, Self::Http, Self::Socks];

    pub fn is_manual(self) -> bool {
        matches!(self, Self::Http | Self::Socks)
    }
}

pub fn proxy_mode_is_system(mode: &ProxyMode) -> bool {
    *mode == ProxyMode::System
}

pub fn infer_legacy_proxy_mode(proxy: &str) -> ProxyMode {
    let raw = proxy.trim();
    if raw.is_empty() {
        return ProxyMode::System;
    }
    let scheme = raw.split("://").next().unwrap_or("").to_ascii_lowercase();
    match scheme.as_str() {
        "socks" | "socks5" | "socks5h" => ProxyMode::Socks,
        _ => ProxyMode::Http,
    }
}

fn validate_host(host: &str) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("enter a host".into());
    }
    let host = host
        .split_once("://")
        .map_or(host, |(_, rest)| rest)
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.is_empty() {
        return Err("enter a host".into());
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(host.to_string());
    }
    if is_hostname(host) {
        return Ok(host.to_string());
    }
    Err("the host must be a hostname or IP address".into())
}

fn is_hostname(host: &str) -> bool {
    if host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn validate_port(port: &str) -> Result<u16, String> {
    let port = port.trim();
    if port.is_empty() {
        return Err("enter a port".into());
    }
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("the port must be a number between 1 and 65535".into());
    }
    match port.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err("the port must be a number between 1 and 65535".into()),
    }
}

pub fn split_legacy_address(raw: &str) -> (String, String) {
    let raw = raw.trim();
    let rest = raw.split_once("://").map_or(raw, |(_, rest)| rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
    if let Some(inner) = rest.strip_prefix('[')
        && let Some((host, port)) = inner.split_once("]:")
    {
        return (host.to_string(), port.trim().to_string());
    }
    if let Some((host, port)) = rest.rsplit_once(':')
        && !host.contains(':')
    {
        return (host.to_string(), port.trim().to_string());
    }
    (rest.to_string(), String::new())
}

/// The resolved proxy policy used by the HTTP client and local playback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProxyConfig {
    /// Invalid saved settings block requests until corrected in the interface.
    Invalid(String),
    Off,
    #[default]
    System,
    Http(ManualProxy),
    Socks(ManualProxy),
}

impl ProxyConfig {
    pub fn password_record(&self) -> Option<ProxyPassword> {
        match self {
            Self::Http(manual) | Self::Socks(manual) => manual.password_record(),
            _ => None,
        }
    }

    pub fn restore_password(&mut self, saved: &ProxyPassword) -> bool {
        match self {
            Self::Http(manual) | Self::Socks(manual) => manual.restore_password(saved),
            _ => false,
        }
    }

    /// Librespot's CONNECT client only understands unauthenticated,
    /// plaintext HTTP proxies. Never give it a credential-bearing URL: the
    /// upstream client logs that URL at info level and does not send proxy
    /// authentication in either of its HTTP paths.
    pub fn librespot_url(&self) -> Option<reqwest::Url> {
        match self {
            Self::Http(manual) => manual.librespot_url(),
            Self::System => system_http_proxy(),
            Self::Invalid(_) | Self::Off | Self::Socks(_) => None,
        }
    }
}

/// The HTTP proxy System mode would hand librespot, if it can resolve one.
/// SOCKS system proxies are ignored: the engine cannot use them.
fn system_http_proxy() -> Option<reqwest::Url> {
    let matcher = hyper_util::client::proxy::matcher::Matcher::from_system();
    let dest = http::Uri::from_static("https://apresolve.spotify.com");
    let intercept = matcher.intercept(&dest)?;
    librespot_system_proxy(&intercept)
}

fn librespot_system_proxy(
    intercept: &hyper_util::client::proxy::matcher::Intercept,
) -> Option<reqwest::Url> {
    if intercept.basic_auth().is_some() || intercept.raw_auth().is_some() {
        return None;
    }
    http_proxy_from_uri(intercept.uri())
}

fn http_proxy_from_uri(uri: &http::Uri) -> Option<reqwest::Url> {
    match uri.scheme_str() {
        Some("http") => reqwest::Url::parse(&uri.to_string()).ok(),
        _ => None,
    }
}

#[derive(Clone, Copy)]
pub enum ManualKind {
    Http,
    Socks,
}

/// A configured HTTP or SOCKS5 endpoint.
#[derive(Clone, PartialEq, Eq)]
pub struct ManualProxy {
    endpoint: reqwest::Url,
    username: String,
    password: String,
}

impl std::fmt::Debug for ManualProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManualProxy")
            .field("url", &self.redacted())
            .finish()
    }
}

impl ManualProxy {
    pub fn password_record(&self) -> Option<ProxyPassword> {
        if self.password.is_empty() {
            return None;
        }
        Some(ProxyPassword {
            host: self.endpoint.host_str()?.to_string(),
            port: self.endpoint.port()?,
            username: self.username.clone(),
            password: self.password.clone(),
        })
    }

    pub fn restore_password(&mut self, saved: &ProxyPassword) -> bool {
        if self.endpoint.host_str() != Some(saved.host.as_str())
            || self.endpoint.port() != Some(saved.port)
            || self.username != saved.username
        {
            return false;
        }
        self.password.clone_from(&saved.password);
        true
    }

    pub fn parse(
        kind: ManualKind,
        host: &str,
        port: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, String> {
        let host = validate_host(host)?;
        let port = validate_port(port)?;
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let scheme = match kind {
            ManualKind::Http => "http",
            // Resolve Spotify hostnames at the proxy, so SOCKS mode does not
            // leak DNS queries or fail behind a proxy-only network.
            ManualKind::Socks => "socks5h",
        };
        let endpoint = reqwest::Url::parse(&format!("{scheme}://{host}:{port}"))
            .map_err(|error| format!("not a proxy address: {error}"))?;
        Ok(Self {
            endpoint,
            username: username.to_string(),
            password: password.to_string(),
        })
    }

    /// A reqwest proxy with credentials applied separately so build errors
    /// cannot echo a password.
    pub fn reqwest_proxy(&self) -> Result<reqwest::Proxy, reqwest::Error> {
        let mut proxy = reqwest::Proxy::all(self.endpoint.clone())?;
        if !self.username.is_empty() || !self.password.is_empty() {
            proxy = proxy.basic_auth(&self.username, &self.password);
        }
        Ok(proxy)
    }

    fn librespot_url(&self) -> Option<reqwest::Url> {
        (self.username.is_empty() && self.password.is_empty()).then(|| self.endpoint.clone())
    }

    /// The credential-free endpoint, for logs and the debugger.
    pub fn redacted(&self) -> reqwest::Url {
        self.endpoint.clone()
    }

    /// The configured password, exposed for the settings layer's round-trip.
    pub fn password(&self) -> &str {
        &self.password
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_only_accepts_plain_http_proxy_urls() {
        assert_eq!(ProxyConfig::Off.librespot_url(), None);
        let http_uri: http::Uri = "http://127.0.0.1:8080".parse().unwrap();
        let https_uri: http::Uri = "https://127.0.0.1:8080".parse().unwrap();
        let socks_uri: http::Uri = "socks5://127.0.0.1:1080".parse().unwrap();
        assert!(http_proxy_from_uri(&http_uri).is_some());
        assert!(http_proxy_from_uri(&https_uri).is_none());
        assert!(http_proxy_from_uri(&socks_uri).is_none());
    }

    #[test]
    fn proxy_credentials_stay_opaque_and_out_of_urls() {
        let proxy = ManualProxy::parse(
            ManualKind::Http,
            "127.0.0.1",
            "7890",
            "alice/name",
            " secret/with spaces ",
        )
        .unwrap();
        let redacted = proxy.redacted();
        assert!(redacted.username().is_empty());
        assert_eq!(redacted.password(), None);
        assert_eq!(proxy.username, "alice/name");
        assert_eq!(proxy.password, " secret/with spaces ");
        assert_eq!(ProxyConfig::Http(proxy.clone()).librespot_url(), None);
        let debug = format!("{proxy:?}");
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn system_proxy_authentication_and_tls_endpoints_stay_out_of_librespot() {
        let authenticated = hyper_util::client::proxy::matcher::Matcher::builder()
            .all("http://alice:secret@127.0.0.1:8080")
            .build();
        let destination = http::Uri::from_static("https://apresolve.spotify.com");
        let intercept = authenticated.intercept(&destination).unwrap();
        assert!(librespot_system_proxy(&intercept).is_none());

        let tls = hyper_util::client::proxy::matcher::Matcher::builder()
            .all("https://127.0.0.1:8080")
            .build();
        let intercept = tls.intercept(&destination).unwrap();
        assert!(librespot_system_proxy(&intercept).is_none());
    }
}

