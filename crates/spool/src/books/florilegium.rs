//! Handing a book to Florilegium, where books are read and quoted. The EPUB goes onto its
//! shelf and into its library at once; Florilegium then marks any entry of its reading
//! list that names the book as got.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// What Florilegium said when it took a book.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Receipt {
    pub at: i64,
    /// "shelved", or "exists" when it had these very bytes already.
    pub status: String,
    pub shelf_id: Option<i64>,
    pub book_id: Option<i64>,
}

/// Send an EPUB. An error says why it was not taken.
pub async fn send(http: &reqwest::Client, base: &str, token: &str, file: &Path, name: &str) -> Result<Receipt> {
    let bytes = tokio::fs::read(file).await?;
    let part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_string()).mime_str("application/epub+zip")?;
    let form = reqwest::multipart::Form::new().text("library", "1").part("file", part);
    let mut req = http.post(format!("{}/shelf/upload", base.trim_end_matches('/'))).multipart(form).timeout(std::time::Duration::from_secs(300));
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.map_err(|e| anyhow::anyhow!("Florilegium could not be reached: {}", e.without_url()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("Florilegium turned the token down");
    }
    let body: serde_json::Value = resp.json().await.map_err(|_| anyhow::anyhow!("Florilegium answered {status} with something that is not its API"))?;
    if !status.is_success() {
        bail!("Florilegium said: {}", body["error"].as_str().unwrap_or(status.as_str()));
    }
    // It answers for each file it was sent, even when one failed.
    let r = &body["results"][0];
    match r["status"].as_str() {
        Some("error") => bail!("Florilegium could not take it: {}", r["error"].as_str().unwrap_or("no reason given")),
        Some(s @ ("shelved" | "exists")) => {
            let book = &r["book"];
            let receipt = Receipt { at: crate::db::now(), status: s.to_string(), shelf_id: book["id"].as_i64(), book_id: book["book_id"].as_i64() };
            if receipt.book_id.is_none() {
                bail!("Florilegium shelved it but did not add it to its library");
            }
            Ok(receipt)
        }
        _ => bail!("Florilegium's answer could not be read"),
    }
}
