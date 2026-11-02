//! Small display helpers shared by the views.

use std::time::{Duration, SystemTime};

use sshconf::{ConfigSet, LineRef};

/// `"3 h ago"`-style age of `time`.
pub fn ago(time: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(time)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// `file:line` with only the file name, and the full `path:line`.
pub fn location(set: &ConfigSet, at: LineRef) -> (String, String) {
    let file = &set.files[at.file];
    let name = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.path.display().to_string());
    let line = set.line(at).number;
    (format!("{name}:{line}"), set.location(at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        let now = SystemTime::now();
        assert_eq!(ago(now), "just now");
        assert_eq!(ago(now - Duration::from_secs(5 * 60)), "5 min ago");
        assert_eq!(ago(now - Duration::from_secs(3 * 3600 + 5)), "3 h ago");
        assert_eq!(ago(now - Duration::from_secs(2 * 86400)), "2 days ago");
        assert_eq!(ago(now + Duration::from_secs(60)), "just now");
    }
}
