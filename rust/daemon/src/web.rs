/*! `agent-boxd web`: the phone app. Loopback only, so it's reached through Coder's owner-only port forwarding;
bridges /ws to the daemon and web-pushes new inbox items. */

use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::{protocol::Role, Message, WebSocket};

use crate::{config, util};

const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/** The daemon calls the phone may make; anything else gets an error reply. */
const ALLOWED: &[&str] = &[
    "hello", "subscribe", "transcript_read", "transcript_unwatch", "chat_new", "chat_start", "chat_stop", "chat_send",
    "chat_keys", "inbox_read", "config_get",
];

/** Chat fields the phone never shows, dropped from every model it gets. */
const CHAT_DROP: &[&str] = &["tmux", "session", "transcript", "created_at", "launched_at", "forked_from", "base", "model", "feature"];

pub fn run() -> anyhow::Result<()> {
    config::load();
    let port = config::count("phone_port", 8792) as u16;
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    eprintln!("phone: http://127.0.0.1:{port}");
    let push = Arc::new(Push::load());
    let watcher = push.clone();
    std::thread::spawn(move || loop {
        watch_inbox(&watcher);
        std::thread::sleep(Duration::from_secs(5));
    });
    for stream in listener.incoming().flatten() {
        let push = push.clone();
        std::thread::spawn(move || handle(stream, &push));
    }
    Ok(())
}

struct Req {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_request(s: &mut TcpStream) -> Option<Req> {
    s.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
        let n = s.read(&mut chunk).ok().filter(|n| *n > 0)?;
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let (path, query) = target.split_once('?').map(|(p, q)| (p.to_string(), q.to_string())).unwrap_or((target, String::new()));
    let headers: HashMap<String, String> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let len = headers.get("content-length").and_then(|l| l.parse::<usize>().ok()).unwrap_or(0).min(1 << 20);
    let mut body = buf[end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut chunk).ok().filter(|n| *n > 0)?;
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Some(Req { method, path, query, headers, body })
}

fn respond(s: &mut TcpStream, status: u16, kind: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(body));
}

fn respond_json(s: &mut TcpStream, v: &Value) {
    respond(s, 200, "application/json", v.to_string().as_bytes());
}

fn handle(mut s: TcpStream, push: &Push) {
    let Some(req) = read_request(&mut s) else { return };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/ws") => bridge(s, &req),
        ("GET", "/api/push/key") => respond_json(&mut s, &json!({ "key": push.public_key() })),
        ("POST", "/api/push/subscribe") => match serde_json::from_slice::<Value>(&req.body) {
            Ok(sub) if sub["endpoint"].is_string() => {
                push.subscribe(sub);
                respond_json(&mut s, &json!({ "ok": true }))
            }
            _ => respond(&mut s, 400, "text/plain", b"expected a PushSubscription\n"),
        },
        ("POST", "/api/push/test") => {
            let sent = push.send_all(&json!({ "title": "a.box", "body": "Notifications from your box work." }));
            respond_json(&mut s, &json!({ "sent": sent }))
        }
        ("GET", path) => static_file(&mut s, path),
        _ => respond(&mut s, 404, "text/plain", b"not found\n"),
    }
}

fn static_file(s: &mut TcpStream, path: &str) {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|p| p == ".." || p.starts_with('.')) {
        return respond(s, 404, "text/plain", b"not found\n");
    }
    let file: PathBuf = util::agent_box_dir().join("web").join("phone").join(rel);
    let Ok(body) = std::fs::read(&file) else { return respond(s, 404, "text/plain", b"not found\n") };
    let kind = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    respond(s, 200, kind, &body)
}

enum Shaped {
    Model(String),
    Line(String),
    Drop,
}

/** A daemon line as the phone gets it: models slimmed, tool input and output cut to `phone_text_max`, and
events it has no use for (hooks, terminals, app actions) dropped. */
fn shape(line: &str) -> Shaped {
    let Ok(mut v) = serde_json::from_str::<Value>(line) else { return Shaped::Drop };
    let max = config::count("phone_text_max", 1500);
    match v.get("event").and_then(Value::as_str) {
        Some("model") => {
            if let Some(chats) = v.pointer_mut("/data/chats").and_then(Value::as_array_mut) {
                for c in chats.iter_mut().filter_map(Value::as_object_mut) {
                    for k in CHAT_DROP {
                        c.remove(*k);
                    }
                }
            }
            return Shaped::Model(v.to_string());
        }
        Some("transcript") => cut_entries(v.pointer_mut("/data/entries"), max),
        Some(_) => return Shaped::Drop,
        None => cut_entries(v.pointer_mut("/result/entries"), max),
    }
    Shaped::Line(v.to_string())
}

