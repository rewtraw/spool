//! An in-process NNTP server and helpers for building fake posts. Test support only.

use crate::yenc;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Default)]
pub struct ServerState {
    pub articles: HashMap<String, Vec<u8>>,
    /// Message ids this server pretends not to have.
    pub missing: HashSet<String>,
    /// (user, password) required, if any.
    pub auth: Option<(String, String)>,
    /// Close the connection after serving this many more bodies, once.
    pub drop_after: Option<usize>,
    /// Refuse every new connection.
    pub down: bool,
    pub delay_ms: u64,
    /// How often each message id was served.
    pub served: HashMap<String, usize>,
}

#[derive(Clone)]
pub struct FakeServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<ServerState>>,
    pub connections: Arc<AtomicUsize>,
}

impl FakeServer {
    pub async fn start(articles: HashMap<String, Vec<u8>>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(ServerState { articles, ..Default::default() }));
        let connections = Arc::new(AtomicUsize::new(0));
        let (st, cn) = (state.clone(), connections.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { break };
                if st.lock().down {
                    drop(sock);
                    continue;
                }
                cn.fetch_add(1, Ordering::SeqCst);
                let st = st.clone();
                tokio::spawn(async move {
                    let _ = serve(sock, st).await;
                });
            }
        });
        FakeServer { addr, state, connections }
    }

    pub fn config(&self, id: &str, priority: u32, connections: u32) -> crate::nntp::ServerConfig {
        let auth = self.state.lock().auth.clone();
        crate::nntp::ServerConfig {
            id: id.into(),
            name: id.into(),
            host: self.addr.ip().to_string(),
            port: self.addr.port(),
            tls: false,
            username: auth.as_ref().map(|a| a.0.clone()).unwrap_or_default(),
            password: auth.as_ref().map(|a| a.1.clone()).unwrap_or_default(),
            connections,
            priority,
            ..Default::default()
        }
    }

    pub fn total_served(&self) -> usize {
        self.state.lock().served.values().sum()
    }

    pub fn max_served_per_article(&self) -> usize {
        self.state.lock().served.values().copied().max().unwrap_or(0)
    }
}

async fn serve(sock: tokio::net::TcpStream, state: Arc<Mutex<ServerState>>) -> std::io::Result<()> {
    let (r, mut w) = sock.into_split();
    let mut lines = BufReader::new(r).lines();
    w.write_all(b"200 fake server ready\r\n").await?;
    let mut authed = state.lock().auth.is_none();
    let mut user = String::new();
    while let Some(line) = lines.next_line().await? {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("AUTHINFO USER ") {
            user = line[14..].to_string();
            w.write_all(b"381 password required\r\n").await?;
        } else if upper.starts_with("AUTHINFO PASS ") {
            let ok = state.lock().auth.as_ref().is_none_or(|(u, p)| *u == user && *p == line[14..]);
            authed = ok;
            w.write_all(if ok { b"281 ok\r\n" } else { b"481 authentication failed\r\n" }).await?;
        } else if upper.starts_with("BODY ") {
            if !authed {
                w.write_all(b"480 authentication required\r\n").await?;
                continue;
            }
            let id = line[5..].trim().trim_matches(['<', '>']).to_string();
            let (reply, delay, drop_now) = {
                let mut s = state.lock();
                let drop_now = match s.drop_after {
                    Some(0) => {
                        s.drop_after = None;
                        true
                    }
                    Some(n) => {
                        s.drop_after = Some(n - 1);
                        false
                    }
                    None => false,
                };
                let reply = if s.missing.contains(&id) { None } else { s.articles.get(&id).cloned() };
                if reply.is_some() && !drop_now {
                    *s.served.entry(id.clone()).or_default() += 1;
                }
                (reply, s.delay_ms, drop_now)
            };
            if drop_now {
                return Ok(());
            }
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            match reply {
                Some(body) => {
                    w.write_all(format!("222 0 <{id}>\r\n").as_bytes()).await?;
                    w.write_all(&body).await?;
                    w.write_all(b".\r\n").await?;
                }
                None => w.write_all(b"430 no such article\r\n").await?,
            }
        } else if upper.starts_with("STAT ") {
            if !authed {
                w.write_all(b"480 authentication required\r\n").await?;
                continue;
            }
            let id = line[5..].trim().trim_matches(['<', '>']).to_string();
            let has = {
                let s = state.lock();
                !s.missing.contains(&id) && s.articles.contains_key(&id)
            };
            w.write_all(if has { format!("223 0 <{id}>\r\n") } else { "430 no such article\r\n".to_string() }.as_bytes()).await?;
        } else if upper.starts_with("QUIT") {
            w.write_all(b"205 bye\r\n").await?;
            return Ok(());
        } else {
            w.write_all(b"500 unknown command\r\n").await?;
        }
    }
    Ok(())
}

/// A set of fake posted files: the NZB that describes them and the articles a server would hold.
#[derive(Default)]
pub struct Post {
    pub articles: HashMap<String, Vec<u8>>,
    files_xml: String,
    counter: usize,
    pub password: Option<String>,
}

impl Post {
    pub fn new() -> Post {
        Post::default()
    }

    /// Add a file under `posted_name`, split into segments of `segment_size` decoded bytes.
    /// Returns the message ids, in order.
    pub fn add_file(&mut self, posted_name: &str, data: &[u8], segment_size: usize) -> Vec<String> {
        self.add_file_with_subject(posted_name, &format!("\"{posted_name}\" yEnc"), data, segment_size)
    }

    pub fn add_file_with_subject(&mut self, posted_name: &str, subject: &str, data: &[u8], segment_size: usize) -> Vec<String> {
        self.counter += 1;
        let chunks: Vec<&[u8]> = if data.is_empty() { vec![&data[..]] } else { data.chunks(segment_size).collect() };
        let total = chunks.len() as u32;
        let mut ids = vec![];
        let mut segs = String::new();
        let mut offset = 0u64;
        for (i, chunk) in chunks.iter().enumerate() {
            let id = format!("f{}s{}@fake.test", self.counter, i + 1);
            let body = yenc::encode(posted_name, data.len() as u64, i as u32 + 1, total, offset, chunk);
            segs.push_str(&format!("<segment bytes=\"{}\" number=\"{}\">{}</segment>", body.len(), i + 1, id));
            self.articles.insert(id.clone(), body);
            ids.push(id);
            offset += chunk.len() as u64;
        }
        let subject = subject.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
        self.files_xml.push_str(&format!(
            "<file poster=\"t@t\" date=\"1700000000\" subject=\"{subject} (1/{total})\"><groups><group>alt.binaries.test</group></groups><segments>{segs}</segments></file>"
        ));
        ids
    }

    pub fn nzb(&self) -> Vec<u8> {
        let head = match &self.password {
            Some(p) => format!("<head><meta type=\"password\">{p}</meta></head>"),
            None => String::new(),
        };
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">{head}{}</nzb>", self.files_xml).into_bytes()
    }
}

/// Deterministic pseudo-random bytes, so tests need no RNG crate.
pub fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}
