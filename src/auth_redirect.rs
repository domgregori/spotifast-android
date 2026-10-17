//! The desktop's OAuth redirect capture: a loopback `TcpListener` on
//! `http://127.0.0.1:<port>/login`, wrapped as `spotifast_core::auth`'s
//! `AuthRedirectHandler`. The Android front end supplies its own handler that
//! completes a channel from `onNewIntent` instead.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use spotifast_core::auth::{AuthRedirect, AuthRedirectHandler, Grant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::watch;

const REDIRECT_PATH: &str = "/login";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A browser sends its request line at once; anything slower or longer is
/// not the redirect, and must not hold the listener while the user waits.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_LINE_LIMIT: u64 = 8 * 1024;

/// Reserve the callback port before offering or opening browser sign-in.
fn listen_for_redirect(port: u16) -> Result<TcpListener> {
    let address: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = std::net::TcpListener::bind(address).with_context(|| {
        format!("Cannot start Spotify sign-in: port {port} is in use or unavailable. Close the app using this port, then try again.")
    })?;
    listener.set_nonblocking(true)?;
    Ok(TcpListener::from_std(listener)?)
}

/// Listens on a reserved port for Spotify's redirect and returns the
/// authorization code.
///
/// Ends early when `cancel` flips to true (the user gave up) or after ten
/// minutes.
async fn wait_for_code_on_listener(
    listener: TcpListener,
    expected_state: &str,
    mut cancel: watch::Receiver<bool>,
) -> Result<String> {
    let deadline = tokio::time::sleep(LOGIN_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        let (mut stream, _) = tokio::select! {
            accepted = listener.accept() => accepted.context("redirect listener failed")?,
            _ = cancel.changed() => {
                if *cancel.borrow() { bail!("sign-in cancelled"); }
                continue;
            }
            _ = &mut deadline => bail!("sign-in timed out; try again"),
        };

        let mut reader = BufReader::new(&mut stream).take(REQUEST_LINE_LIMIT);
        let mut request_line = String::new();
        match tokio::time::timeout(REQUEST_TIMEOUT, reader.read_line(&mut request_line)).await {
            Ok(Ok(_)) if request_line.ends_with('\n') => {}
            _ => continue,
        }
        let outcome = parse_request_line(&request_line, expected_state);
        let (status, body) = match &outcome {
            Ok(_) => ("200 OK", success_page()),
            Err(error) => ("400 Bad Request", failure_page(&error.to_string())),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        match outcome {
            Ok(code) => return Ok(code),
            Err(error) => {
                // A favicon request or a stale tab is not the redirect; keep waiting.
                log::debug!("ignored request on the redirect listener: {error}");
                continue;
            }
        }
    }
}

fn parse_request_line(line: &str, expected_state: &str) -> Result<String> {
    let target = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed request"))?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != REDIRECT_PATH {
        bail!("unexpected redirect path");
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = urlencoding::decode(value)
            .map(|value| value.into_owned())
            .unwrap_or_else(|_| value.to_string());
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => error = Some(value),
            _ => {}
        }
    }
    if error.is_some() {
        bail!("Spotify refused the sign-in. Try again.");
    }
    if state.as_deref() != Some(expected_state) {
        bail!("state mismatch");
    }
    code.ok_or_else(|| anyhow!("Spotify did not return an authorization code"))
}

