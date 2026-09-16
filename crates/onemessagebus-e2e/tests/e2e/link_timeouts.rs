//! The transport's bounds through the built binary: a fetch that crosses the
//! connect timeout or the read timeout is refused naming it, within the bound
//! the documentation states, each measured here rather than assumed.
//!
//! Apart from `links` because each journey waits out a bound: the proxy and the
//! origin that never answer are loopback listeners started here.

use std::io::Read as _;
use std::net::{SocketAddr, TcpListener};
use std::time::{Duration, Instant};

use onemessagebus::{CONNECT_TIMEOUT, READ_TIMEOUT};

use crate::links::{Proxy, Scratch};

/// A loopback listener that accepts each connection, reads what arrives, and
/// never answers.
fn silent_origin() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("an address");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buffer = [0_u8; 1024];
                while matches!(stream.read(&mut buffer), Ok(n) if n > 0) {}
            });
        }
    });
    addr
}

#[test]
fn an_origin_that_never_answers_is_refused_at_the_read_timeout() {
    let scratch = Scratch::new();
    let link = format!("http://{}/frames.json@8", silent_origin());
    let started = Instant::now();
    let run = scratch.run(&["schemas", "fetch", &link], None, &[]);
    let took = started.elapsed();
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr.contains(&format!(
            "{link}: cannot fetch the bundle: nothing was received within the {}-second read timeout",
            READ_TIMEOUT.as_secs()
        )),
        "{}",
        run.stderr
    );
    assert!(
        took >= READ_TIMEOUT,
        "refused after {took:?}, before the read bound"
    );
    assert!(
        took < READ_TIMEOUT + Duration::from_secs(10),
        "refused after {took:?}, far past the read bound"
    );
}

#[test]
fn a_connection_that_never_completes_is_refused_at_the_connect_timeout() {
    let scratch = Scratch::new();
    let proxy = Proxy::start(true);
    let link = "https://127.0.0.1:9/frames.json@8";
    let proxy_url = proxy.url();
    let started = Instant::now();
    let run = scratch.run(
        &["schemas", "fetch", link],
        None,
        &[("HTTPS_PROXY", proxy_url.as_str())],
    );
    let took = started.elapsed();
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr.contains(&format!(
            "{link}: cannot fetch the bundle: the connection was not established within the {}-second connect timeout",
            CONNECT_TIMEOUT.as_secs()
        )),
        "{}",
        run.stderr
    );
    assert_eq!(
        proxy.log(),
        vec!["CONNECT 127.0.0.1:9".to_owned()],
        "the proxy took the connection"
    );
    assert!(
        took >= CONNECT_TIMEOUT,
        "refused after {took:?}, before the connect bound"
    );
    assert!(
        took < CONNECT_TIMEOUT + Duration::from_secs(8),
        "refused after {took:?}, far past the connect bound"
    );
}
