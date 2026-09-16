//! Contract L through the built binary: schema links resolved by the verbs that
//! load a configuration, the cache a pinned remote link resolves through, the
//! `schemas` verbs over it, and the transport — TLS, proxies, timeouts.
//!
//! Every origin, proxy and silent listener is a loopback server a journey starts
//! itself, so no request leaves the host. Every variable the resolver or the
//! client reads is removed from the binary's environment before a journey sets
//! the ones it means, so nothing in the environment the suite runs under decides
//! a journey.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use onemessagebus_agent::codec::onejudge;
use serde_json::{json, Value};

use crate::support::{onemessagebus, Run};

/// Every variable the resolver or its HTTP client reads.
const AMBIENT: &[&str] = &[
    "ONEMESSAGEBUS_SCHEMA_CACHE_DIR",
    "ONEMESSAGEBUS_SCHEMA_TTL",
    "ONEMESSAGEBUS_SCHEMA_REFRESH",
    "XDG_CACHE_HOME",
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    // The host running the journeys may be inside a run; `serve` must not read it.
    "ONEPIPELINE_RUN_ID",
    "ONEPIPELINE_CHANNEL_ASKER",
    "ONEPIPELINE_SERVE_SESSION_SECONDS",
];

/// The id every bundle here publishes, and a document for it at `version`:
/// `8.1` requires `hello`, and anything newer requires `world` as well, so which
/// bundle a verb registered shows in what it accepts.
const FRAME: &str = "demo.frame@1";

fn bundle(version: &str) -> String {
    let required = if version == "8.1" || version == "8" {
        json!(["hello"])
    } else {
        json!(["hello", "world"])
    };
    json!({
        "version": version,
        "description": "the demo frame grammar",
        "schemas": [{"id": FRAME, "schema": {"type": "object", "required": required}}]
    })
    .to_string()
}

/// A scratch directory: a cache, a configuration naming links, and the binary
/// run from there with only the variables a journey names.
pub(crate) struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    pub(crate) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn text(&self, name: &str) -> String {
        self.path(name).to_str().expect("a UTF-8 path").to_owned()
    }

    fn cache(&self) -> String {
        self.text("cache")
    }

    /// `onemessagebus.yaml` naming `links`, over a local transport, with a
    /// `frames` queue validated against the linked id.
    fn config(&self, links: &[&str]) -> String {
        let links: Vec<String> = links
            .iter()
            .map(|link| format!("  - {}\n", serde_json::to_string(link).expect("a string")))
            .collect();
        std::fs::write(
            self.path("onemessagebus.yaml"),
            format!(
                "version: 1\ntransport: {{kind: local, dir: {}}}\nqueues:\n  frames: {{schema: {FRAME}}}\nschemas:\n{}",
                serde_json::to_string(&self.path("channel")).expect("a path"),
                links.concat()
            ),
        )
        .expect("the configuration is written");
        self.text("onemessagebus.yaml")
    }

    /// Run the binary with `args` and `stdin`, the cache at [`cache`](Self::cache)
    /// unless `env` names the variable itself.
    pub(crate) fn run(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Run {
        let started = self.spawn(args, stdin, env);
        let output = started.wait_with_output().expect("the binary exits");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).expect("UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("UTF-8"),
        }
    }

    fn spawn(
        &self,
        args: &[&str],
        stdin: Option<&str>,
        env: &[(&str, &str)],
    ) -> std::process::Child {
        let mut command = onemessagebus();
        for name in AMBIENT {
            command.env_remove(name);
        }
        if !env.iter().any(|(name, _)| *name == "HOME") {
            command.env("HOME", self.path("home"));
        }
        if !env
            .iter()
            .any(|(name, _)| *name == "ONEMESSAGEBUS_SCHEMA_CACHE_DIR")
        {
            command.env("ONEMESSAGEBUS_SCHEMA_CACHE_DIR", self.cache());
        }
        command
            .args(args)
            .envs(env.iter().copied())
            .current_dir(self.dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("the binary spawns");
        let mut handle = child.stdin.take().expect("a stdin pipe");
        if let Some(text) = stdin {
            let _ = handle.write_all(text.as_bytes());
        }
        drop(handle);
        child
    }

    /// `schema check <FRAME> --config <config>` over `record`.
    fn check(&self, config: &str, record: &Value, env: &[(&str, &str)]) -> Run {
        self.run(
            &["schema", "check", FRAME, "--config", config],
            Some(&record.to_string()),
            env,
        )
    }

    /// What `schemas` lists.
    fn cached(&self, env: &[(&str, &str)]) -> Value {
        let listed = self.run(&["schemas"], None, env);
        assert_eq!(listed.code, 0, "{}", listed.stderr);
        serde_json::from_str(&listed.stdout).expect("schemas prints JSON")
    }

    fn versions(&self) -> Vec<String> {
        self.cached(&[])["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["version"].as_str().expect("a version").to_owned())
            .collect()
    }
}

/// One request an origin saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: BTreeMap<String, String>,
}

impl Seen {
    fn conditional(&self) -> bool {
        self.headers.contains_key("if-none-match") || self.headers.contains_key("if-modified-since")
    }
}

/// What an origin serves, and what it saw.
struct Served {
    body: String,
    etag: String,
    seen: Vec<Seen>,
}

/// A loopback HTTP origin — or HTTPS, over rustls — serving one bundle with an
/// `ETag` and a `Last-Modified`, answering `304` to a request whose
/// `If-None-Match` names the current tag.
struct Origin {
    addr: SocketAddr,
    served: Arc<Mutex<Served>>,
    stop: Arc<AtomicBool>,
    scheme: &'static str,
}

impl Origin {
    fn http(version: &str) -> Self {
        Self::start(version, None)
    }

    fn https(version: &str, tls: Arc<rustls::ServerConfig>) -> Self {
        Self::start(version, Some(tls))
    }

    fn start(version: &str, tls: Option<Arc<rustls::ServerConfig>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("an address");
        let served = Arc::new(Mutex::new(Served {
            body: bundle(version),
            etag: format!("\"{version}\""),
            seen: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let origin = Self {
            addr,
            served: Arc::clone(&served),
            stop: Arc::clone(&stop),
            scheme: if tls.is_some() { "https" } else { "http" },
        };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let served = Arc::clone(&served);
                let tls = tls.clone();
                std::thread::spawn(move || match tls {
                    None => answer(stream, &served),
                    Some(config) => {
                        let Ok(connection) = rustls::ServerConnection::new(config) else {
                            return;
                        };
                        answer(rustls::StreamOwned::new(connection, stream), &served);
                    }
                });
            }
        });
        origin
    }

    /// The link to the bundle, pinned when `pin` names one.
    fn link(&self, pin: Option<&str>) -> String {
        let url = format!("{}://{}/frames.json", self.scheme, self.addr);
        match pin {
            Some(pin) => format!("{url}@{pin}"),
            None => url,
        }
    }

    fn url(&self) -> String {
        self.link(None)
    }

