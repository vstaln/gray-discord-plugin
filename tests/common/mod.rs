//! Shared loopback Discord REST stub. No live network, no tokens.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Default)]
pub struct Sent {
    pub auth: Option<String>,
    pub body: serde_json::Value,
}

/// Toggleable stub: `fail_auth` makes /users/@me 401, `fail_send` makes
/// message POSTs 403. Records every message POST's auth header + JSON body.
pub struct Stub {
    pub base: String,
    pub sent: Arc<Mutex<Vec<Sent>>>,
    pub fail_auth: Arc<Mutex<bool>>,
    pub fail_send: Arc<Mutex<bool>>,
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
        let task = {
            let sent = sent.clone();
            let fail_auth = fail_auth.clone();
            let fail_send = fail_send.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let sent = sent.clone();
                    let fail_auth = fail_auth.clone();
                    let fail_send = fail_send.clone();
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
                            respond(&mut stream, 200, r#"{"id":"42","type":1}"#).await;
                        } else if path == "/api/v10/channels/42/messages" && method == "POST" {
                            // Record before the fail toggle, like the Python
                            // fixture: the refused attempt still reached the server.
                            let body: serde_json::Value =
                                serde_json::from_slice(&body).unwrap_or_default();
                            let n = {
                                let mut sent = sent.lock().unwrap();
                                let n = sent.len();
                                sent.push(Sent {
                                    auth: headers.get("authorization").cloned(),
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
            _task: task,
        }
    }
}
