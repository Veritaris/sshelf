//! A local HTTP server standing in for GitHub's release API (SPEC.md §10
//! *Update check*): no test ever asks GitHub. Included with `#[path]` by the
//! test binaries that start an update check.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

/// The server's address and the requests' heads it got.
pub struct Server {
    pub url: String,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

/// Answers every request with `body` as a 200 (a thread per server, left
/// waiting when the test ends).
pub fn serve(body: String) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
    let heads: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&heads);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut head = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            seen.lock().unwrap().push(head);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nETag: \"t1\"\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    Server { url, heads }
}

/// A made-up latest release `version`, as GitHub's JSON.
pub fn release(version: &str) -> String {
    format!(
        "{{\"tag_name\": \"{version}\", \
         \"html_url\": \"https://github.com/Veritaris/sshelf/releases/tag/{version}\", \
         \"published_at\": \"2026-11-02T10:00:00Z\", \"draft\": false, \"prerelease\": false}}"
    )
}