    fn serve(&self, version: &str) {
        let mut served = self.served.lock().expect("the origin's state");
        served.body = bundle(version);
        served.etag = format!("\"{version}\"");
    }

    /// Serve `text` as the document, under a tag of its own.
    fn serve_text(&self, text: String) {
        let mut served = self.served.lock().expect("the origin's state");
        served.etag = format!("\"{}\"", text.len());
        served.body = text;
    }

    fn seen(&self) -> Vec<Seen> {
        self.served.lock().expect("the origin's state").seen.clone()
    }

    /// Close the port: every later connection is refused.
    fn down(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

/// Read one request off `stream` and answer it from `served`.
fn answer(mut stream: impl Read + Write, served: &Mutex<Served>) {
    let Some((path, headers)) = read_head(&mut stream) else {
        return;
    };
    let response = {
        let mut served = served.lock().expect("the origin's state");
        served.seen.push(Seen {
            path: path.clone(),
            headers: headers.clone(),
        });
        let canned = |status: &str, headers: &str| {
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")
        };
        let redirect = match path.as_str() {
            "/moved.json" => Some("/frames.json"),
            "/away.json" => Some("http://example.org/frames.json"),
            "/loop.json" => Some("/loop.json"),
            _ => None,
        };
        if let Some(location) = redirect {
            canned("302 Found", &format!("Location: {location}\r\n"))
        } else if path == "/nowhere.json" {
            canned("302 Found", "")
        } else if path == "/gone.json" {
            canned("404 Not Found", "")
        } else if path == "/unchanged.json" {
            canned("304 Not Modified", "")
        } else if headers.get("if-none-match") == Some(&served.etag) {
            format!(
                "HTTP/1.1 304 Not Modified\r\nETag: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                served.etag
            )
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: {}\r\nLast-Modified: Tue, 01 Sep 2026 09:12:44 GMT\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                served.etag,
                served.body.len(),
                served.body
            )
        }
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// The request line's target and the headers, lower-cased by name.
fn read_head(stream: &mut impl Read) -> Option<(String, BTreeMap<String, String>)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let target = line.split_whitespace().nth(1)?.to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Some((target, headers))
}

/// A loopback `CONNECT` proxy that logs each request line; `silent`, it accepts
/// the connection, reads the `CONNECT`, and never answers.
pub(crate) struct Proxy {
    addr: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
}

impl Proxy {
    pub(crate) fn start(silent: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("an address");
        let log = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&log);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut client) = stream else { continue };
                let log = Arc::clone(&kept);
                std::thread::spawn(move || {
                    let Some((target, _)) = read_head(&mut client) else {
                        return;
                    };
                    log.lock()
                        .expect("the log")
                        .push(format!("CONNECT {target}"));
                    if silent {
                        std::thread::sleep(Duration::from_secs(120));
                        return;
                    }
                    let Ok(upstream) = TcpStream::connect(&target) else {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
                        return;
                    };
                    let _ = client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                    pipe(client, upstream);
                });
            }
        });
        Self { addr, log }
    }

    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(crate) fn log(&self) -> Vec<String> {
        self.log.lock().expect("the log").clone()
    }
}

/// Copy bytes both ways between two streams until either side closes.
fn pipe(a: TcpStream, b: TcpStream) {
    let (mut a_read, mut b_write) = (
        a.try_clone().expect("a clone"),
        b.try_clone().expect("a clone"),
    );
    let forward = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_read, &mut b_write);
        let _ = b_write.shutdown(Shutdown::Write);
    });
    let (mut b_read, mut a_write) = (b, a);
    let _ = std::io::copy(&mut b_read, &mut a_write);
    let _ = a_write.shutdown(Shutdown::Write);
    let _ = forward.join();
}

/// A CA, and a server configuration presenting a leaf for `127.0.0.1` it signed;
/// the CA's PEM is written to `ca_file`.
fn tls_origin_config(ca_file: &Path) -> Arc<rustls::ServerConfig> {
    let ca_key = rcgen::KeyPair::generate().expect("a CA key");
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "onemessagebus journeys CA");
    let ca = ca_params.self_signed(&ca_key).expect("a CA certificate");
    std::fs::write(ca_file, ca.pem()).expect("the CA is written");
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_key = rcgen::KeyPair::generate().expect("a leaf key");
    let leaf = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
        .expect("leaf params")
        .signed_by(&leaf_key, &issuer)
        .expect("a leaf certificate");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    )
    .expect("a server configuration");
    Arc::new(config)
}

/// An absolute path as a `file://` URL: `file:///tmp/x`, or `file:///C:/x`.
fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn stamp_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
}

#[test]
fn a_pinned_link_is_fetched_once_stored_under_its_declared_version_and_used_inside_the_window() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let config = scratch.config(&[&origin.link(Some("8"))]);
    assert_eq!(
        scratch.cached(&[]),
        json!({"cache": scratch.cache(), "entries": []})
    );

    let first = scratch.check(&config, &json!({"hello": 1}), &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let seen = origin.seen();
    assert_eq!(seen.len(), 1, "one fetch");
    assert_eq!(seen[0].path, "/frames.json");
    assert!(!seen[0].conditional(), "nothing was cached to ask about");

    let listed = scratch.cached(&[]);
    let entries = listed["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "{listed}");
    assert_eq!(entries[0]["url"], json!(origin.url()));
    assert_eq!(
        entries[0]["version"],
        json!("8.1"),
        "stored under the declared version"
    );
    assert!(
        stamp_ok(entries[0]["confirmed_at"].as_str().expect("a stamp")),
        "{listed}"
    );
    let text = scratch.run(&["schemas", "--format", "text"], None, &[]);
    assert_eq!(
        text.stdout,
        format!(
            "cache {}\n{} 8.1 {}\n",
            scratch.cache(),
            origin.url(),
            entries[0]["confirmed_at"].as_str().expect("a stamp")
        )
    );

    // Inside the window: the entry, with no request, for a record it accepts
    // and one it refuses.
    let again = scratch.check(&config, &json!({"hello": 2}), &[]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    let violates = scratch.check(&config, &json!({"world": 2}), &[]);
    assert_eq!(violates.code, 1, "{}", violates.stderr);
    assert!(
        violates.stderr.contains("\"hello\" is a required property"),
        "{}",
        violates.stderr
    );
    assert_eq!(
        origin.seen().len(),
        1,
        "a resolution inside the window made a request"
    );
}

#[test]
fn past_the_window_a_conditional_request_is_sent_and_a_304_reconfirms_the_entry() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let config = scratch.config(&[&origin.link(Some("8"))]);
    assert_eq!(scratch.check(&config, &json!({"hello": 1}), &[]).code, 0);
    let before = scratch.cached(&[])["entries"][0]["confirmed_at"].clone();
    std::thread::sleep(Duration::from_millis(2100));

    let past = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("ONEMESSAGEBUS_SCHEMA_TTL", "1")],
    );
    assert_eq!(past.code, 0, "{}", past.stderr);
    assert!(
        !past.stderr.contains("could not revalidate"),
        "{}",
        past.stderr
    );
    let seen = origin.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1].headers.get("if-none-match").map(String::as_str),
        Some("\"8.1\"")
    );
    assert_eq!(
        seen[1].headers.get("if-modified-since").map(String::as_str),
        Some("Tue, 01 Sep 2026 09:12:44 GMT")
    );
    let after = scratch.cached(&[]);
    assert_eq!(scratch.versions(), vec!["8.1"], "a 304 stores nothing new");
    assert_ne!(
        after["entries"][0]["confirmed_at"], before,
        "the 304 re-confirmed the entry"
    );
}

