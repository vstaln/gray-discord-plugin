//! Port of grayai_legacy's attachment handling, lite: a user DMing an image
//! or a .txt is talking to a relay that used to drop it on the floor. We
//! save the file under workdir and name it in the prompt — gray reads it
//! with its own tools (`cat <image>` is its vision path, cat for text),
//! so no decoding happens here. Cap, don't stream: a 500MB attachment is a
//! deny, not a memory spike.

use std::path::{Path, PathBuf};

/// Discord's own attachment payload fields we care about.
pub struct AttachmentRef<'a> {
    pub url: &'a str,
    pub filename: &'a str,
    pub size: u64,
}

pub async fn save(
    http: &reqwest::Client,
    token: &str,
    a: &AttachmentRef<'_>,
    msg_id: &str,
    workdir: &Path,
    max_bytes: u64,
) -> Option<PathBuf> {
    if a.size > max_bytes || a.size == 0 {
        return None;
    }
    let safe: String = a
        .filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let dir = workdir.join("attachments");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{msg_id}-{safe}"));
    let resp = http
        .get(a.url)
        .header(reqwest::header::AUTHORIZATION, format!("Bot {token}"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    std::fs::write(&path, &bytes).ok()?;
    Some(path)
}

/// How the prompt names what the user attached. One line per file, path
/// first — that is what gray's tools consume.
pub fn prompt_lines(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| format!("[attached file: {}]", p.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_are_sanitised_and_prefixed_by_the_message_id() {
        assert_eq!(
            prompt_lines(&[PathBuf::from("/w/attachments/123-cat.png")]),
            "[attached file: /w/attachments/123-cat.png]"
        );
        let bad = "../../etc/pa$$wd".to_string();
        let safe: String = bad
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        assert_eq!(safe, ".._.._etc_pa__wd");
        assert!(!safe.contains('/'), "no path separator survives sanitising");
    }

    #[test]
    fn no_attachments_produce_no_lines() {
        assert_eq!(prompt_lines(&[]), "");
    }
}
