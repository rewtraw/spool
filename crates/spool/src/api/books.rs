//! Books: finding them in Open Library's catalogue, the ones added, choosing a file when the
//! index offers several, the files themselves, and the index of Anna's Archive.

use super::*;

#[derive(Deserialize)]
pub(super) struct Lookup {
    q: String,
}

pub(super) async fn book_lookup(State(app): State<App>, Query(l): Query<Lookup>) -> R<Value> {
    if l.q.trim().len() < 2 {
        return Ok(Json(json!([])));
    }
    Ok(Json(json!(app.book_lookup(l.q.trim()).await?)))
}

pub(super) async fn list_books(State(app): State<App>) -> R<Value> {
    Ok(Json(json!({"books": app.db.books()?, "status": app.books_status()})))
}

pub(super) async fn get_book(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    Ok(Json(json!(app.db.book(id)?.ok_or_else(|| not_found("book"))?)))
}

#[derive(Deserialize)]
pub(super) struct AddBook {
    ol_work: String,
    language: Option<String>,
}

pub(super) async fn add_book(State(app): State<App>, who: Option<axum::Extension<Who>>, Json(a): Json<AddBook>) -> R<Value> {
    let work = a.ol_work.trim().trim_start_matches("/works/");
    if !work.starts_with("OL") || !work.ends_with('W') {
        return Err(ApiError(StatusCode::BAD_REQUEST, "an Open Library work is named like OL45804W".into()));
    }
    Ok(Json(json!(app.add_book(work, a.language.as_deref(), Who::of(who).account).await?)))
}

#[derive(Deserialize)]
pub(super) struct RemoveBook {
    #[serde(default)]
    delete_file: bool,
}

pub(super) async fn delete_book(State(app): State<App>, Path(id): Path<i64>, Query(r): Query<RemoveBook>) -> R<Value> {
    app.remove_book(id, r.delete_file)?;
    Ok(Json(json!({"deleted": id})))
}

#[derive(Deserialize)]
pub(super) struct Choose {
    md5: String,
}

pub(super) async fn choose_book(State(app): State<App>, Path(id): Path<i64>, Json(c): Json<Choose>) -> R<Value> {
    Ok(Json(json!(app.choose_book_file(id, &c.md5)?)))
}

pub(super) async fn retry_book(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    Ok(Json(json!(app.retry_book(id)?)))
}

pub(super) async fn resend_book(State(app): State<App>, Path(id): Path<i64>) -> R<Value> {
    Ok(Json(json!(app.resend_book(id)?)))
}

/// The EPUB, to read or save.
pub(super) async fn book_file(State(app): State<App>, Path(id): Path<i64>, req: axum::extract::Request) -> Response {
    let Ok(Some(b)) = app.db.book(id) else { return StatusCode::NOT_FOUND.into_response() };
    if b.file.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let head = req.method() == axum::http::Method::HEAD;
    let mut response = crate::serve::send_file(&app.books.files_dir().join(&b.file), req.headers(), head).await;
    if let Ok(v) = "application/epub+zip".parse() {
        response.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    clips::to_save(&mut response, b.file.rsplit_once(" [").map(|(n, _)| format!("{n}.epub")).as_deref().unwrap_or(&b.file));
    response
}

/// Build the index again now, from the newest snapshot.
pub(super) async fn build_book_index(State(app): State<App>) -> R<Value> {
    if app.books.index_state.lock().building {
        return Ok(Json(json!({"started": false, "message": "already building"})));
    }
    let a = app.clone();
    tokio::spawn(async move {
        if a.task_start("book_index", "Running") {
            let m = a.book_index_task(true).await.unwrap_or_else(|e| format!("Failed: {e:#}"));
            a.task_end("book_index", &m);
        }
    });
    Ok(Json(json!({"started": true})))
}