#[test]
fn a_200_with_a_newer_satisfying_version_is_stored_beside_the_old_entry_and_used() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let config = scratch.config(&[&origin.link(Some("8"))]);
    assert_eq!(scratch.check(&config, &json!({"hello": 1}), &[]).code, 0);
    origin.serve("8.2");

    let ttl = [("ONEMESSAGEBUS_SCHEMA_TTL", "0")];
    let newer = scratch.check(&config, &json!({"hello": 1}), &ttl);
    assert_eq!(newer.code, 1, "8.2 requires world: {}", newer.stderr);
    assert!(
        newer.stderr.contains("\"world\" is a required property"),
        "{}",
        newer.stderr
    );
    assert!(origin.seen()[1].conditional());
    assert_eq!(
        scratch.versions(),
        vec!["8.1", "8.2"],
        "stored beside the old entry"
    );
    assert_eq!(
        scratch
            .check(&config, &json!({"hello": 1, "world": 1}), &[])
            .code,
        0
    );
}

#[test]
fn a_200_whose_version_the_pin_does_not_admit_is_a_hard_error_naming_link_pin_and_version() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    let config = scratch.config(&[&link]);
    assert_eq!(scratch.check(&config, &json!({"hello": 1}), &[]).code, 0);
    origin.serve("9");

    let refused = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("ONEMESSAGEBUS_SCHEMA_TTL", "0")],
    );
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    for named in [link.as_str(), "@8", "version 9"] {
        assert!(
            refused.stderr.contains(named),
            "{} does not name {named}",
            refused.stderr
        );
    }
    assert_eq!(refused.stdout, "");
    assert_eq!(
        scratch.versions(),
        vec!["8.1"],
        "nothing the pin refuses is stored"
    );

    // And on a first fetch too, with nothing cached.
    let fresh = Scratch::new();
    let config = fresh.config(&[&link]);
    let first = fresh.check(&config, &json!({"hello": 1}), &[]);
    assert_eq!(first.code, 2, "{}", first.stderr);
    assert!(
        first.stderr.contains("which the pin @8 does not admit"),
        "{}",
        first.stderr
    );
    assert!(fresh.versions().is_empty());
}

#[test]
fn a_revalidation_that_cannot_be_made_reuses_the_entry_and_says_so_on_stderr() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    let config = scratch.config(&[&link]);
    assert_eq!(scratch.check(&config, &json!({"hello": 1}), &[]).code, 0);
    origin.down();

    let reused = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("ONEMESSAGEBUS_SCHEMA_TTL", "0")],
    );
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert!(
        reused.stderr.contains(&format!(
            "onemessagebus: {link}: could not revalidate the cached bundle at version 8.1"
        )),
        "{}",
        reused.stderr
    );
    let violates = scratch.check(&config, &json!({}), &[("ONEMESSAGEBUS_SCHEMA_TTL", "0")]);
    assert_eq!(
        violates.code, 1,
        "the reused entry still validates: {}",
        violates.stderr
    );
}

#[test]
fn a_zero_window_revalidates_on_every_resolution() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let config = scratch.config(&[&origin.link(Some("8"))]);
    let ttl = [("ONEMESSAGEBUS_SCHEMA_TTL", "0")];
    for _ in 0..3 {
        let run = scratch.check(&config, &json!({"hello": 1}), &ttl);
        assert_eq!(run.code, 0, "{}", run.stderr);
    }
    let seen = origin.seen();
    assert_eq!(seen.len(), 3, "every resolution asked the origin");
    assert!(!seen[0].conditional());
    assert!(seen[1].conditional() && seen[2].conditional());

    let refused = scratch.check(&config, &json!({}), &[("ONEMESSAGEBUS_SCHEMA_TTL", "soon")]);
    assert_eq!(refused.code, 2);
    assert!(
        refused.stderr.contains("ONEMESSAGEBUS_SCHEMA_TTL"),
        "{}",
        refused.stderr
    );
}

#[test]
fn a_forced_refresh_refetches_unconditionally_and_replaces_what_the_cache_holds() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let config = scratch.config(&[&origin.link(Some("8"))]);
    assert_eq!(scratch.check(&config, &json!({"hello": 1}), &[]).code, 0);

    let refresh = [("ONEMESSAGEBUS_SCHEMA_REFRESH", "1")];
    let forced = scratch.check(&config, &json!({"hello": 1}), &refresh);
    assert_eq!(forced.code, 0, "{}", forced.stderr);
    let seen = origin.seen();
    assert_eq!(seen.len(), 2, "a fetch inside the window, forced");
    assert!(!seen[1].conditional(), "the forced fetch is unconditional");

    origin.serve("8.2");
    assert_eq!(
        scratch
            .check(&config, &json!({"hello": 1, "world": 1}), &refresh)
            .code,
        0
    );
    assert_eq!(
        scratch.versions(),
        vec!["8.2"],
        "the refetch replaced the older entry"
    );

    let refused = scratch.check(
        &config,
        &json!({}),
        &[("ONEMESSAGEBUS_SCHEMA_REFRESH", "yes")],
    );
    assert_eq!(refused.code, 2);
    assert!(
        refused.stderr.contains("ONEMESSAGEBUS_SCHEMA_REFRESH"),
        "{}",
        refused.stderr
    );
}

#[test]
fn an_unpinned_link_is_fetched_on_every_resolution_and_leaves_no_cache_entry() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let config = scratch.config(&[&origin.link(None)]);
    for _ in 0..2 {
        let run = scratch.check(&config, &json!({"hello": 1}), &[]);
        assert_eq!(run.code, 0, "{}", run.stderr);
    }
    let seen = origin.seen();
    assert_eq!(seen.len(), 2, "fetched each time, inside any window");
    assert!(seen.iter().all(|seen| !seen.conditional()));
    assert!(scratch.versions().is_empty());
    assert!(
        !scratch.path("cache").exists(),
        "an unpinned link wrote the cache"
    );
}

