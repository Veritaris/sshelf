//! Who holds a port, for the message of a test that found it taken (CI has
//! failed on "already in use" for a port `hold_port` had just found free).
//! Included with `#[path]` by the test binaries that report supervisor
//! errors.

/// `message` and, on Linux, the sockets on each port it names as "port N …
/// already in use" with their state and process (from `/proc`).
pub fn explain(message: &str) -> String {
    let ports: Vec<u16> = message
        .split("port ")
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect();
    if !message.contains("already in use") || ports.is_empty() {
        return message.to_string();
    }
    let mut out = message.to_string();
    for port in ports {
        out.push_str(&format!("\nport {port}: {}", holders(port)));
    }
    out
}

#[cfg(target_os = "linux")]
fn holders(port: u16) -> String {
    use std::fs;

    // /proc/net/tcp{,6}: "sl local rem st … uid timeout inode …", the
    // addresses as hex "ADDR:PORT".
    let mut sockets = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(table) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let Some(local) = fields.get(1) else { continue };
            let local_port = local
                .rsplit_once(':')
                .and_then(|(_, p)| u16::from_str_radix(p, 16).ok());
            if local_port == Some(port) {
                let state = match fields.get(3).copied() {
                    Some("01") => "ESTABLISHED",
                    Some("06") => "TIME_WAIT",
                    Some("0A") => "LISTEN",
                    Some(other) => other,
                    None => "?",
                };
                let inode = fields.get(9).copied().unwrap_or("0").to_string();
                sockets.push((state.to_string(), inode, (*local).to_string()));
            }
        }
    }
    if sockets.is_empty() {
        return "no socket on it now".into();
    }
    let mut owners: Vec<(String, String)> = Vec::new();
    if let Ok(procs) = fs::read_dir("/proc") {
        for entry in procs.flatten() {
            let pid = entry.file_name().to_string_lossy().into_owned();
            if !pid.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let Ok(link) = fs::read_link(fd.path()) else {
                    continue;
                };
                let link = link.to_string_lossy().into_owned();
                if let Some((_, inode, _)) = sockets
                    .iter()
                    .find(|(_, inode, _)| link == format!("socket:[{inode}]"))
                {
                    let cmdline = fs::read(entry.path().join("cmdline"))
                        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
                        .unwrap_or_default();
                    owners.push((inode.clone(), format!("pid {pid} ({})", cmdline.trim())));
                }
            }
        }
    }
    sockets
        .iter()
        .map(|(state, inode, local)| {
            let by: Vec<&str> = owners
                .iter()
                .filter(|(i, _)| i == inode)
                .map(|(_, o)| o.as_str())
                .collect();
            let by = if by.is_empty() {
                "no process found".to_string()
            } else {
                by.join(", ")
            };
            format!("{state} {local} held by {by}")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(not(target_os = "linux"))]
fn holders(_port: u16) -> String {
    "(holder lookup is Linux only)".into()
}
