//! Loopback HTTP server for original media (Android).
//!
//! Android's WebView cannot play video from `vpmedia://`: it applies a request's `Range`
//! to intercepted responses itself, by skipping bytes in the body, so every range but
//! `bytes=0-` fails, and each intercepted body is copied whole into a Java array. There
//! the viewer loads media from `http://127.0.0.1:<port>/<token>/<id>` instead, answered by
//! the same code as `vpmedia://` (`media::answer`). The server binds to loopback only, and
//! the random token in the path keeps other apps on the device out.
//!
//! Only Android starts it; it is compiled everywhere so it is tested everywhere.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

use tauri::http::Response;
use tauri::{AppHandle, Runtime};

static BASE: OnceLock<String> = OnceLock::new();

/// Failed accepts in a row after which the server listens again.
const REBIND_AFTER: u32 = 5;

/// Largest request head accepted.
const MAX_HEAD: usize = 16 * 1024;

/// Starts the server once. Returns the base URL (`http://127.0.0.1:<port>/<token>`).
pub fn start<R: Runtime>(app: &AppHandle<R>) -> std::io::Result<String> {
    if let Some(base) = BASE.get() {
        return Ok(base.clone());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let token = vp_crypto::random_hex(16).map_err(std::io::Error::other)?;
    let base = format!("http://127.0.0.1:{}/{token}", listener.local_addr()?.port());
    let app = app.clone();
    let prefix = format!("/{token}/");
    let addr = listener.local_addr()?;
    std::thread::spawn(move || {
        let mut listener = Some(listener);
        let mut failures = 0u32;
        loop {
            let Some(l) = &listener else {
                // Listen again on the same port: the WebView keeps using the same base URL.
                std::thread::sleep(Duration::from_millis(200));
                match TcpListener::bind(addr) {
                    Ok(fresh) => {
                        crate::applog::info("media-server", "listening again");
                        listener = Some(fresh);
                        failures = 0;
                    }
                    Err(e) if failures == REBIND_AFTER => {
                        crate::applog::warn("media-server", format!("cannot listen again: {e}"));
                        failures += 1;
                    }
                    Err(_) => {}
                }
                continue;
            };
            match l.accept() {
                Ok((stream, _)) => {
                    failures = 0;
                    let (app, prefix) = (app.clone(), prefix.clone());
                    std::thread::spawn(move || {
                        let _ = serve_connection(stream, &prefix, |id, range, fit| crate::media::answer(&app, id, range, fit));
                    });
                }
                Err(e) => {
                    // One failed accept is usually a client that gave up; a run of them means
                    // the socket went bad (it can while the app is in the background).
                    failures += 1;
                    if failures == 1 {
                        crate::applog::warn("media-server", format!("accept failed: {e}"));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                    if failures >= REBIND_AFTER {
                        listener = None; // closed before binding the port again
                    }
                }
            }
        }
    });
    Ok(BASE.get_or_init(|| base).clone())
}

/// The base URL if the server runs.
pub fn base() -> Option<String> {
    BASE.get().cloned()
}

/// A parsed request: method, path and `Range` header.
struct Head {
    method: String,
    path: String,
    range: Option<String>,
    keep_alive: bool,
}

fn read_head(reader: &mut impl BufRead) -> std::io::Result<Option<Head>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path), version) = (parts.next(), parts.next(), parts.next()) else {
        return Err(std::io::ErrorKind::InvalidData.into());
    };
    let mut head = Head {
        method: method.to_string(),
        path: path.to_string(),
        range: None,
        keep_alive: version != Some("HTTP/1.0"),
    };
    let mut size = line.len();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        size += n;
        if n == 0 || size > MAX_HEAD {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let l = line.trim_end();
        if l.is_empty() {
            return Ok(Some(head));
        }
        if let Some((name, value)) = l.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("range") {
                head.range = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("connection") {
                head.keep_alive = !value.eq_ignore_ascii_case("close");
            }
        }
    }
}

/// Answers requests on one connection (keep-alive) with `answer(id, range)`.
fn serve_connection(
    stream: TcpStream,
    prefix: &str,
    answer: impl Fn(Option<i64>, Option<String>, Option<u32>) -> Response<Vec<u8>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some(head) = read_head(&mut reader)? {
        let response = match head.path.strip_prefix(prefix) {
            Some(rest) if head.method == "GET" || head.method == "HEAD" => {
                let (path, query) = rest.split_once('?').map_or((rest, None), |(p, q)| (p, Some(q)));
                answer(path.parse::<i64>().ok(), head.range, crate::media::fit_param(query))
            }
            Some(_) => Response::builder().status(405).body(Vec::new()).unwrap(),
            None => Response::builder().status(404).body(Vec::new()).unwrap(),
        };
        write_response(&mut writer, &response, head.method == "HEAD", head.keep_alive)?;
        if !head.keep_alive {
            break;
        }
    }
    Ok(())
}

fn write_response(w: &mut impl Write, r: &Response<Vec<u8>>, head_only: bool, keep_alive: bool) -> std::io::Result<()> {
    let status = r.status();
    let mut out = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), status.canonical_reason().unwrap_or(""));
    for (name, value) in r.headers() {
        if name != tauri::http::header::CONTENT_LENGTH {
            out.push_str(&format!("{}: {}\r\n", name, value.to_str().unwrap_or("")));
        }
    }
    out.push_str(&format!("Content-Length: {}\r\n", r.body().len()));
    out.push_str("Cache-Control: no-store\r\n");
    out.push_str(if keep_alive { "Connection: keep-alive\r\n\r\n" } else { "Connection: close\r\n\r\n" });
    w.write_all(out.as_bytes())?;
    if !head_only {
        w.write_all(r.body())?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn run(requests: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_connection(stream, "/tok/", |id, range, fit| {
                let body = format!("id={id:?} range={range:?} fit={fit:?}").into_bytes();
                Response::builder().status(206).header("Content-Range", "bytes 0-1/2").body(body).unwrap()
            })
            .unwrap();
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(requests.as_bytes()).unwrap();
        let mut out = String::new();
        client.read_to_string(&mut out).unwrap();
        server.join().unwrap();
        out
    }

    #[test]
    fn answers_ranges_and_keeps_alive() {
        let out = run(
            "GET /tok/27?v=1 HTTP/1.1\r\nHost: x\r\nRange: bytes=5-\r\n\r\nGET /tok/3?fit=4096 HTTP/1.1\r\nConnection: close\r\n\r\n",
        );
        assert!(out.starts_with("HTTP/1.1 206 Partial Content\r\n"), "{out}");
        assert!(out.contains("id=Some(27) range=Some(\"bytes=5-\") fit=None"));
        assert!(out.contains("id=Some(3) range=None fit=Some(4096)"));
        assert!(out.contains("Connection: close"));
        assert!(out.to_ascii_lowercase().contains("content-range: bytes 0-1/2"));
    }

    #[test]
    fn rejects_other_paths_and_methods() {
        let out = run("GET /other/1 HTTP/1.1\r\n\r\nPOST /tok/1 HTTP/1.1\r\nConnection: close\r\n\r\n");
        assert!(out.starts_with("HTTP/1.1 404"));
        assert!(out.contains("HTTP/1.1 405"));
        assert!(!out.contains("id="));
    }

    #[test]
    fn head_has_no_body() {
        let out = run("HEAD /tok/9 HTTP/1.0\r\n\r\n");
        assert!(out.contains("Content-Length: "));
        assert!(!out.contains("id="));
    }
}