#[test]
fn the_cache_directory_is_its_variable_else_xdg_cache_home_else_home() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let config = scratch.config(&[&origin.link(Some("8"))]);

    let xdg = scratch.text("xdg");
    let under_xdg = [
        ("ONEMESSAGEBUS_SCHEMA_CACHE_DIR", ""),
        ("XDG_CACHE_HOME", xdg.as_str()),
    ];
    assert_eq!(
        scratch
            .check(&config, &json!({"hello": 1}), &under_xdg)
            .code,
        0
    );
    let expected = scratch.path("xdg").join("onemessagebus").join("schemas");
    assert_eq!(
        scratch.cached(&under_xdg)["cache"],
        json!(expected.to_str().expect("UTF-8"))
    );
    assert_eq!(
        scratch.cached(&under_xdg)["entries"][0]["version"],
        json!("8.1")
    );

    let home = scratch.text("home");
    let under_home = [
        ("ONEMESSAGEBUS_SCHEMA_CACHE_DIR", ""),
        ("HOME", home.as_str()),
    ];
    assert_eq!(
        scratch
            .check(&config, &json!({"hello": 1}), &under_home)
            .code,
        0
    );
    let expected = scratch
        .path("home")
        .join(".cache")
        .join("onemessagebus")
        .join("schemas");
    assert_eq!(
        scratch.cached(&under_home)["cache"],
        json!(expected.to_str().expect("UTF-8"))
    );
    assert_eq!(
        scratch.cached(&under_home)["entries"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(origin.seen().len(), 2, "each cache fetched for itself");

    let named = scratch.text("named");
    let under_variable = [
        ("ONEMESSAGEBUS_SCHEMA_CACHE_DIR", named.as_str()),
        ("XDG_CACHE_HOME", xdg.as_str()),
    ];
    assert_eq!(
        scratch
            .check(&config, &json!({"hello": 1}), &under_variable)
            .code,
        0
    );
    assert_eq!(scratch.cached(&under_variable)["cache"], json!(named));
    assert!(
        scratch.path("named").is_dir(),
        "the variable wins over XDG_CACHE_HOME"
    );
}

#[test]
fn file_and_bare_path_links_are_read_every_time_never_cached_and_held_to_their_pin() {
    let scratch = Scratch::new();
    std::fs::write(scratch.path("frames.json"), bundle("8.1")).expect("a bundle");
    let file_url = format!("{}@8", file_url(&scratch.path("frames.json")));
    for link in [file_url.as_str(), "frames.json@8"] {
        std::fs::write(scratch.path("frames.json"), bundle("8.1")).expect("a bundle");
        let config = scratch.config(&[link]);
        let first = scratch.check(&config, &json!({"hello": 1}), &[]);
        assert_eq!(first.code, 0, "{link}: {}", first.stderr);

        std::fs::write(scratch.path("frames.json"), bundle("8.2")).expect("a newer bundle");
        let reread = scratch.check(&config, &json!({"hello": 1}), &[]);
        assert_eq!(
            reread.code, 1,
            "{link}: read again, not cached: {}",
            reread.stderr
        );

        std::fs::write(scratch.path("frames.json"), bundle("9")).expect("an unpinned bundle");
        let refused = scratch.check(&config, &json!({"hello": 1}), &[]);
        assert_eq!(refused.code, 2, "{link}: {}", refused.stderr);
        assert!(
            refused
                .stderr
                .contains("version 9, which the pin @8 does not admit"),
            "{link}: {}",
            refused.stderr
        );
        assert!(
            !scratch.path("cache").exists(),
            "{link}: a file link wrote the cache"
        );
    }
}

#[test]
fn a_configuration_registers_every_linked_entry_for_the_verbs_that_load_it() {
    let scratch = Scratch::new();
    std::fs::write(scratch.path("frames.json"), bundle("8.1")).expect("a bundle");
    let link = format!("{}@8", file_url(&scratch.path("frames.json")));
    let config = scratch.config(&[&link]);

    let checked = scratch.check(&config, &json!({"hello": 1}), &[]);
    assert_eq!(checked.code, 0, "{}", checked.stderr);
    let violates = scratch.check(&config, &json!({"world": 1}), &[]);
    assert_eq!(violates.code, 1, "{}", violates.stderr);
    let listed = scratch.run(
        &["schema", "list", "--config", &config, "--format", "text"],
        None,
        &[],
    );
    assert!(
        listed.stdout.lines().any(|id| id == FRAME),
        "{}",
        listed.stdout
    );
    let unlinked = scratch.run(
        &["schema", "check", FRAME],
        Some("{}"),
        &[("ONEMESSAGEBUS_CONFIG", config.as_str())],
    );
    assert_eq!(
        unlinked.code, 2,
        "a schema verb reads no configuration from the variable"
    );

    let sent = scratch.run(
        &["send", "frames", "--config", &config],
        Some(r#"{"hello":1}"#),
        &[],
    );
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    let refused = scratch.run(
        &["send", "frames", "--config", &config],
        Some(r#"{"world":1}"#),
        &[],
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("\"hello\" is a required property"),
        "{}",
        refused.stderr
    );
    let validated = scratch.run(
        &["validate", "frames"],
        Some(r#"{"hello":1}"#),
        &[("ONEMESSAGEBUS_CONFIG", config.as_str())],
    );
    assert_eq!(validated.code, 0, "{}", validated.stderr);

    let generated = scratch.run(
        &[
            "schema", "gen", "--lang", "json", FRAME, "--config", &config,
        ],
        None,
        &[],
    );
    assert_eq!(generated.code, 0, "{}", generated.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&generated.stdout).expect("JSON"),
        json!({"type": "object", "required": ["hello"]}),
        "schema gen renders the linked document"
    );
    let unlinked_gen = scratch.run(&["schema", "gen", "--lang", "json", FRAME], None, &[]);
    assert_eq!(unlinked_gen.code, 2, "{}", unlinked_gen.stderr);

    // Registering a different document under a linked id is refused, and
    // nothing is written.
    let linked_registry = scratch.text("linked-registry");
    std::fs::write(
        scratch.path("array.json"),
        json!({"type": "array"}).to_string(),
    )
    .expect("a schema");
    let contradicted = scratch.run(
        &[
            "schema",
            "register",
            FRAME,
            "--file",
            &scratch.text("array.json"),
            "--registry",
            &linked_registry,
            "--config",
            &config,
        ],
        None,
        &[],
    );
    assert_eq!(contradicted.code, 2, "{}", contradicted.stderr);
    assert!(
        contradicted.stderr.contains(&format!(
            "{FRAME} is already registered with a different document"
        )),
        "{}",
        contradicted.stderr
    );
    assert!(
        !scratch.path("linked-registry").exists(),
        "a refused register wrote the registry"
    );

    // A linked entry contradicting an id the profile registers.
    let profile_document = scratch.run(
        &["schema", "gen", "--lang", "json", "agent.labels@1"],
        None,
        &[],
    );
    assert_eq!(profile_document.code, 0, "{}", profile_document.stderr);
    std::fs::write(
        scratch.path("contradicts.json"),
        json!({"version": "1", "schemas": [{"id": "agent.labels@1", "schema": {"type": "string"}}]}).to_string(),
    )
    .expect("a bundle");
    let config = scratch.config(&[&link, "contradicts.json@1"]);
    for run in [
        scratch.check(&config, &json!({"hello": 1}), &[]),
        scratch.run(
            &["send", "frames", "--config", &config],
            Some(r#"{"hello":1}"#),
            &[],
        ),
    ] {
        assert_eq!(run.code, 2, "{}", run.stderr);
        assert!(
            run.stderr
                .contains("agent.labels@1 is already registered with a different document"),
            "{}",
            run.stderr
        );
        assert!(run.stderr.contains("contradicts.json@1"), "{}", run.stderr);
    }

    // And one contradicting a `--registry` directory's document.
    let registry = scratch.text("registry");
    std::fs::write(
        scratch.path("other.json"),
        json!({"type": "array"}).to_string(),
    )
    .expect("a schema");
    let registered = scratch.run(
        &[
            "schema",
            "register",
            FRAME,
            "--file",
            &scratch.text("other.json"),
            "--registry",
            &registry,
        ],
        None,
        &[],
    );
    assert_eq!(registered.code, 0, "{}", registered.stderr);
    let config = scratch.config(&[&link]);
    let conflict = scratch.run(
        &[
            "schema",
            "check",
            FRAME,
            "--config",
            &config,
            "--registry",
            &registry,
        ],
        Some("[]"),
        &[],
    );
    assert_eq!(conflict.code, 2, "{}", conflict.stderr);
    assert!(
        conflict.stderr.contains(&format!(
            "{FRAME} is already registered with a different document"
        )),
        "{}",
        conflict.stderr
    );
}

#[test]
fn http_proxy_carries_an_http_link_and_https_proxy_alone_does_not() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let proxy = Proxy::start(false);
    let link = origin.link(None);
    let config = scratch.config(&[&link]);
    let proxy_url = proxy.url();

    let direct = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("HTTPS_PROXY", proxy_url.as_str())],
    );
    assert_eq!(direct.code, 0, "{}", direct.stderr);
    assert!(
        proxy.log().is_empty(),
        "an http:// link went through HTTPS_PROXY: {:?}",
        proxy.log()
    );

    let proxied = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("HTTP_PROXY", proxy_url.as_str())],
    );
    assert_eq!(proxied.code, 0, "{}", proxied.stderr);
    assert_eq!(proxy.log(), vec![format!("CONNECT {}", origin.addr)]);
    assert_eq!(origin.seen().len(), 2, "both fetches reached the bundle");

    let bypassed = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[
            ("http_proxy", proxy_url.as_str()),
            ("no_proxy", "127.0.0.1"),
        ],
    );
    assert_eq!(bypassed.code, 0, "{}", bypassed.stderr);
    assert_eq!(
        proxy.log().len(),
        1,
        "the lower-case spellings: {:?}",
        proxy.log()
    );
}

