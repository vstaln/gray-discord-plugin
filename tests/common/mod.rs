//! Shared loopback Discord REST stub. No live network, no tokens.
//! Each test target reads a different subset of the fixture's fields; the
//! union is only ever complete across all of them.
#![allow(dead_code)]
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Default)]
pub struct Sent {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub headers: HashMap<String, String>,
    pub body: serde_json::Value,
}

/// Toggleable stub: `fail_auth` makes /users/@me 401, `fail_send` makes
/// message POSTs 403. Records every message POST's auth header + JSON body.
pub struct Stub {
    pub base: String,
    pub sent: Arc<Mutex<Vec<Sent>>>,
    pub fail_auth: Arc<Mutex<bool>>,
    pub fail_send: Arc<Mutex<bool>>,
    /// When set, `/channels/42` reports a guild channel (id 77) so doctor's
    /// permission path runs; the guild has a permissions-bearing role 78.
    pub channel_guild: Arc<Mutex<bool>>,
    _task: tokio::task::JoinHandle<()>,
}

async fn read_request(
    stream: &mut tokio::net::TcpStream,
) -> Option<(String, String, HashMap<String, String>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = find_headers_end(&buf) {
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or("").to_string();
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let mut headers = HashMap::new();
            for line in lines {
                if let Some((k, v)) = line.split_once(':') {
                    headers.insert(k.trim().to_lowercase(), v.trim().to_string());
                }
            }
            let len: usize = headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let body_start = end;
            while buf.len() < body_start + len {
                let n = stream.read(&mut tmp).await.ok()?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let body = buf[body_start..(body_start + len).min(buf.len())].to_vec();
            return Some((method, path, headers, body));
        }
        if buf.len() > 65536 {
            return None;
        }
    }
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

async fn respond(stream: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let text = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    );
    let _ = stream.write_all(text.as_bytes()).await;
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    }
}

fn decode_request_body(headers: &HashMap<String, String>, body: &[u8]) -> serde_json::Value {
    if let Ok(value) = serde_json::from_slice(body) {
        return value;
    }
    let Some(content_type) = headers.get("content-type") else {
        return serde_json::Value::Null;
    };
    let Some(boundary) = content_type.split("boundary=").nth(1) else {
        return serde_json::Value::Null;
    };
    let marker = format!("--{boundary}").into_bytes();
    for part in split_on(body, &marker) {
        if part.is_empty() || part.starts_with(b"--") {
            continue;
        }
        let Some(split) = part.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let name = String::from_utf8_lossy(&part[..split]).to_ascii_lowercase();
        if !name.contains("name=\"payload_json\"") {
            continue;
        }
        let start = split + 4;
        let end = part.len().saturating_sub(2);
        if end >= start {
            if let Ok(value) = serde_json::from_slice(&part[start..end]) {
                return value;
            }
        }
    }
    serde_json::Value::Null
}

/// Split `body` on every occurrence of the multipart boundary `marker`
/// (the whole byte sequence: a payload may well contain `-`).
fn split_on<'a>(body: &'a [u8], marker: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at + marker.len() <= body.len() {
        if &body[at..at + marker.len()] == marker {
            parts.push(&body[start..at]);
            at += marker.len();
            start = at;
        } else {
            at += 1;
        }
    }
    parts.push(&body[start..]);
    parts
}

// flags 262144 = 1<<18 APP_FLAG_MESSAGE_CONTENT so doctor intent checks pass.
const USER: &str = r#"{"id":"123","username":"fixture","bot":true}"#;