fn cut_entries(entries: Option<&mut Value>, max: usize) {
    let Some(Value::Array(entries)) = entries else { return };
    for e in entries.iter_mut() {
        if let Some(o) = e.as_object_mut() {
            for k in ["parentUuid", "cwd", "version", "userType", "requestId", "gitBranch", "sessionId", "slug"] {
                o.remove(k);
            }
        }
        if let Some(Value::Array(blocks)) = e.pointer_mut("/message/content") {
            for b in blocks.iter_mut().filter(|b| matches!(b["type"].as_str(), Some("tool_use" | "tool_result"))) {
                cut_strings(b, max);
            }
        }
    }
}

fn cut_strings(v: &mut Value, max: usize) {
    match v {
        Value::String(t) if t.len() > max => {
            let mut at = max;
            while !t.is_char_boundary(at) {
                at -= 1;
            }
            t.truncate(at);
            t.push_str(" …");
        }
        Value::Array(a) => a.iter_mut().for_each(|x| cut_strings(x, max)),
        Value::Object(o) => {
            o.remove("signature");
            o.values_mut().for_each(|x| cut_strings(x, max));
        }
        _ => {}
    }
}

/** One phone connection to the daemon, models sent at most every `phone_model_ms`. With `?z=1` everything
sent is one zlib stream in binary frames, flushed after each line. */
fn bridge(mut s: TcpStream, req: &Req) {
    let Some(key) = req.headers.get("sec-websocket-key") else { return respond(&mut s, 400, "text/plain", b"websocket only\n") };
    let digest = sha1_smol::Sha1::from(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11")).digest().bytes();
    let accept = base64::engine::general_purpose::STANDARD.encode(digest);
    let head = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    let zlib = req.query.split('&').any(|p| p == "z=1");
    let _ = s.set_nodelay(true);
    if s.set_read_timeout(Some(Duration::from_millis(40))).is_err() {
        return;
    }
    let mut ws = WebSocket::from_raw_socket(s, Role::Server, None);
    let Ok(mut daemon) = UnixStream::connect(util::socket_path()) else {
        let _ = ws.close(None);
        let _ = ws.flush();
        return;
    };
    let (tx, rx) = channel::<Option<String>>();
    if let Ok(reader) = daemon.try_clone() {
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if tx.send(Some(line)).is_err() {
                    return;
                }
            }
            let _ = tx.send(None);
        });
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let mut send = |ws: &mut WebSocket<TcpStream>, text: &str| -> bool {
        let msg = if zlib {
            if enc.write_all(text.as_bytes()).and_then(|_| enc.write_all(b"\n")).and_then(|_| enc.flush()).is_err() {
                return false;
            }
            Message::binary(std::mem::take(enc.get_mut()))
        } else {
            Message::text(text.to_string())
        };
        ws.send(msg).is_ok()
    };
    let throttle = config::ms("phone_model_ms", 1000);
    let mut model: Option<String> = None;
    let mut sent_model = String::new();
    let mut model_at = Instant::now() - Duration::from_secs(3600);
    let mut ping_at = Instant::now();
    'conn: loop {
        let mut replies = vec![];
        match ws.read() {
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                let method = v["method"].as_str().unwrap_or("");
                if ALLOWED.contains(&method) {
                    if daemon.write_all(format!("{}\n", t.as_str()).as_bytes()).is_err() {
                        break;
                    }
                } else {
                    replies.push(json!({ "id": v["id"], "error": format!("{method} isn't allowed from the phone") }).to_string());
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        loop {
            match rx.try_recv() {
                Ok(Some(line)) => match shape(&line) {
                    Shaped::Model(m) => model = Some(m),
                    Shaped::Line(l) => replies.push(l),
                    Shaped::Drop => {}
                },
                Ok(None) => break 'conn,
                Err(_) => break,
            }
        }
        for r in replies {
            if !send(&mut ws, &r) {
                break 'conn;
            }
        }
        if model.is_some() && model_at.elapsed() >= throttle {
            let m = model.take().unwrap_or_default();
            if m != sent_model {
                if !send(&mut ws, &m) {
                    break;
                }
                sent_model = m;
            }
            model_at = Instant::now();
        }
        if ping_at.elapsed() >= Duration::from_secs(20) {
            if ws.send(Message::Ping(Vec::new().into())).is_err() {
                break;
            }
            ping_at = Instant::now();
        }
    }
    let _ = daemon.shutdown(std::net::Shutdown::Both);
    let _ = ws.close(None);
    let _ = ws.flush();
}