#[test]
fn an_https_link_is_fetched_from_a_loopback_tls_origin_the_client_trusts_through_its_root_store() {
    let scratch = Scratch::new();
    let tls = tls_origin_config(&scratch.path("ca.pem"));
    let origin = Origin::https("8.1", tls);
    let link = origin.link(Some("8"));
    let config = scratch.config(&[&link]);
    let ca = scratch.text("ca.pem");

    let trusted = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("SSL_CERT_FILE", ca.as_str())],
    );
    assert_eq!(trusted.code, 0, "{}", trusted.stderr);
    let cert_dir = scratch.path("certs");
    std::fs::create_dir_all(&cert_dir).expect("a certificate directory");
    std::fs::copy(scratch.path("ca.pem"), cert_dir.join("ca.pem")).expect("the CA copied");
    let by_dir = Scratch::new();
    let dir_config = by_dir.config(&[&link]);
    let trusted_by_dir = by_dir.check(
        &dir_config,
        &json!({"hello": 1}),
        &[("SSL_CERT_DIR", cert_dir.to_str().expect("UTF-8"))],
    );
    assert_eq!(
        trusted_by_dir.code, 0,
        "SSL_CERT_DIR: {}",
        trusted_by_dir.stderr
    );
    assert_eq!(
        origin.seen().len(),
        2,
        "fetched over TLS through SSL_CERT_DIR"
    );
    assert_eq!(
        origin.seen().len(),
        2,
        "fetched over TLS, once per root store"
    );
    assert_eq!(scratch.versions(), vec!["8.1"]);

    // A root store that does not hold the journey's CA refuses the origin.
    let other = tls_origin_config(&scratch.path("other-ca.pem"));
    drop(other);
    let other_ca = scratch.text("other-ca.pem");
    let untrusted = Scratch::new();
    let config = untrusted.config(&[&link]);
    let refused = untrusted.check(
        &config,
        &json!({"hello": 1}),
        &[("SSL_CERT_FILE", other_ca.as_str())],
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains(&format!("{link}: cannot fetch the bundle")),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stderr.to_lowercase().contains("certificate"),
        "{}",
        refused.stderr
    );
    assert_eq!(
        origin.seen().len(),
        2,
        "no request crossed an untrusted handshake"
    );
}

#[test]
fn https_proxy_carries_the_fetch_and_no_proxy_naming_the_host_bypasses_it() {
    let scratch = Scratch::new();
    let tls = tls_origin_config(&scratch.path("ca.pem"));
    let origin = Origin::https("8.1", tls);
    let proxy = Proxy::start(false);
    let link = origin.link(None);
    let config = scratch.config(&[&link]);
    let ca = scratch.text("ca.pem");
    let proxy_url = proxy.url();

    let proxied = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[
            ("SSL_CERT_FILE", ca.as_str()),
            ("HTTPS_PROXY", proxy_url.as_str()),
        ],
    );
    assert_eq!(proxied.code, 0, "{}", proxied.stderr);
    assert_eq!(proxy.log(), vec![format!("CONNECT {}", origin.addr)]);
    assert_eq!(
        origin.seen().len(),
        1,
        "the bundle was reached through the proxy"
    );

    let bypassed = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[
            ("SSL_CERT_FILE", ca.as_str()),
            ("HTTPS_PROXY", proxy_url.as_str()),
            ("NO_PROXY", "example.org, 127.0.0.1"),
        ],
    );
    assert_eq!(bypassed.code, 0, "{}", bypassed.stderr);
    assert_eq!(
        proxy.log().len(),
        1,
        "the bypassed fetch went through the proxy: {:?}",
        proxy.log()
    );
    assert_eq!(origin.seen().len(), 2, "the bundle was reached directly");
}

/// A supervisor frame whose turn is taken: `serve` answers it at once, raising
/// nothing.
fn frame() -> String {
    json!({
        "op": "supervisor",
        "task": "onepipeline run `r-7`.\nwatch",
        "persona": "A careful monitor.",
        "done_when": "the watch is kept",
        "worktree": "/repo",
        "history_name": "r-7-monitor",
        "messages": [{"role": "user", "content": "watch"}, {"role": "assistant", "content": "nothing drifted"}],
        "session": "r-7-user"
    })
    .to_string()
}

