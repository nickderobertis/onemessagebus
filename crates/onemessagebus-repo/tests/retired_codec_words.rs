//! The removed protocol and policy vocabulary cannot creep back into product
//! sources or documentation.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn files(path: &Path, found: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("source directory is readable") {
            files(&entry.expect("directory entry").path(), found);
        }
    } else {
        found.push(path.to_path_buf());
    }
}

#[test]
fn retired_codec_words_are_absent_from_scanned_paths_and_author_words_have_one_allowlist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths = Vec::new();
    for relative in [
        "crates",
        "docs",
        "npm/onemessagebus-sdk/src",
        "python/onemessagebus-sdk/src",
    ] {
        files(&root.join(relative), &mut paths);
    }
    let author_allowed: BTreeSet<&str> = [
        "crates/onemessagebus-agent/src/channel.rs",
        "crates/onemessagebus/src/author.rs",
        "docs/contract.md",
        "docs/queues.md",
        "npm/onemessagebus-sdk/src/generated/messages/agent-queued-commands-1.ts",
        "npm/onemessagebus-sdk/src/generated/messages/agent-queued-reply-1.ts",
        "npm/onemessagebus-sdk/src/generated/messages/agent-reply-envelope-2.ts",
        "npm/onemessagebus-sdk/src/generated/messages/agent-reply-envelope-3.ts",
        "python/onemessagebus-sdk/src/onemessagebus/_generated/messages/agent_queued_commands_v1.py",
        "python/onemessagebus-sdk/src/onemessagebus/_generated/messages/agent_queued_reply_v1.py",
        "python/onemessagebus-sdk/src/onemessagebus/_generated/messages/agent_reply_envelope_v2.py",
        "python/onemessagebus-sdk/src/onemessagebus/_generated/messages/agent_reply_envelope_v3.py",
    ]
    .into_iter()
    .collect();
    let retired = [
        "onejudge",
        "codex",
        "judge side",
        "judge seat",
        "monitor-failed",
        "monitor-completion",
    ];
    for path in paths {
        let relative = path
            .strip_prefix(&root)
            .expect("under root")
            .to_string_lossy()
            .replace('\\', "/");
        if relative.starts_with("crates/") && !relative.contains("/src/") {
            continue;
        }
        if relative.contains("/target/")
            || relative.ends_with("CHANGELOG.md")
            || relative.contains("/tests/")
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (line, content) in text.lines().enumerate() {
            let lower = content.to_lowercase();
            for word in retired {
                assert!(
                    !lower.contains(word),
                    "{relative}:{}: retired word `{word}`",
                    line + 1
                );
            }
            if !author_allowed.contains(relative.as_str()) {
                for word in ["monitor", "pacemaker"] {
                    assert!(
                        !lower
                            .split(|character: char| !character.is_ascii_alphanumeric()
                                && character != '-')
                            .any(|part| part == word),
                        "{relative}:{}: author word `{word}` is outside the allowlist",
                        line + 1
                    );
                }
            }
        }
    }
}