impl Stub {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub");
        let port = listener.local_addr().expect("addr").port();
        let sent: Arc<Mutex<Vec<Sent>>> = Arc::default();
        let fail_auth: Arc<Mutex<bool>> = Arc::default();
        let fail_send: Arc<Mutex<bool>> = Arc::default();
        let channel_guild: Arc<Mutex<bool>> = Arc::default();
        let task = {
            let sent = sent.clone();
            let fail_auth = fail_auth.clone();
            let fail_send = fail_send.clone();
            let channel_guild = channel_guild.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let sent = sent.clone();
                    let fail_auth = fail_auth.clone();
                    let fail_send = fail_send.clone();
                    let channel_guild = channel_guild.clone();
                    tokio::spawn(async move {
                        let Some((method, path, headers, body)) = read_request(&mut stream).await
                        else {
                            return;
                        };
                        if path == "/api/v10/users/@me" {
                            if *fail_auth.lock().unwrap() {
                                respond(&mut stream, 401, r#"{"message":"Unauthorized","code":0}"#)
                                    .await;
                            } else {
                                respond(&mut stream, 200, USER).await;
                            }
                        } else if path == "/api/v10/applications/@me" {
                            respond(&mut stream, 200, r#"{"id":"999","flags":262144}"#).await;
                        } else if path == "/api/v10/channels/42" {
                            let body = if *channel_guild.lock().unwrap() {
                                r#"{"id":"42","type":0,"guild_id":"77"}"#
                            } else {
                                r#"{"id":"42","type":1}"#
                            };
                            respond(&mut stream, 200, body).await;
                        } else if path == "/api/v10/guilds/77" {
                            // @everyone (id == guild) carries nothing; role 78
                            // carries exactly doctor's HOME_PERMS (68608).
                            respond(
                                &mut stream,
                                200,
                                r#"{"id":"77","owner_id":"999","roles":[{"id":"77","permissions":"0"},{"id":"78","permissions":"68608"}]}"#,
                            )
                            .await;
                        } else if path == "/api/v10/guilds/77/members/123" {
                            respond(&mut stream, 200, r#"{"roles":["78"]}"#).await;
                        } else if (method == "PATCH" || method == "DELETE")
                            && path
                                .strip_prefix("/api/v10/channels/42/messages/")
                                .is_some_and(|id| {
                                    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
                                })
                        {
                            let body = decode_request_body(&headers, &body);
                            let n = {
                                let mut sent = sent.lock().unwrap();
                                let n = sent.len();
                                sent.push(Sent {
                                    method: method.clone(),
                                    path: path.clone(),
                                    auth: headers.get("authorization").cloned(),
                                    headers: headers.clone(),
                                    body,
                                });
                                n
                            };
                            let reply = format!(r#"{{"id":"{}","channel_id":"42"}}"#, 1000 + n);
                            respond(&mut stream, 200, &reply).await;
                        } else if method == "POST"
                            && path.starts_with("/api/v10/interactions/")
                            && path.ends_with("/callback")
                        {
                            let body = decode_request_body(&headers, &body);
                            {
                                let mut sent = sent.lock().unwrap();
                                sent.push(Sent {
                                    method: method.clone(),
                                    path: path.clone(),
                                    auth: headers.get("authorization").cloned(),
                                    headers: headers.clone(),
                                    body,
                                });
                            }
                            respond(&mut stream, 204, "").await;
                        } else if method == "PATCH"
                            && path.starts_with("/api/v10/webhooks/")
                            && path.ends_with("/messages/@original")
                        {
                            let body = decode_request_body(&headers, &body);
                            let n = {
                                let mut sent = sent.lock().unwrap();
                                let n = sent.len();
                                sent.push(Sent {
                                    method: method.clone(),
                                    path: path.clone(),
                                    auth: headers.get("authorization").cloned(),
                                    headers: headers.clone(),
                                    body,
                                });
                                n
                            };
                            let reply = format!(r#"{{"id":"{}","channel_id":"42"}}"#, 2000 + n);
                            respond(&mut stream, 200, &reply).await;
                        } else if method == "POST" && path.starts_with("/api/v10/webhooks/") {
                            let body = decode_request_body(&headers, &body);
                            let n = {
                                let mut sent = sent.lock().unwrap();
                                let n = sent.len();
                                sent.push(Sent {
                                    method: method.clone(),
                                    path: path.clone(),
                                    auth: headers.get("authorization").cloned(),
                                    headers: headers.clone(),
                                    body,
                                });
                                n
                            };
                            let reply = format!(r#"{{"id":"{}","channel_id":"42"}}"#, 3000 + n);
                            respond(&mut stream, 200, &reply).await;
                        } else if path == "/api/v10/channels/42/messages" && method == "POST" {
                            // Record before the fail toggle, like the Python
                            // fixture: the refused attempt still reached the server.
                            let body = decode_request_body(&headers, &body);
                            let n = {
                                let mut sent = sent.lock().unwrap();
                                let n = sent.len();
                                sent.push(Sent {
                                    method: method.clone(),
                                    path: path.clone(),
                                    auth: headers.get("authorization").cloned(),
                                    headers: headers.clone(),
                                    body,
                                });
                                n
                            };
                            if *fail_send.lock().unwrap() {
                                respond(
                                    &mut stream,
                                    403,
                                    r#"{"message":"Missing Permissions","code":50013}"#,
                                )
                                .await;
                            } else {
                                let reply = format!(r#"{{"id":"{}","channel_id":"42"}}"#, 1000 + n);
                                respond(&mut stream, 200, &reply).await;
                            }
                        } else if path == "/api/v10/channels/42/typing" && method == "POST" {
                            respond(&mut stream, 204, "{}").await;
                        } else {
                            respond(
                                &mut stream,
                                404,
                                r#"{"message":"Unknown fixture endpoint"}"#,
                            )
                            .await;
                        }
                    });
                }
            })
        };
        Self {
            base: format!("http://127.0.0.1:{port}/api/v10"),
            sent,
            fail_auth,
            fail_send,
            channel_guild,
            _task: task,
        }
    }
}