/// `serve surfaces --codec onejudge` over a planner channel whose configuration
/// links `link`.
fn serve(scratch: &Scratch, link: &str, env: &[(&str, &str)]) -> Run {
    std::fs::write(
        scratch.path("serve.yaml"),
        format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: planner-channel\nschemas:\n  - {}\n",
            serde_json::to_string(&scratch.path("channel")).expect("a path"),
            serde_json::to_string(link).expect("a string")
        ),
    )
    .expect("written");
    let mut env = env.to_vec();
    env.push((onejudge::CODEX_ALT_HOME_ENV, "/nowhere/codex-alt"));
    scratch.run(
        &[
            "serve",
            "surfaces",
            "--codec",
            "onejudge",
            "--config",
            &scratch.text("serve.yaml"),
        ],
        Some(&format!("{}\n", frame())),
        &env,
    )
}

#[test]
fn serve_uses_a_warmed_cache_whatever_its_age_and_refuses_before_a_frame_with_nothing_cached() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    let warmed = scratch.run(&["schemas", "fetch", &link], None, &[]);
    assert_eq!(warmed.code, 0, "{}", warmed.stderr);
    assert_eq!(origin.seen().len(), 1);

    // Every entry is past a zero window, and a refresh is asked for: serve still
    // makes no request.
    let stale = [
        ("ONEMESSAGEBUS_SCHEMA_TTL", "0"),
        ("ONEMESSAGEBUS_SCHEMA_REFRESH", "1"),
    ];
    let answered = serve(&scratch, &link, &stale);
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(answered.lines().len(), 1, "{}", answered.stdout);
    assert_eq!(
        origin.seen().len(),
        1,
        "serve asked the origin about a cached entry"
    );

    origin.down();
    let answered = serve(&scratch, &link, &stale);
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(
        answered.lines()[0]["completion"],
        json!(false),
        "{}",
        answered.stdout
    );
    assert!(
        !answered.stderr.contains("revalidate"),
        "no revalidation was attempted: {}",
        answered.stderr
    );

    let cold = Scratch::new();
    let refused = serve(&cold, &link, &[]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert_eq!(refused.stdout, "", "a frame was answered");
    assert!(
        refused
            .stderr
            .contains(&format!("{link}: cannot fetch the bundle")),
        "{}",
        refused.stderr
    );
}

#[test]
fn schemas_lists_clears_and_fetch_warms_revalidating_whatever_the_window() {
    let scratch = Scratch::new();
    let first = Origin::http("8.1");
    let second = Origin::http("3");
    let (one, two) = (first.link(Some("8")), second.link(Some("3")));

    let missing = scratch.run(&["schemas", "fetch"], None, &[]);
    assert_eq!(missing.code, 2, "{}", missing.stderr);
    assert!(
        missing
            .stderr
            .contains("name the links to fetch, or --config"),
        "{}",
        missing.stderr
    );

    let config = scratch.config(&[&one, &two]);
    let fetched = scratch.run(&["schemas", "fetch", "--config", &config], None, &[]);
    assert_eq!(fetched.code, 0, "{}", fetched.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&fetched.stdout).expect("JSON"),
        json!({"links": [
            {"link": one, "outcome": "fetched", "version": "8.1"},
            {"link": two, "outcome": "fetched", "version": "3"}
        ]})
    );

    // Inside the window, fetch still asks; the origin confirms.
    let confirmed = scratch.run(&["schemas", "fetch", &one, "--format", "text"], None, &[]);
    assert_eq!(confirmed.code, 0, "{}", confirmed.stderr);
    assert_eq!(confirmed.stdout, format!("{one} confirmed 8.1\n"));
    assert!(first.seen()[1].conditional());

    // Revalidation that cannot be made: the entry is reused, exit 0.
    first.down();
    let reused = scratch.run(&["schemas", "fetch", &one], None, &[]);
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    let report: Value = serde_json::from_str(&reused.stdout).expect("JSON");
    assert_eq!(report["links"][0]["outcome"], json!("reused"), "{report}");
    assert_eq!(report["links"][0]["version"], json!("8.1"));
    assert!(report["links"][0]["reason"].is_string(), "{report}");
    assert!(
        reused.stderr.contains("could not revalidate"),
        "{}",
        reused.stderr
    );

    // A link it cannot resolve: the report, then exit 1 naming it.
    let absent = "http://127.0.0.1:9/absent.json@1";
    let failed = scratch.run(&["schemas", "fetch", &two, absent], None, &[]);
    assert_eq!(failed.code, 1, "{}", failed.stderr);
    let report: Value = serde_json::from_str(&failed.stdout).expect("JSON");
    assert_eq!(report["links"][0]["outcome"], json!("confirmed"));
    assert_eq!(report["links"][1]["outcome"], json!("failed"));
    assert!(report["links"][1].get("version").is_none(), "{report}");
    assert!(
        failed.stderr.contains(&format!(
            "schemas fetch: 1 of 2 links did not resolve: {absent}: cannot fetch the bundle"
        )),
        "{}",
        failed.stderr
    );
    let as_text = scratch.run(&["schemas", "fetch", absent, "--format", "text"], None, &[]);
    assert_eq!(as_text.code, 1, "{}", as_text.stderr);
    assert!(
        as_text.stdout.starts_with(&format!(
            "{absent} failed - ({absent}: cannot fetch the bundle: "
        )),
        "{}",
        as_text.stdout
    );
    assert_eq!(as_text.stdout.lines().count(), 1);

    // A link whose bundle its pin does not admit refuses the input.
    let unadmitted = format!("{}@9", second.url());
    let pin_refused = scratch.run(&["schemas", "fetch", &two, &unadmitted], None, &[]);
    assert_eq!(pin_refused.code, 2, "{}", pin_refused.stderr);
    let report: Value = serde_json::from_str(&pin_refused.stdout).expect("JSON");
    assert_eq!(report["links"][1]["outcome"], json!("failed"), "{report}");
    assert!(
        pin_refused
            .stderr
            .contains("the bundle declares version 3, which the pin @9 does not admit"),
        "{}",
        pin_refused.stderr
    );

    let malformed = scratch.run(
        &["schemas", "fetch", "http://example.org/frames.json@1"],
        None,
        &[],
    );
    assert_eq!(malformed.code, 2, "{}", malformed.stderr);

    let mut versions = scratch.versions();
    versions.sort();
    assert_eq!(versions, vec!["3", "8.1"]);
    let cleared = scratch.run(&["schemas", "clear"], None, &[]);
    assert_eq!(cleared.code, 0, "{}", cleared.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&cleared.stdout).expect("JSON"),
        json!({"cache": scratch.cache(), "removed": 2})
    );
    assert_eq!(
        scratch.cached(&[]),
        json!({"cache": scratch.cache(), "entries": []})
    );
    let again = scratch.run(&["schemas", "clear", "--format", "text"], None, &[]);
    assert_eq!(again.code, 0);
    assert_eq!(
        again.stdout,
        format!("removed 0 from {}\n", scratch.cache())
    );

    let nowhere = scratch.run(
        &["schemas"],
        None,
        &[("ONEMESSAGEBUS_SCHEMA_CACHE_DIR", ""), ("HOME", "")],
    );
    assert_eq!(nowhere.code, 2, "{}", nowhere.stderr);
    assert!(
        nowhere.stderr.contains("no schema cache directory"),
        "{}",
        nowhere.stderr
    );
}

