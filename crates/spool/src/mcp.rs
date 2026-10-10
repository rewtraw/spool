//! Model Context Protocol endpoint: lets an AI session look at Spool and operate it.
//!
//! Served at `/mcp` as stateless streamable HTTP (one JSON-RPC request in, one JSON answer out).
//! Every tool is a thin wrapper over the same HTTP API the web app uses, called in-process, so
//! the two cannot drift apart. Answers are cut down to what a model needs and never carry
//! indexer links, keys or tokens.

use crate::app::App;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Spool finds, downloads and files movies and TV for one household, replacing Radarr, Sonarr and SABnzbd. \
Start with `status` for the overall picture or `find_title` to get a title's id; most other tools take that id. \
`diagnose` answers \"why has this not downloaded\" in one call. Sizes are in GB. \
Disk space is tight: check `status` before starting large downloads. \
Tools that delete files need `confirm: true` and should only be called when the person asked for that deletion.";

#[derive(Clone, Copy, PartialEq)]
enum Access {
    Read,
    Full,
}

#[derive(Clone)]
pub struct Mcp {
    app: App,
    /// The HTTP API without its sign-in layer; requests reaching it here are already authorized.
    api: Router,
}

pub fn routes(app: App, api: Router) -> Router {
    let refuse = || async { (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response() };
    Router::new().route("/mcp", post(handle).get(refuse).delete(refuse)).with_state(Arc::new(Mcp { app, api }))
}

fn access(app: &App, headers: &HeaderMap) -> Option<Access> {
    let g = app.settings.general();
    // Unlike the web app, this endpoint is never open: Spool makes itself a key at first start.
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))?
        .trim();
    if !g.api_key.is_empty() && given == g.api_key {
        Some(Access::Full)
    } else if !g.mcp_read_key.is_empty() && given == g.mcp_read_key {
        Some(Access::Read)
    } else {
        None
    }
}

