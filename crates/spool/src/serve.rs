//! Sending a file, or the part of one that was asked for. Players ask for a film in parts,
//! and ask again from another place every time someone skips; Spool and a machine it is
//! linked to both answer them with this.

use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::Path;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

/// What part of a file a request wants: first and last byte, and whether it asked for a part.
/// An error means the range cannot be met. Several ranges at once are answered with the
/// whole file, which a player is always ready for.
fn byte_range(header: Option<&str>, len: u64) -> Result<(u64, u64, bool), ()> {
    let whole = (0, len.saturating_sub(1), false);
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else { return Ok(whole) };
    if spec.contains(',') {
        return Ok(whole);
    }
    let (from, to) = spec.trim().split_once('-').ok_or(())?;
    let (start, end) = match (from.trim(), to.trim()) {
        ("", "") => return Err(()),
        ("", last) => {
            let n: u64 = last.parse().map_err(|_| ())?;
            (len.saturating_sub(n), len.saturating_sub(1))
        }
        (first, "") => (first.parse().map_err(|_| ())?, len.saturating_sub(1)),
        (first, last) => (first.parse().map_err(|_| ())?, last.parse::<u64>().map_err(|_| ())?.min(len.saturating_sub(1))),
    };
    if len == 0 || start >= len || start > end {
        return Err(());
    }
    Ok((start, end, true))
}

/// Send an open file, or the part of it asked for, a piece at a time and no faster than it
/// is taken. `name` is only what kind of file to call it. Whatever is `held` is kept until
/// the last piece has gone or the player has hung up.
pub async fn send<H: Send + 'static>(mut file: tokio::fs::File, name: &Path, headers: &HeaderMap, head: bool, held: H) -> Response {
    let len = match file.metadata().await {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "a file that was opened could not be measured");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // Asking only what is there says nothing of parts.
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).filter(|_| !head);
    let Ok((start, end, partial)) = byte_range(range, len) else {
        return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{len}"))]).into_response();
    };
    let count = if len == 0 { 0 } else { end - start + 1 };
    let mime = mime_guess::from_path(name).first().filter(|m| m.type_() == "video").map(|m| m.to_string()).unwrap_or_else(|| "video/x-matroska".into());
    let mut response = Response::builder()
        .status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, count);
    if partial {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    if head || count == 0 {
        return response.body(Body::empty()).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    const PIECE: usize = 256 * 1024;
    let pieces = ReaderStream::with_capacity(file.take(count), PIECE).map(move |piece| {
        let _ = &held;
        piece
    });
    response.body(Body::from_stream(pieces)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Send the file at a path.
pub async fn send_file(path: &Path, headers: &HeaderMap, head: bool) -> Response {
    match tokio::fs::File::open(path).await {
        Ok(file) => send(file, path, headers, head, ()).await,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "a player asked for a file that could not be opened");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_are_read_as_players_send_them() {
        assert_eq!(byte_range(None, 1000), Ok((0, 999, false)));
        assert_eq!(byte_range(Some("bytes=0-"), 1000), Ok((0, 999, true)));
        assert_eq!(byte_range(Some("bytes=200-299"), 1000), Ok((200, 299, true)));
        assert_eq!(byte_range(Some("bytes=900-5000"), 1000), Ok((900, 999, true)));
        assert_eq!(byte_range(Some("bytes=-100"), 1000), Ok((900, 999, true)));
        assert_eq!(byte_range(Some("bytes=1000-"), 1000), Err(()));
        assert_eq!(byte_range(Some("bytes=5-2"), 1000), Err(()));
        assert_eq!(byte_range(Some("bytes=abc"), 1000), Err(()));
        assert_eq!(byte_range(Some("bytes=0-9, 20-29"), 1000), Ok((0, 999, false)), "several parts at once are answered with the whole");
        assert_eq!(byte_range(Some("bytes=-0"), 1000), Err(()));
        assert_eq!(byte_range(Some("bytes=-5000"), 1000), Ok((0, 999, true)));
        assert_eq!(byte_range(Some("bytes=0-"), 0), Err(()));
        assert_eq!(byte_range(None, 0), Ok((0, 0, false)));
        assert_eq!(byte_range(Some("items=0-5"), 1000), Ok((0, 999, false)), "a unit that is not bytes is not a range");
        assert_eq!(byte_range(Some("bytes=99999999999999999999-"), 1000), Err(()));
    }
}