#[test]
fn a_revalidation_answered_with_a_document_that_is_no_bundle_reuses_the_entry() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    let config = scratch.config(&[&link]);
    assert_eq!(scratch.check(&config, &json!({"hello": 1}), &[]).code, 0);
    origin.serve_text("<html>a captive portal</html>".to_owned());

    let reused = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("ONEMESSAGEBUS_SCHEMA_TTL", "0")],
    );
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert!(origin.seen()[1].conditional());
    assert!(
        reused.stderr.contains(&format!(
            "onemessagebus: {link}: could not revalidate the cached bundle at version 8.1 (the origin answered a document that is not a schema bundle: the document is not JSON"
        )),
        "{}",
        reused.stderr
    );
    assert_eq!(
        scratch.versions(),
        vec!["8.1"],
        "nothing unreadable is stored"
    );

    // With nothing cached, the same document refuses naming the link.
    let cold = Scratch::new();
    let config = cold.config(&[&link]);
    let refused = cold.check(&config, &json!({"hello": 1}), &[]);
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(&format!(
            "{link}: not a schema bundle: the document is not JSON"
        )),
        "{}",
        refused.stderr
    );
}

#[test]
fn a_response_past_the_bundle_bound_is_refused_naming_the_bound() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    origin.serve_text(format!(
        "{{\"padding\": \"{}\"}}",
        "x".repeat(17 * 1024 * 1024)
    ));
    let link = origin.link(Some("8"));
    let run = scratch.run(&["schemas", "fetch", &link], None, &[]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr.contains(&format!(
            "{link}: cannot fetch the bundle: reading the response: the document is larger than the 16 MiB a bundle may be"
        )),
        "{}",
        run.stderr
    );
    assert!(scratch.versions().is_empty());
}

// llmlint: ignore-block[tests_mirror_real_usage] every journey to the ignore-end below write into the cache directory: the cache directory is a user-facing input — it is the directory ONEMESSAGEBUS_SCHEMA_CACHE_DIR, XDG_CACHE_HOME or HOME names, shared, editable and possibly written by another release — and what this journey holds is the binary's boundary validation of that input, which no verb can produce a corrupt or unreadable entry to exercise; the binary is still driven only through its command line.
#[cfg(unix)]
#[test]
fn a_cache_directory_that_cannot_be_read_is_refused_by_the_verbs_that_report_on_it() {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    assert_eq!(scratch.run(&["schemas", "fetch", &link], None, &[]).code, 0);
    let entry_dir = std::fs::read_dir(scratch.path("cache"))
        .expect("the cache")
        .flatten()
        .map(|entry| entry.path())
        .next()
        .expect("one entry directory");
    std::fs::set_permissions(&entry_dir, std::fs::Permissions::from_mode(0o000)).expect("locked");
    if std::fs::read_dir(&entry_dir).is_ok() {
        // A user the permission bits do not bind (root) reads it anyway.
        std::fs::set_permissions(&entry_dir, std::fs::Permissions::from_mode(0o755))
            .expect("unlocked");
        return;
    }
    let listed = scratch.run(&["schemas"], None, &[]);
    let cleared = scratch.run(&["schemas", "clear"], None, &[]);
    // Resolution fetches past the entry it cannot read, and then refuses the
    // entry it cannot write, naming it.
    let config = scratch.config(&[&link]);
    let resolved = scratch.check(&config, &json!({"hello": 1}), &[]);
    std::fs::set_permissions(&entry_dir, std::fs::Permissions::from_mode(0o755)).expect("unlocked");

    for run in [&listed, &cleared] {
        assert_eq!(run.code, 1, "{}", run.stderr);
        assert_eq!(run.stdout, "");
        assert!(
            run.stderr.contains(&format!(
                "the schema cache at {}: cannot read it: ",
                entry_dir.display()
            )),
            "{}",
            run.stderr
        );
    }
    assert_eq!(
        origin.seen().len(),
        2,
        "resolution did not fetch past the unreadable entry"
    );
    assert_eq!(resolved.code, 1, "{}", resolved.stderr);
    assert!(
        resolved
            .stderr
            .contains(&format!("the schema cache at {}", entry_dir.display()))
            && resolved.stderr.contains("cannot write it"),
        "{}",
        resolved.stderr
    );
}

#[test]
fn a_cache_entry_past_its_bound_is_passed_over_and_refetched() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    let config = scratch.config(&[&link]);
    assert_eq!(scratch.run(&["schemas", "fetch", &link], None, &[]).code, 0);
    let entry_dir = std::fs::read_dir(scratch.path("cache"))
        .expect("the cache")
        .flatten()
        .map(|entry| entry.path())
        .next()
        .expect("one entry directory");

    // A body still declaring 8.1, padded past the bundle bound with whitespace
    // JSON allows: unbounded, it would read as the entry.
    let body = entry_dir.join("8.1.json");
    let padded = format!("{}{}", bundle("8.1"), " ".repeat(17 * 1024 * 1024));
    std::fs::write(&body, padded).expect("padded");
    assert!(
        scratch.versions().is_empty(),
        "an oversized cached body was listed"
    );
    let refetched = scratch.check(&config, &json!({"hello": 1}), &[]);
    assert_eq!(refetched.code, 0, "{}", refetched.stderr);
    assert_eq!(
        origin.seen().len(),
        2,
        "the oversized entry was used rather than refetched"
    );
    assert_eq!(scratch.versions(), vec!["8.1"]);

    // Metadata padded past its own bound is passed over the same way.
    let meta = entry_dir.join("8.1.meta.json");
    let text = std::fs::read_to_string(&meta).expect("metadata");
    std::fs::write(&meta, format!("{text}{}", " ".repeat(65 * 1024))).expect("padded");
    assert!(
        scratch.versions().is_empty(),
        "oversized metadata was listed"
    );

    // Metadata whose validator no request could carry is passed over too, and
    // the link refetched rather than a malformed header sent.
    let unusable = text.replace(r#""\"8.1\"""#, r#""bad\nvalue""#);
    assert_ne!(unusable, text, "the metadata records the tag: {text}");
    std::fs::write(&meta, unusable).expect("rewritten");
    assert!(
        scratch.versions().is_empty(),
        "metadata with an unusable validator was listed"
    );
    let refetched = scratch.check(
        &config,
        &json!({"hello": 1}),
        &[("ONEMESSAGEBUS_SCHEMA_TTL", "0")],
    );
    assert_eq!(refetched.code, 0, "{}", refetched.stderr);
    assert!(
        !origin.seen().last().expect("a request").conditional(),
        "the unusable entry was revalidated"
    );
    assert_eq!(scratch.versions(), vec!["8.1"]);
}