/** Web push: a VAPID key and the phone's subscriptions, kept in ~/.agent-box/phone.json. */
struct Push {
    state: Mutex<Value>,
    key: p256::ecdsa::SigningKey,
    subject: String,
}

fn push_file() -> PathBuf {
    util::agent_home().join("phone.json")
}

impl Push {
    fn load() -> Push {
        let mut state: Value = std::fs::read_to_string(push_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
        let saved = state["vapid"].as_str().and_then(|k| B64URL.decode(k).ok()).and_then(|b| p256::ecdsa::SigningKey::from_slice(&b).ok());
        let key = match saved {
            Some(k) => k,
            None => {
                let k = p256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
                state["vapid"] = json!(B64URL.encode(k.to_bytes()));
                k
            }
        };
        let email = util::run("git", &["config", "--global", "user.email"], &[], None, Duration::from_secs(5)).stdout.trim().to_string();
        let subject = config::get("phone_push_subject").and_then(|v| v.as_str().map(str::to_string)).unwrap_or(format!("mailto:{email}"));
        let push = Push { state: Mutex::new(state), key, subject };
        push.save(&push.state.lock().unwrap());
        push
    }

    fn save(&self, state: &Value) {
        let path = push_file();
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(state).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn public_key(&self) -> String {
        B64URL.encode(self.key.verifying_key().to_encoded_point(false).as_bytes())
    }

    fn subscribe(&self, sub: Value) {
        let mut state = self.state.lock().unwrap();
        let mut subs = state["subs"].as_array().cloned().unwrap_or_default();
        subs.retain(|s| s["endpoint"] != sub["endpoint"]);
        subs.push(sub);
        state["subs"] = Value::Array(subs);
        self.save(&state);
    }

    /** Sends `payload` to every subscription, dropping the ones the push service says are gone. */
    fn send_all(&self, payload: &Value) -> usize {
        let subs = self.state.lock().unwrap()["subs"].as_array().cloned().unwrap_or_default();
        let body = payload.to_string();
        let mut sent = 0;
        let mut gone = vec![];
        for sub in &subs {
            match self.send(sub, body.as_bytes()) {
                Ok(code) if (200..300).contains(&code) => sent += 1,
                Ok(404 | 410) => gone.push(sub["endpoint"].clone()),
                Ok(code) => eprintln!("phone: push got HTTP {code}"),
                Err(e) => eprintln!("phone: push failed: {e}"),
            }
        }
        if !gone.is_empty() {
            let mut state = self.state.lock().unwrap();
            let mut subs = state["subs"].as_array().cloned().unwrap_or_default();
            subs.retain(|s| !gone.contains(&s["endpoint"]));
            state["subs"] = Value::Array(subs);
            self.save(&state);
        }
        sent
    }

    fn send(&self, sub: &Value, payload: &[u8]) -> Result<u16, String> {
        let endpoint = sub["endpoint"].as_str().ok_or("no endpoint")?;
        let decode = |k: &str| {
            sub["keys"][k].as_str().map(|s| s.trim_end_matches('=')).and_then(|s| B64URL.decode(s).ok()).ok_or(format!("bad {k}"))
        };
        let body = encrypt(&decode("p256dh")?, &decode("auth")?, payload)?;
        let origin = endpoint.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
        let jwt = self.jwt(&origin);
        let mut child = std::process::Command::new("curl")
            .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15", "-X", "POST", "--data-binary", "@-"])
            .args(["-H", "TTL: 86400", "-H", "Urgency: high", "-H", "Content-Encoding: aes128gcm", "-H", "Content-Type: application/octet-stream"])
            .args(["-H", &format!("Authorization: vapid t={jwt}, k={}", self.public_key()), endpoint])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(mut w) = child.stdin.take() {
            w.write_all(&body).map_err(|e| e.to_string())?;
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<u16>().map_err(|_| String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn jwt(&self, aud: &str) -> String {
        use p256::ecdsa::signature::Signer;
        let header = B64URL.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64URL.encode(json!({ "aud": aud, "exp": util::now() as u64 + 12 * 3600, "sub": self.subject }).to_string());
        let msg = format!("{header}.{claims}");
        let sig: p256::ecdsa::Signature = self.key.sign(msg.as_bytes());
        format!("{msg}.{}", B64URL.encode(sig.to_bytes()))
    }
}

/** RFC 8291 (aes128gcm) encryption of a push message for a subscription's `p256dh` key and `auth` secret. */
fn encrypt(ua_public: &[u8], auth: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use rand_core::RngCore;
    let ua = p256::PublicKey::from_sec1_bytes(ua_public).map_err(|e| e.to_string())?;
    let secret = p256::ecdh::EphemeralSecret::random(&mut rand_core::OsRng);
    let as_public = secret.public_key().to_encoded_point(false);
    let shared = secret.diffie_hellman(&ua);
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public.as_bytes());
    let mut ikm = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(auth), shared.raw_secret_bytes()).expand(&info, &mut ikm).map_err(|e| e.to_string())?;
    let mut salt = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut salt);
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), &ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    hk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|e| e.to_string())?;
    hk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|e| e.to_string())?;
    let mut data = plain.to_vec();
    data.push(2);
    let cipher = aes_gcm::Aes128Gcm::new_from_slice(&cek).map_err(|e| e.to_string())?;
    let sealed = cipher.encrypt(aes_gcm::Nonce::from_slice(&nonce), data.as_ref()).map_err(|e| e.to_string())?;
    let mut out = salt.to_vec();
    out.extend_from_slice(&4096u32.to_be_bytes());
    out.push(as_public.as_bytes().len() as u8);
    out.extend_from_slice(as_public.as_bytes());
    out.extend_from_slice(&sealed);
    Ok(out)
}