fn page(title: &str, heading: &str, body: &str, accent: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title>\
<style>:root{{color-scheme:dark}}body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#0f1114;color:#e8eaed;font-family:Inter,system-ui,sans-serif}}\
main{{max-width:28rem;padding:2.5rem;border-radius:1.25rem;background:#181b20;box-shadow:0 20px 60px rgba(0,0,0,.5);text-align:center}}\
.mark{{width:64px;height:64px;border-radius:50%;background:{accent};display:grid;place-items:center;margin:0 auto 1.25rem}}\
.mark svg{{width:30px;height:30px;fill:#0f1114}}h1{{font-size:1.4rem;margin:.25rem 0 .5rem}}p{{color:#a5adba;line-height:1.5;margin:0}}</style>\
<main><div class=\"mark\"><svg viewBox=\"0 0 24 24\"><path d=\"M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z\"/></svg></div>\
<h1>{heading}</h1><p>{body}</p></main><script>setTimeout(function(){{window.close()}},1500)</script></html>"
    )
}

fn success_page() -> String {
    page(
        "Signed in to Spotifast",
        "You're signed in",
        "You can close this tab and go back to Spotifast.",
        "#1ed760",
    )
}

fn failure_page(reason: &str) -> String {
    page(
        "Sign-in failed",
        "Sign-in didn't complete",
        &format!("{reason}. Return to Spotifast and try again."),
        "#f5717f",
    )
}

/// The desktop loopback redirect, verbatim from the pre-split `auth.rs`.
#[derive(Default)]
pub struct LoopbackRedirectHandler;

impl AuthRedirectHandler for LoopbackRedirectHandler {
    fn redirect_uri(&self, grant: &Grant) -> String {
        format!("http://127.0.0.1:{}{REDIRECT_PATH}", grant.redirect_port)
    }

    fn prepare(&self, grant: &Grant) -> Result<Box<dyn AuthRedirect + Send>> {
        let listener = listen_for_redirect(grant.redirect_port)?;
        Ok(Box::new(LoopbackRedirect { listener }))
    }
}

struct LoopbackRedirect {
    listener: TcpListener,
}

impl AuthRedirect for LoopbackRedirect {
    fn wait_for_code(
        self: Box<Self>,
        expected_state: &str,
        cancel: watch::Receiver<bool>,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
        let expected_state = expected_state.to_owned();
        Box::pin(async move {
            wait_for_code_on_listener(self.listener, &expected_state, cancel).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn redirect_listener_can_retry_after_port_is_released() {
        // A test running alongside can be handed the port between its
        // release and the retry, so that case tries again on another port.
        let (listener, port) = (0..10)
            .find_map(|_| {
                let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let port = occupied.local_addr().unwrap().port();
                let error = listen_for_redirect(port).unwrap_err().to_string();
                assert!(error.contains(&port.to_string()));
                assert!(error.contains("try again"));
                drop(occupied);
                Some((listen_for_redirect(port).ok()?, port))
            })
            .expect("a released port can be listened on again");
        let (cancel, receiver) = watch::channel(false);
        let waiting = tokio::spawn(async move {
            wait_for_code_on_listener(listener, "state", receiver).await
        });
        let mut browser = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        browser
            .write_all(b"GET /login?code=test-code&state=state HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let code = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(code, "test-code");
        drop(cancel);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_or_endless_request_does_not_hold_the_redirect_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut stalled = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stalled.write_all(b"GET /login?code=").await.unwrap();
        let mut endless = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        endless.write_all(&[b'a'; 16 * 1024]).await.unwrap();
        let mut browser = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        browser
            .write_all(b"GET /login?code=test-code&state=state HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let (_cancel, receiver) = watch::channel(false);
        let code = wait_for_code_on_listener(listener, "state", receiver)
            .await
            .unwrap();
        assert_eq!(code, "test-code");
    }

    #[test]
    fn redirect_errors_never_include_authorization_response_contents() {
        for request in [
            "GET /login?error=dummy-authorization-secret&state=ok HTTP/1.1",
            "GET /dummy-authorization-secret HTTP/1.1",
        ] {
            let error = parse_request_line(request, "ok").unwrap_err();
            assert!(!format!("{error:?} {error}").contains("dummy-authorization-secret"));
        }
    }

    #[test]
    fn request_line_parsing() {
        let code =
            parse_request_line("GET /login?code=abc%20d&state=s1 HTTP/1.1\r\n", "s1").unwrap();
        assert_eq!(code, "abc d");
        assert!(parse_request_line("GET /login?code=abc&state=other HTTP/1.1", "s1").is_err());
        assert!(parse_request_line("GET /favicon.ico HTTP/1.1", "s1").is_err());
        assert!(
            parse_request_line("GET /login?error=access_denied&state=s1 HTTP/1.1", "s1").is_err()
        );
    }
}