#[cfg(unix)]
#[test]
fn a_cache_write_replaces_a_symbolic_link_at_an_entry_rather_than_following_it() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    assert_eq!(scratch.run(&["schemas", "fetch", &link], None, &[]).code, 0);
    let entry_dir = std::fs::read_dir(scratch.path("cache"))
        .expect("the cache")
        .flatten()
        .map(|entry| entry.path())
        .next()
        .expect("one entry directory");

    // Each file of the entry becomes a link to a file outside the cache.
    let mut outside = Vec::new();
    for name in ["8.1.json", "8.1.meta.json"] {
        let target = scratch.path(&format!("outside-{name}"));
        std::fs::write(&target, "untouched").expect("a file outside the cache");
        let entry = entry_dir.join(name);
        std::fs::remove_file(&entry).expect("the entry file");
        std::os::unix::fs::symlink(&target, &entry).expect("a symbolic link");
        outside.push((entry, target));
    }

    let refreshed = scratch.run(
        &["schemas", "fetch", &link],
        None,
        &[("ONEMESSAGEBUS_SCHEMA_REFRESH", "1")],
    );
    assert_eq!(refreshed.code, 0, "{}", refreshed.stderr);
    for (entry, target) in &outside {
        assert_eq!(
            std::fs::read_to_string(target).expect("the outside file"),
            "untouched",
            "the write to {} followed its link",
            entry.display()
        );
        assert!(
            std::fs::symlink_metadata(entry)
                .expect("the entry")
                .file_type()
                .is_file(),
            "{} is still a link",
            entry.display()
        );
    }
    assert_eq!(scratch.versions(), vec!["8.1"]);
    let names: Vec<String> = std::fs::read_dir(&entry_dir)
        .expect("the entry directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "a staged write was left behind: {names:?}");

    // An entry directory that is itself a link to one outside the cache is
    // neither listed, cleared, read nor written through.
    let moved = scratch.path("outside-entries");
    std::fs::rename(&entry_dir, &moved).expect("the entry directory moved out");
    std::os::unix::fs::symlink(&moved, &entry_dir).expect("a symbolic link");
    assert!(
        scratch.versions().is_empty(),
        "an entry outside the cache was listed"
    );
    let cleared = scratch.run(&["schemas", "clear"], None, &[]);
    assert_eq!(cleared.code, 0, "{}", cleared.stderr);
    let report: Value = serde_json::from_str(&cleared.stdout).expect("a JSON report");
    assert_eq!(report["removed"], 0, "{}", cleared.stdout);
    let refused = scratch.run(&["schemas", "fetch", &link], None, &[]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(&format!(
            "{}: cannot write it: it is not a directory of the cache",
            entry_dir.display()
        )),
        "{}",
        refused.stderr
    );
    let outside_names = std::fs::read_dir(&moved)
        .expect("the moved entries")
        .flatten()
        .count();
    assert_eq!(
        outside_names, 2,
        "the entries outside the cache were touched"
    );
}
// llmlint: ignore-end[tests_mirror_real_usage]

#[test]
fn a_redirect_is_followed_only_to_a_location_a_link_may_name() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let moved = format!("http://{}/moved.json@8", origin.addr);
    let followed = scratch.run(&["schemas", "fetch", &moved], None, &[]);
    assert_eq!(followed.code, 0, "{}", followed.stderr);
    let paths: Vec<String> = origin.seen().into_iter().map(|seen| seen.path).collect();
    assert_eq!(paths, vec!["/moved.json", "/frames.json"]);

    let away = format!("http://{}/away.json@8", origin.addr);
    let refused = scratch.run(&["schemas", "fetch", &away], None, &[]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(&format!(
            "{away}: cannot fetch the bundle: the origin redirected to \"http://example.org/frames.json\", which a link may not follow: http:// is taken only for a loopback host"
        )),
        "{}",
        refused.stderr
    );
    assert_eq!(origin.seen().len(), 3, "the refused redirect was followed");
}

#[test]
fn an_origin_answer_no_bundle_can_come_from_is_refused_naming_it() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let refusals = [
        (
            "/nowhere.json",
            "the origin answered HTTP 302 with no Location",
        ),
        ("/loop.json", "the origin redirected more than 5 times"),
        ("/gone.json", "the origin answered HTTP 404"),
        // A request that carried no validator cannot be answered "not modified".
        ("/unchanged.json", "the origin answered HTTP 304"),
    ];
    for (path, why) in refusals {
        let link = format!("http://{}{path}@8", origin.addr);
        let refused = scratch.run(&["schemas", "fetch", &link], None, &[]);
        assert_eq!(refused.code, 1, "{path}: {}", refused.stderr);
        assert!(
            refused
                .stderr
                .contains(&format!("{link}: cannot fetch the bundle: {why}")),
            "{path}: {}",
            refused.stderr
        );
    }
    let loops = origin
        .seen()
        .iter()
        .filter(|seen| seen.path == "/loop.json")
        .count();
    assert_eq!(
        loops, 6,
        "the first request and five redirects are followed"
    );
    assert!(
        scratch.versions().is_empty(),
        "a refusal left a cache entry"
    );
}

#[test]
fn a_proxy_the_environment_names_that_is_not_usable_is_refused_naming_it() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    let refused = scratch.run(
        &["schemas", "fetch", &link],
        None,
        &[("HTTP_PROXY", "not a proxy at all")],
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(&format!(
            "{link}: cannot fetch the bundle: the proxy \"not a proxy at all\" is not usable: "
        )),
        "{}",
        refused.stderr
    );
    assert!(
        origin.seen().is_empty(),
        "the origin was reached around the proxy"
    );
}

#[cfg(unix)]
#[test]
fn the_resident_core_starts_from_a_warmed_cache_and_refuses_to_start_with_nothing_cached() {
    let scratch = Scratch::new();
    let origin = Origin::http("8.1");
    let link = origin.link(Some("8"));
    let config = scratch.config(&[&link]);
    assert_eq!(scratch.run(&["schemas", "fetch", &link], None, &[]).code, 0);
    origin.down();

    let socket = scratch.path("bus.sock");
    let socket_text = socket.to_str().expect("UTF-8").to_owned();
    let args = [
        "serve",
        "--resident",
        "--socket",
        socket_text.as_str(),
        "--config",
        config.as_str(),
    ];
    let stale = [("ONEMESSAGEBUS_SCHEMA_TTL", "0")];
    let mut warm = scratch.spawn(&args, None, &stale);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !socket.exists() {
        assert!(
            matches!(warm.try_wait(), Ok(None)),
            "the resident exited before it listened"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "the resident never listened"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::remove_file(&socket).expect("the socket is removed");
    let output = warm.wait_with_output().expect("the resident stops");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("revalidate"));

    let cold = Scratch::new();
    let cold_config = cold.config(&[&link]);
    let cold_socket = cold.path("bus.sock");
    let refused = cold.run(
        &[
            "serve",
            "--resident",
            "--socket",
            cold_socket.to_str().expect("UTF-8"),
            "--config",
            &cold_config,
        ],
        None,
        &[],
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains(&format!("{link}: cannot fetch the bundle")),
        "{}",
        refused.stderr
    );
    assert!(
        !cold_socket.exists(),
        "the resident listened with nothing cached"
    );
}