/** Pushes each inbox item that turns up unread once it stays unread `phone_push_delay_secs`, so what's
already seen at the Mac doesn't buzz the phone. Returns when the daemon goes. */
fn watch_inbox(push: &Push) {
    let Ok(stream) = UnixStream::connect(util::socket_path()) else { return };
    let Ok(mut w) = stream.try_clone() else { return };
    if w.write_all(b"{\"id\":1,\"method\":\"subscribe\",\"params\":{\"from_seq\":null}}\n").is_err() {
        return;
    }
    let (tx, rx) = channel::<Vec<Value>>();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if !line.contains("\"event\":\"model\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if tx.send(v["data"]["inbox"].as_array().cloned().unwrap_or_default()).is_err() {
                break;
            }
        }
    });
    let delay = config::secs("phone_push_delay_secs", 15);
    let mut seen: Option<HashMap<i64, i64>> = None;
    let mut due: HashMap<i64, (Instant, i64)> = HashMap::new();
    let mut inbox: Vec<Value> = vec![];
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(items) => {
                inbox = items;
                let first = seen.is_none();
                let seen = seen.get_or_insert_with(HashMap::new);
                for it in &inbox {
                    let (Some(id), rev) = (it["id"].as_i64(), it["rev"].as_i64().unwrap_or(0)) else { continue };
                    if seen.get(&id).is_none_or(|r| rev > *r) {
                        if !first {
                            due.insert(id, (Instant::now() + delay, rev));
                        }
                        seen.insert(id, rev);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        let ready: Vec<(i64, i64)> = due.iter().filter(|(_, (t, _))| *t <= now).map(|(id, (_, rev))| (*id, *rev)).collect();
        for (id, rev) in ready {
            due.remove(&id);
            let Some(it) = inbox.iter().find(|i| i["id"].as_i64() == Some(id) && i["rev"].as_i64().unwrap_or(0) == rev) else { continue };
            let mut body = Value::String(it["text"].as_str().unwrap_or("").to_string());
            cut_strings(&mut body, 300);
            push.send_all(&json!({ "title": it["title"], "body": body, "chat": it["chat"], "tag": format!("inbox-{id}") }));
        }
    }
}