async fn handle(State(mcp): State<Arc<Mcp>>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let Some(level) = access(&mcp.app, &headers) else {
        return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], Json(json!({"error": "a Spool API key is required: Authorization: Bearer <key>"}))).into_response();
    };
    match body {
        Value::Array(batch) => {
            let mut out = vec![];
            for msg in batch {
                if let Some(r) = mcp.message(msg, level).await {
                    out.push(r);
                }
            }
            if out.is_empty() { StatusCode::ACCEPTED.into_response() } else { Json(Value::Array(out)).into_response() }
        }
        msg => match mcp.message(msg, level).await {
            Some(r) => Json(r).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}

struct Tool {
    name: &'static str,
    /// Changes nothing in Spool or on disk.
    read: bool,
    /// Can remove files or records.
    destructive: bool,
    description: &'static str,
    schema: fn() -> Value,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

const TOOLS: &[Tool] = &[
    Tool { name: "status", read: true, destructive: false, description: "Overall state: mode, library size, free disk space, warnings, what is downloading now, and open attention items (things that failed and need a decision).", schema: || obj(json!({}), &[]) },
    Tool {
        name: "find_title",
        read: true,
        destructive: false,
        description: "Find a movie or series by name. Returns matches already in the library (with their id) and, with lookup=true, candidates from TMDB/TVmaze that could be added with add_title.",
        schema: || obj(json!({"query": {"type": "string"}, "kind": {"type": "string", "enum": ["movie", "series"], "description": "Limit to one kind. Needed for lookup; defaults to movie there."}, "lookup": {"type": "boolean", "description": "Also search TMDB (movies) or TVmaze (series) for titles not in the library."}}), &["query"]),
    },
    Tool {
        name: "title_detail",
        read: true,
        destructive: false,
        description: "Everything about one library title: monitoring, profile, files on disk, episode coverage per season, downloads in progress, the releases Spool has considered (with the reason each was rejected), saved releases in the archive, and its Plex link.",
        schema: || obj(json!({"title_id": {"type": "integer"}, "rejected_limit": {"type": "integer", "description": "How many rejected releases to include (default 15)."}}), &["title_id"]),
    },
    Tool {
        name: "diagnose",
        read: true,
        destructive: false,
        description: "Explain why a title is missing or stuck: whether it is monitored and released, when it was last searched, why releases were rejected (grouped by reason), failed downloads with their errors, blocklisted releases, open attention items and matching log lines.",
        schema: || obj(json!({"title_id": {"type": "integer"}}), &["title_id"]),
    },
    Tool { name: "activity", read: true, destructive: false, description: "The download queue with progress and speed, followed by recently finished, failed and cancelled downloads. Each row has an acquisition_id for manage_download.", schema: || obj(json!({"limit": {"type": "integer", "description": "How many finished rows to include (default 20)."}}), &[]) },
    Tool {
        name: "logs",
        read: true,
        destructive: false,
        description: "Recent lines from Spool's log, newest last. Filter by a substring (a title, release name or job id) and/or minimum level.",
        schema: || obj(json!({"contains": {"type": "string"}, "level": {"type": "string", "enum": ["info", "warn", "error"]}, "lines": {"type": "integer", "description": "Default 60, at most 300."}}), &[]),
    },
    Tool { name: "archive", read: true, destructive: false, description: "Releases whose NZB Spool has saved, grouped by title, including titles no longer in the library. Each release has a saved_id usable with download_release and check_availability.", schema: || obj(json!({"query": {"type": "string", "description": "Filter by title or release name."}}), &[]) },
    Tool { name: "check_availability", read: true, destructive: false, description: "Ask the Usenet servers whether a saved release is still there, by sampling its articles. Downloads nothing.", schema: || obj(json!({"saved_id": {"type": "integer"}}), &["saved_id"]) },
    Tool { name: "disk_space", read: true, destructive: false, description: "Free space, what is waiting for room, what sits in the recycle folder, and the largest titles with whether each was watched in Plex and whether it could be downloaded again from a saved release. Use it to suggest what to remove.", schema: || obj(json!({"limit": {"type": "integer", "description": "How many titles to list, largest first (default 30)."}}), &[]) },
    Tool { name: "plex_report", read: true, destructive: false, description: "Compare Plex with Spool: what Plex has that Spool does not track, and titles with files that Plex has not picked up.", schema: || obj(json!({}), &[]) },
    Tool {
        name: "add_title",
        read: false,
        destructive: false,
        description: "Add a movie or series to the library. Get tmdb_id (movies) or tvmaze_id (series) from find_title with lookup=true. By default it is monitored and a search starts at once, which may begin a download.",
        schema: || obj(json!({"kind": {"type": "string", "enum": ["movie", "series"]}, "tmdb_id": {"type": "integer"}, "tvmaze_id": {"type": "integer"}, "monitored": {"type": "boolean", "description": "Default true."}, "search": {"type": "boolean", "description": "Search and download right away. Default true."}, "profile": {"type": "string", "description": "Quality profile name. Defaults to the library default."}}), &["kind"]),
    },
    Tool {
        name: "update_title",
        read: false,
        destructive: false,
        description: "Change a title's monitoring or quality profile, or the monitoring of one season. Unmonitored titles are never downloaded automatically.",
        schema: || obj(json!({"title_id": {"type": "integer"}, "monitored": {"type": "boolean"}, "profile": {"type": "string", "description": "Quality profile name."}, "season": {"type": "integer", "description": "With monitored: apply to this season only."}}), &["title_id"]),
    },
    Tool {
        name: "remove_title",
        read: false,
        destructive: true,
        description: "Remove a title from the library and cancel its downloads. Files stay on disk unless delete_files is true, which also needs confirm=true. Saved releases in the archive are always kept.",
        schema: || obj(json!({"title_id": {"type": "integer"}, "delete_files": {"type": "boolean"}, "confirm": {"type": "boolean"}}), &["title_id"]),
    },
    Tool { name: "refresh_title", read: false, destructive: false, description: "Re-read a title's details and episode list from TMDB/TVmaze and rescan its folder for files added or removed outside Spool.", schema: || obj(json!({"title_id": {"type": "integer"}}), &["title_id"]) },
    Tool {
        name: "search_releases",
        read: false,
        destructive: false,
        description: "Ask the indexers (and the archive) for releases of a title and judge them. With grab=false it only reports; with grab=true the best acceptable release starts downloading. For a series give season, or season and episode; neither means everything missing. Uses indexer request allowance.",
        schema: || obj(json!({"title_id": {"type": "integer"}, "season": {"type": "integer"}, "episode": {"type": "integer"}, "grab": {"type": "boolean", "description": "Default false."}, "compact": {"type": "boolean", "description": "Look for smaller copies of files that are over the profile's size target, accepting equal or lower quality that fits it. With grab=true the chosen copy replaces the file on disk once imported."}}), &["title_id"]),
    },
    Tool {
        name: "download_release",
        read: false,
        destructive: false,
        description: "Download one specific release: a release_id from title_detail or search_releases (works for rejected ones too, overriding the rejection), or a saved_id from the archive. If the title already has a file, the new one replaces it once imported.",
        schema: || obj(json!({"release_id": {"type": "integer"}, "saved_id": {"type": "integer"}}), &[]),
    },
    Tool {
        name: "manage_download",
        read: false,
        destructive: false,
        description: "Act on a download by acquisition_id: cancel; cancel_and_find_another (blocklists it and searches again); pause; resume (also overrides a free-space hold); retry_import. pause_all and resume_all act on the whole queue and need no id.",
        schema: || obj(json!({"action": {"type": "string", "enum": ["cancel", "cancel_and_find_another", "pause", "resume", "retry_import", "pause_all", "resume_all"]}, "acquisition_id": {"type": "integer"}}), &["action"]),
    },
    Tool { name: "unblock_release", read: false, destructive: false, description: "Take a release off the blocklist so it can be chosen again. Blocklist ids come from diagnose.", schema: || obj(json!({"blocklist_id": {"type": "integer"}}), &["blocklist_id"]) },
    Tool { name: "dismiss_attention", read: false, destructive: false, description: "Close an attention item once it has been dealt with. Ids come from status.", schema: || obj(json!({"attention_id": {"type": "integer"}}), &["attention_id"]) },
    Tool { name: "plex_sync", read: false, destructive: false, description: "Re-read the Plex library now so the Spool-to-Plex matching and plex_report are current.", schema: || obj(json!({}), &[]) },
    Tool { name: "plex_track", read: false, destructive: false, description: "Start tracking something Plex has and Spool does not, using a rating_key from plex_report. Added unmonitored at its existing folder; nothing is moved or downloaded.", schema: || obj(json!({"rating_key": {"type": "string"}}), &["rating_key"]) },
    Tool {
        name: "run_task",
        read: false,
        destructive: false,
        description: "Start a background task: rss (read indexer feeds), backlog (search a few missing titles), refresh (update details), scan (rescan library folders), housekeeping (backup and tidy), plex (read Plex), compact (look for smaller copies of the few titles furthest over their size target; replaces files when they arrive).",
        schema: || obj(json!({"name": {"type": "string", "enum": ["rss", "backlog", "refresh", "scan", "housekeeping", "plex", "compact"]}}), &["name"]),
    },
    Tool { name: "delete_title_files", read: false, destructive: true, description: "Free space by deleting all of a title's media files. The title stays in the library, unmonitored so it is not fetched again, and its saved releases stay in the archive for a later re-download. Needs confirm=true.", schema: || obj(json!({"title_id": {"type": "integer"}, "confirm": {"type": "boolean"}}), &["title_id"]) },
    Tool { name: "delete_file", read: false, destructive: true, description: "Delete one media file by file_id (from title_detail). It goes to the recycle folder if recycling is on, otherwise it is gone at once. Needs confirm=true.", schema: || obj(json!({"file_id": {"type": "integer"}, "confirm": {"type": "boolean"}}), &["file_id"]) },
    Tool { name: "empty_recycle", read: false, destructive: true, description: "Permanently delete everything in the recycle folder to free disk space. Needs confirm=true.", schema: || obj(json!({"confirm": {"type": "boolean"}}), &[]) },
];

fn gb(bytes: &Value) -> Value {
    json!((bytes.as_f64().unwrap_or(0.0) / 1e9 * 10.0).round() / 10.0)
}

fn when(ts: &Value) -> Value {
    match ts.as_i64() {
        Some(t) if t > 0 => json!(chrono::DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())),
        _ => Value::Null,
    }
}

fn quality(q: &Value) -> Value {
    let name = q["quality"].as_str().unwrap_or("unknown");
    let version = q["revision"]["version"].as_i64().unwrap_or(1);
    json!(if version > 1 { format!("{name} v{version}") } else { name.to_string() })
}

/// One judged release, without anything that identifies the indexer account.
fn release_row(d: &Value) -> Value {
    json!({
        "release_id": d["id"], "name": d["release"]["title"], "indexer": d["release"]["indexer"], "quality": quality(&d["quality"]), "size_gb": gb(&d["release"]["size"]),
        "covers": d["covers"], "accepted": d["accepted"],
        "rejected_because": d["rejections"].as_array().map(|r| r.iter().filter_map(|x| x["message"].as_str()).collect::<Vec<_>>()).unwrap_or_default(),
    })
}

fn acquisition_row(a: &Value, job: &Value, title: &Value) -> Value {
    let mut v = json!({"acquisition_id": a["id"], "title": title, "title_id": a["title_id"], "release": a["release"]["title"], "quality": quality(&a["quality"]), "state": a["state"], "size_gb": gb(&a["release"]["size"]), "updated": when(&a["updated_at"])});
    if let Some(e) = a["error"].as_str() {
        v["error"] = json!(e);
    }
    if job.is_object() {
        let (done, total) = (job["done_bytes"].as_f64().unwrap_or(0.0), job["total_bytes"].as_f64().unwrap_or(0.0));
        v["step"] = job["message"].clone();
        v["size_gb"] = gb(&job["total_bytes"]);
        v["percent"] = json!(if total > 0.0 { (done / total * 100.0).round() } else { 0.0 });
        v["speed_mb_s"] = json!((job["speed"].as_f64().unwrap_or(0.0) / 1048576.0 * 10.0).round() / 10.0);
    }
    v
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Remove anything address-like, which is where a key could hide.
fn redact(line: &str) -> String {
    line.split(' ').map(|w| if w.contains("://") || w.to_lowercase().contains("apikey=") { "[address removed]" } else { w }).collect::<Vec<_>>().join(" ")
}

fn log_lines(path: &std::path::Path, contains: Option<&str>, level: Option<&str>, limit: usize) -> Result<Vec<String>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| format!("the log at {} could not be read: {e}", path.display()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    // The tail is enough; a filter that needs older history is better answered by title_detail.
    let window = 4 << 20;
    f.seek(SeekFrom::Start(len.saturating_sub(window))).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let needle = contains.map(str::to_lowercase);
    let levels: &[&str] = match level {
        Some("error") => &[" ERROR "],
        Some("warn") => &[" WARN ", " ERROR "],
        _ => &[],
    };
    let mut lines: Vec<String> = text
        .lines()
        .skip(if len > window { 1 } else { 0 })
        .map(strip_ansi)
        .filter(|l| levels.is_empty() || levels.iter().any(|lv| l.contains(lv)))
        .filter(|l| needle.as_ref().is_none_or(|n| l.to_lowercase().contains(n)))
        .map(|l| redact(&l))
        .collect();
    let start = lines.len().saturating_sub(limit);
    Ok(lines.split_off(start))
}

impl Mcp {
    async fn message(&self, msg: Value, level: Access) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or("");
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                let asked = msg["params"]["protocolVersion"].as_str().unwrap_or("");
                let version = if PROTOCOLS.contains(&asked) { asked } else { PROTOCOLS[0] };
                Ok(json!({"protocolVersion": version, "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "spool", "title": "Spool", "version": env!("CARGO_PKG_VERSION")}, "instructions": INSTRUCTIONS}))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": TOOLS.iter().filter(|t| level == Access::Full || t.read).map(|t| json!({
                "name": t.name, "description": t.description, "inputSchema": (t.schema)(),
                "annotations": {"readOnlyHint": t.read, "destructiveHint": t.destructive, "openWorldHint": false},
            })).collect::<Vec<_>>()})),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let args = msg["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
                match TOOLS.iter().find(|t| t.name == name) {
                    None => Err((-32602, format!("unknown tool: {name}"))),
                    Some(t) if !t.read && level != Access::Full => Ok(tool_text(&json!("this key is read-only; that tool needs the full-access key"), true)),
                    Some(t) => {
                        tracing::info!(tool = t.name, "mcp call");
                        Ok(match self.tool(t.name, &args).await {
                            Ok(v) => tool_text(&v, false),
                            Err(e) => tool_text(&json!(e), true),
                        })
                    }
                }
            }
            m if m.starts_with("notifications/") => return None,
            other => Err((-32601, format!("method not found: {other}"))),
        };
        // A message without an id is a notification and gets no answer.
        let id = id?;
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        })
    }

    /// Call the HTTP API in-process.
    async fn api(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        let mut req = Request::builder().method(method).uri(path);
        let payload = match &body {
            Some(b) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let resp = self.api.clone().oneshot(req.body(payload).map_err(|e| e.to_string())?).await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 << 20).await.map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            Ok(v)
        } else {
            Err(v["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("Spool answered HTTP {}", status.as_u16())))
        }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        self.api(Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.api(Method::POST, path, Some(body)).await
    }

    fn profile_id(&self, name: &str, kind: &str) -> Result<i64, String> {
        let want = if kind == "movie" { "movie" } else { "tv" };
        let all = self.app.db.profiles().map_err(|e| e.to_string())?;
        all.iter()
            .find(|p| p.kind == want && p.name.eq_ignore_ascii_case(name))
            .map(|p| p.id)
            .ok_or_else(|| format!("no {kind} profile is called {name:?}; there are: {}", all.iter().filter(|p| p.kind == want).map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")))
    }

    async fn tool(&self, name: &str, a: &Value) -> Result<Value, String> {
        let int = |k: &str| a[k].as_i64().ok_or_else(|| format!("{k} is required"));
        let confirmed = a["confirm"].as_bool() == Some(true);
        match name {
            "status" => {
                let s = self.get("/api/status").await?;
                let act = self.get("/api/activity").await?;
                let attention = self.get("/api/attention").await?;
                let queue: Vec<Value> = act["items"].as_array().into_iter().flatten().filter(|r| r["job"].is_object() || r["acquisition"]["state"] == "importing").map(|r| acquisition_row(&r["acquisition"], &r["job"], &r["title"])).collect();
                Ok(json!({
                    "mode": s["mode"], "version": s["version"], "movies": s["movies"], "series": s["series"], "library_gb": gb(&s["library_bytes"]), "free_gb": gb(&s["free_bytes"]),
                    "media_volume_ok": s["volume_ok"], "queue_paused": s["queue_paused"], "warnings": s["warnings"], "downloading": queue,
                    "indexers": self.get("/api/indexers").await?.as_array().into_iter().flatten().map(|i| json!({"name": i["name"], "requests_today": i["usage"]["requests"], "request_limit": i["usage"]["request_limit"], "downloads_today": i["usage"]["grabs"], "download_limit": i["usage"]["grab_limit"], "limits_from": i["usage"]["source"], "failing": i["failing"]})).collect::<Vec<_>>(),
                    "attention": attention.as_array().into_iter().flatten().map(|x| json!({"attention_id": x["item"]["id"], "title": x["title"], "title_id": x["item"]["title_id"], "message": x["item"]["message"], "release": x["item"]["data"]["release"], "reason": x["item"]["data"]["reason"], "since": when(&x["item"]["ts"])})).collect::<Vec<_>>(),
                }))
            }
            "find_title" => {
                let q = a["query"].as_str().unwrap_or("").trim().to_lowercase();
                if q.is_empty() {
                    return Err("query is required".into());
                }
                let kind = a["kind"].as_str();
                let all = self.get("/api/titles").await?;
                let words: Vec<&str> = q.split_whitespace().collect();
                let library: Vec<Value> = all
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| kind.is_none_or(|k| t["kind"] == k))
                    .filter(|t| {
                        let hay = format!("{} {}", t["title"].as_str().unwrap_or("").to_lowercase(), t["year"]);
                        words.iter().all(|w| hay.contains(w))
                    })
                    .take(25)
                    .map(|t| json!({"title_id": t["id"], "title": t["title"], "year": t["year"], "kind": t["kind"], "monitored": t["monitored"], "profile": t["profile"], "files": t["file_count"], "size_gb": gb(&t["size"]), "episodes": if t["kind"] == "series" { json!(format!("{} of {} aired", t["episodes_have"], t["episodes_aired"])) } else { Value::Null }, "downloading": t["active"]}))
                    .collect();
                let mut out = json!({"in_library": library});
                if a["lookup"].as_bool() == Some(true) {
                    let k = kind.unwrap_or("movie");
                    let found = self.get(&format!("/api/lookup?kind={k}&q={}", urlencoding::encode(a["query"].as_str().unwrap_or("")))).await?;
                    out["can_be_added"] = json!(found.as_array().into_iter().flatten().take(10).map(|t| json!({"title": t["title"], "year": t["year"], "kind": k, "tmdb_id": t["tmdb_id"], "tvmaze_id": t["tvmaze_id"], "already_in_library_as": t["library_id"], "overview": t["overview"].as_str().map(|o| o.chars().take(160).collect::<String>())})).collect::<Vec<_>>());
                }
                Ok(out)
            }
            "title_detail" => {
                let t = self.get(&format!("/api/titles/{}", int("title_id")?)).await?;
                let rejected_limit = a["rejected_limit"].as_u64().unwrap_or(15) as usize;
                let decisions: Vec<&Value> = t["decisions"].as_array().into_iter().flatten().collect();
                let mut seasons: std::collections::BTreeMap<i64, (u32, u32, Vec<String>)> = Default::default();
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                for e in t["episodes"].as_array().into_iter().flatten() {
                    let s = seasons.entry(e["season"].as_i64().unwrap_or(0)).or_default();
                    let aired = e["air_date"].as_str().is_some_and(|d| d <= today.as_str());
                    if aired {
                        s.0 += 1;
                    }
                    if !e["file_id"].is_null() {
                        s.1 += 1;
                    } else if aired && e["monitored"] == true {
                        s.2.push(format!("E{:02}", e["episode"].as_i64().unwrap_or(0)));
                    }
                }
                Ok(json!({
                    "title_id": t["id"], "title": t["title"], "year": t["year"], "kind": t["kind"], "monitored": t["monitored"], "profile": t["profile"], "path": t["path"],
                    "tmdb_id": t["tmdb_id"], "tvdb_id": t["tvdb_id"], "imdb_id": t["imdb_id"], "status": t["status"], "released": t["available"], "last_searched": when(&t["last_search_at"]),
                    "plex": t["plex"]["url"],
                    "files": t["files"].as_array().into_iter().flatten().map(|f| json!({"file_id": f["id"], "path": f["rel_path"], "quality": quality(&f["quality"]), "size_gb": gb(&f["size"]), "video": f["media_info"]["video_codec"], "audio": f["media_info"]["audio_codec"], "height": f["media_info"]["height"]})).collect::<Vec<_>>(),
                    "seasons": seasons.iter().map(|(n, (aired, have, missing))| json!({"season": n, "aired": aired, "have": have, "missing_monitored": missing})).collect::<Vec<_>>(),
                    "downloads": t["acquisitions"].as_array().into_iter().flatten().take(12).map(|x| acquisition_row(x, &Value::Null, &Value::Null)).collect::<Vec<_>>(),
                    "releases_accepted": decisions.iter().filter(|d| d["accepted"] == true).take(20).map(|d| release_row(d)).collect::<Vec<_>>(),
                    "releases_rejected": decisions.iter().filter(|d| d["accepted"] != true).take(rejected_limit).map(|d| release_row(d)).collect::<Vec<_>>(),
                    "releases_rejected_total": decisions.iter().filter(|d| d["accepted"] != true).count(),
                    "saved_releases": t["saved"].as_array().into_iter().flatten().map(saved_row).collect::<Vec<_>>(),
                }))
            }
            "diagnose" => {
                let id = int("title_id")?;
                let t = self.get(&format!("/api/titles/{id}")).await?;
                let decisions: Vec<&Value> = t["decisions"].as_array().into_iter().flatten().collect();
                let mut reasons: std::collections::BTreeMap<String, (usize, String, String)> = Default::default();
                for d in decisions.iter().filter(|d| d["accepted"] != true) {
                    for r in d["rejections"].as_array().into_iter().flatten() {
                        let e = reasons.entry(r["code"].as_str().unwrap_or("other").to_string()).or_insert((0, r["message"].as_str().unwrap_or("").to_string(), d["release"]["title"].as_str().unwrap_or("").to_string()));
                        e.0 += 1;
                    }
                }
                let mut reasons: Vec<Value> = reasons.into_iter().map(|(code, (n, message, example))| json!({"reason": code, "releases": n, "example_message": message, "example_release": example})).collect();
                reasons.sort_by_key(|r| std::cmp::Reverse(r["releases"].as_u64().unwrap_or(0)));
                let blocklist = self.get("/api/blocklist").await?;
                let attention = self.get("/api/attention").await?;
                let name = t["title"].as_str().unwrap_or("").to_string();
                // Release names use dots where titles use spaces.
                let needle = name.split_whitespace().next().unwrap_or("").to_string();
                let logs = if needle.len() >= 4 { log_lines(&crate::app::log_path(&self.app.data_dir), Some(&needle), Some("warn"), 15).unwrap_or_default() } else { vec![] };
                let has_file = t["file_count"].as_u64().unwrap_or(0) > 0;
                let accepted = decisions.iter().filter(|d| d["accepted"] == true).count();
                let mut findings: Vec<String> = vec![];
                if t["monitored"] != true {
                    findings.push("The title is not monitored, so Spool never searches for it on its own.".into());
                }
                if t["kind"] == "movie" && t["available"] != true {
                    findings.push("The movie has not reached its release date for downloading yet.".into());
                }
                if t["last_search_at"].as_i64().unwrap_or(0) == 0 {
                    findings.push("It has never been searched; run search_releases.".into());
                }
                if decisions.is_empty() {
                    findings.push("No release has been seen for it. The indexers may have nothing, or may be failing; see warnings in the log lines.".into());
                } else if accepted == 0 {
                    findings.push(format!("{} releases were seen and all were rejected; see rejections.", decisions.len()));
                } else if !has_file && t["active"].is_null() {
                    findings.push(format!("{accepted} acceptable releases exist but none is downloading; earlier attempts may have failed or been refused for disk space."));
                }
                Ok(json!({
                    "title": name, "kind": t["kind"], "monitored": t["monitored"], "released": t["available"], "profile": t["profile"], "has_file": has_file, "episodes": if t["kind"] == "series" { json!(format!("{} of {} aired", t["episodes_have"], t["episodes_aired"])) } else { Value::Null },
                    "last_searched": when(&t["last_search_at"]), "downloading_now": t["active"], "findings": findings,
                    "releases_seen": decisions.len(), "releases_acceptable": accepted, "best_acceptable": decisions.iter().filter(|d| d["accepted"] == true).take(3).map(|d| release_row(d)).collect::<Vec<_>>(),
                    "rejections": reasons,
                    "failed_downloads": t["acquisitions"].as_array().into_iter().flatten().filter(|x| x["state"] == "failed" || x["state"] == "import_blocked").take(10).map(|x| json!({"acquisition_id": x["id"], "release": x["release"]["title"], "state": x["state"], "error": x["error"], "at": when(&x["updated_at"])})).collect::<Vec<_>>(),
                    "blocklisted": blocklist.as_array().into_iter().flatten().filter(|b| b["title_id"] == id).map(|b| json!({"blocklist_id": b["id"], "release": b["release_title"], "reason": b["reason"]})).collect::<Vec<_>>(),
                    "attention": attention.as_array().into_iter().flatten().filter(|x| x["item"]["title_id"] == id).map(|x| json!({"attention_id": x["item"]["id"], "message": x["item"]["message"]})).collect::<Vec<_>>(),
                    "saved_releases": t["saved"].as_array().into_iter().flatten().map(saved_row).collect::<Vec<_>>(),
                    "recent_warnings_in_log": logs,
                }))
            }
            "activity" => {
                let act = self.get("/api/activity").await?;
                let limit = a["limit"].as_u64().unwrap_or(20) as usize;
                let rows: Vec<&Value> = act["items"].as_array().into_iter().flatten().collect();
                let live = |r: &&&Value| r["job"].is_object() || ["downloading", "importing", "import_blocked"].contains(&r["acquisition"]["state"].as_str().unwrap_or(""));
                Ok(json!({
                    "queue_paused": act["paused"],
                    "queue": rows.iter().filter(live).map(|r| acquisition_row(&r["acquisition"], &r["job"], &r["title"])).collect::<Vec<_>>(),
                    "finished": rows.iter().filter(|r| !live(r)).take(limit).map(|r| acquisition_row(&r["acquisition"], &Value::Null, &r["title"])).collect::<Vec<_>>(),
                }))
            }
            "logs" => {
                let limit = (a["lines"].as_u64().unwrap_or(60) as usize).clamp(1, 300);
                let (contains, level) = (a["contains"].as_str().map(str::to_string), a["level"].as_str().map(str::to_string));
                let path = crate::app::log_path(&self.app.data_dir);
                let lines = tokio::task::spawn_blocking(move || log_lines(&path, contains.as_deref(), level.as_deref(), limit)).await.map_err(|e| e.to_string())??;
                Ok(json!({"times_are": "UTC", "lines": lines}))
            }
            "archive" => {
                let r = self.get("/api/archive").await?;
                let q = a["query"].as_str().unwrap_or("").to_lowercase();
                let groups: Vec<Value> = r["groups"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|g| q.is_empty() || g["name"].as_str().unwrap_or("").to_lowercase().contains(&q) || g["releases"].as_array().into_iter().flatten().any(|x| x["release"]["title"].as_str().unwrap_or("").to_lowercase().contains(&q)))
                    .take(60)
                    .map(|g| json!({"title": g["name"], "year": g["year"], "kind": g["kind"], "title_id": g["title_id"], "in_library": !g["title_id"].is_null(), "releases": g["releases"].as_array().into_iter().flatten().map(saved_row).collect::<Vec<_>>()}))
                    .collect();
                Ok(json!({"saved_releases": r["count"], "titles": r["groups"].as_array().map(|g| g.len()), "shown": groups}))
            }
            "check_availability" => {
                let r = self.post(&format!("/api/archive/{}/check", int("saved_id")?), json!({})).await?;
                Ok(json!({"still_on_usenet": r["usable"], "articles_found": r["available"][0], "articles_sampled": r["available"][1]}))
            }
            "disk_space" => {
                let r = self.get("/api/space").await?;
                let limit = a["limit"].as_u64().unwrap_or(30) as usize;
                Ok(json!({
                    "free_gb": gb(&r["free"]), "kept_free_gb": gb(&r["kept_free"]), "library_gb": gb(&r["library"]),
                    "recycle_folder": {"files": r["recycled"]["files"], "gb": gb(&r["recycled"]["bytes"]), "emptied_after_days": r["recycled"]["days"]},
                    "waiting_for_space": {"downloads": r["waiting"]["count"], "gb": gb(&r["waiting"]["bytes"])},
                    "largest_titles": r["titles"].as_array().into_iter().flatten().take(limit).map(|t| json!({
                        "title_id": t["title_id"], "title": t["title"], "year": t["year"], "kind": t["kind"], "size_gb": gb(&t["size"]), "over_size_target_gb": gb(&t["over_target"]), "files": t["files"], "monitored": t["monitored"],
                        "watched_in_plex": t["watched"], "last_watched": when(&t["last_viewed_at"]), "can_download_again": t["can_download_again"],
                    })).collect::<Vec<_>>(),
                }))
            }
            "delete_title_files" => {
                let id = int("title_id")?;
                if !confirmed {
                    let t = self.get(&format!("/api/titles/{id}")).await?;
                    return Err(format!("this would delete {} file(s), {} GB, of {}. Call again with confirm=true if the person asked for that.", t["file_count"], gb(&t["size"]), t["title"]));
                }
                let r = self.post(&format!("/api/titles/{id}/free"), json!({})).await?;
                Ok(json!({"title": r["title"], "files_deleted": r["files"], "gb": gb(&r["bytes"]), "note": if r["recycled"] == true { "moved to the recycle folder; space is reclaimed when that is emptied (empty_recycle)" } else { "deleted; the space is free now" }}))
            }
            "plex_report" => {
                let r = self.get("/api/plex").await?;
                Ok(json!({"configured": r["configured"], "last_read": when(&r["synced_at"]), "plex_items": r["plex_items"], "matched": r["matched"],
                    "in_plex_not_in_spool": r["plex_only"].as_array().into_iter().flatten().map(|x| json!({"rating_key": x["rating_key"], "title": x["title"], "year": x["year"], "kind": x["kind"], "path": x["path"], "can_track": x["can_track"]})).collect::<Vec<_>>(),
                    "in_spool_with_files_not_in_plex": r["spool_only"]}))
            }
            "add_title" => {
                let kind = a["kind"].as_str().ok_or("kind is required")?;
                let g = self.app.settings.general();
                let profile = match a["profile"].as_str() {
                    Some(name) => self.profile_id(name, kind)?,
                    None => {
                        let chosen = if kind == "movie" { g.default_movie_profile } else { g.default_series_profile };
                        let want = if kind == "movie" { "movie" } else { "tv" };
                        if chosen != 0 { chosen } else { self.app.db.profiles().map_err(|e| e.to_string())?.iter().find(|p| p.kind == want).map(|p| p.id).ok_or("no quality profile exists")? }
                    }
                };
                let t = self.post("/api/titles", json!({"kind": kind, "tmdb_id": a["tmdb_id"], "tvmaze_id": a["tvmaze_id"], "profile_id": profile, "monitored": a["monitored"].as_bool().unwrap_or(true), "search": a["search"].as_bool().unwrap_or(true)})).await?;
                Ok(json!({"added": t["title"], "year": t["year"], "title_id": t["id"], "monitored": t["monitored"], "path": t["path"], "searching": a["search"].as_bool().unwrap_or(true)}))
            }
            "update_title" => {
                let id = int("title_id")?;
                let t = self.get(&format!("/api/titles/{id}")).await?;
                if let (Some(season), Some(m)) = (a["season"].as_i64(), a["monitored"].as_bool()) {
                    self.api(Method::PATCH, &format!("/api/titles/{id}/seasons/{season}"), Some(json!({"monitored": m}))).await?;
                    return Ok(json!({"title": t["title"], "season": season, "monitored": m}));
                }
                let mut body = json!({});
                if let Some(m) = a["monitored"].as_bool() {
                    body["monitored"] = json!(m);
                }
                if let Some(p) = a["profile"].as_str() {
                    body["profile_id"] = json!(self.profile_id(p, t["kind"].as_str().unwrap_or("movie"))?);
                }
                if body.as_object().is_some_and(|o| o.is_empty()) {
                    return Err("nothing to change: give monitored and/or profile".into());
                }
                self.api(Method::PATCH, &format!("/api/titles/{id}"), Some(body.clone())).await?;
                Ok(json!({"title": t["title"], "changed": body}))
            }
            "remove_title" => {
                let id = int("title_id")?;
                let delete_files = a["delete_files"].as_bool() == Some(true);
                if delete_files && !confirmed {
                    let t = self.get(&format!("/api/titles/{id}")).await?;
                    return Err(format!("this would delete {} file(s), {} GB, of {}. Call again with confirm=true if the person asked for that.", t["file_count"], gb(&t["size"]), t["title"]));
                }
                let r = self.api(Method::DELETE, &format!("/api/titles/{id}?delete_files={delete_files}"), None).await?;
                Ok(json!({"removed": r["deleted"], "files_deleted": delete_files, "saved_releases": "kept in the archive"}))
            }
            "refresh_title" => {
                let id = int("title_id")?;
                self.post(&format!("/api/titles/{id}/refresh"), json!({})).await?;
                let scan = self.post(&format!("/api/titles/{id}/scan"), json!({})).await?;
                Ok(json!({"refreshed": true, "scan": scan}))
            }
            "search_releases" => {
                let id = int("title_id")?;
                let mut body = json!({"grab": a["grab"].as_bool().unwrap_or(false), "compact": a["compact"].as_bool().unwrap_or(false)});
                match (a["season"].as_i64(), a["episode"].as_i64()) {
                    (Some(s), Some(e)) => {
                        let t = self.get(&format!("/api/titles/{id}")).await?;
                        let ep = t["episodes"].as_array().into_iter().flatten().find(|x| x["season"] == s && x["episode"] == e).ok_or_else(|| format!("S{s:02}E{e:02} is not in the episode list"))?;
                        body["episode"] = ep["id"].clone();
                    }
                    (Some(s), None) => body["season"] = json!(s),
                    (None, Some(_)) => return Err("give season along with episode".into()),
                    _ => {}
                }
                let r = self.post(&format!("/api/titles/{id}/search"), body).await?;
                let decisions: Vec<&Value> = r["decisions"].as_array().into_iter().flatten().collect();
                Ok(json!({
                    "result": r["message"], "indexer_errors": r["errors"],
                    "started": r["grabbed"].as_array().into_iter().flatten().map(|x| acquisition_row(x, &Value::Null, &Value::Null)).collect::<Vec<_>>(),
                    "acceptable": decisions.iter().filter(|d| d["accepted"] == true).take(15).map(|d| release_row(d)).collect::<Vec<_>>(),
                    "rejected": decisions.iter().filter(|d| d["accepted"] != true).take(15).map(|d| release_row(d)).collect::<Vec<_>>(),
                    "rejected_total": decisions.iter().filter(|d| d["accepted"] != true).count(),
                }))
            }
            "download_release" => {
                let r = match (a["release_id"].as_i64(), a["saved_id"].as_i64()) {
                    (Some(id), _) => self.post(&format!("/api/decisions/{id}/grab"), json!({})).await?,
                    (_, Some(id)) => self.post(&format!("/api/archive/{id}/grab"), json!({})).await?,
                    _ => return Err("give release_id or saved_id".into()),
                };
                if r["acquisition"].is_null() {
                    return Ok(json!({"started": false, "note": "that release is already downloading or was just started"}));
                }
                Ok(json!({"started": true, "download": acquisition_row(&r["acquisition"], &Value::Null, &Value::Null)}))
            }
            "manage_download" => {
                let action = a["action"].as_str().ok_or("action is required")?;
                match action {
                    "pause_all" => return self.post("/api/queue/pause", json!({})).await,
                    "resume_all" => return self.post("/api/queue/resume", json!({})).await,
                    _ => {}
                }
                let id = int("acquisition_id")?;
                let api_action = if action == "cancel_and_find_another" { "cancel_blocklist" } else { action };
                self.post(&format!("/api/activity/{id}/{api_action}"), json!({})).await?;
                Ok(json!({"done": action, "acquisition_id": id}))
            }
            "unblock_release" => self.api(Method::DELETE, &format!("/api/blocklist/{}", int("blocklist_id")?), None).await,
            "dismiss_attention" => self.post(&format!("/api/attention/{}/dismiss", int("attention_id")?), json!({})).await,
            "plex_sync" => self.post("/api/plex/sync", json!({})).await,
            "plex_track" => {
                let t = self.post("/api/plex/track", json!({"rating_key": a["rating_key"].as_str().ok_or("rating_key is required")?})).await?;
                Ok(json!({"tracking": t["title"], "title_id": t["id"], "path": t["path"], "monitored": t["monitored"]}))
            }
            "run_task" => {
                let name = a["name"].as_str().ok_or("name is required")?;
                self.post(&format!("/api/tasks/{name}"), json!({})).await?;
                Ok(json!({"started": name, "note": "runs in the background; its result appears in status warnings or logs"}))
            }
            "delete_file" => {
                let id = int("file_id")?;
                if !confirmed {
                    return Err("deleting a media file needs confirm=true, and only when the person asked for it".into());
                }
                self.api(Method::DELETE, &format!("/api/files/{id}"), None).await
            }
            "empty_recycle" => {
                let info = self.get("/api/recycle").await?;
                if !confirmed {
                    return Err(format!("this would permanently delete {} recycled file(s), {} GB. Call again with confirm=true if the person asked for that.", info["files"], gb(&info["bytes"])));
                }
                let r = self.post("/api/recycle/empty", json!({})).await?;
                Ok(json!({"freed_gb": gb(&r["freed"])}))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

fn saved_row(n: &Value) -> Value {
    json!({"saved_id": n["id"], "name": n["release"]["title"], "quality": quality(&n["quality"]), "size_gb": gb(&n["release"]["size"]), "covers": n["covers"], "kept_as": n["source"], "saved": when(&n["fetched_at"]),
        "on_usenet": match n["available"].as_array() { Some(a) => json!(format!("{} of {} sampled articles found", a[0], a[1])), None => json!("not checked") }})
}

fn tool_text(v: &Value, is_error: bool) -> Value {
    let text = match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}
