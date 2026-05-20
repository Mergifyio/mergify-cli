//! Cross-checks the `mergify-merge-queue` skill against the
//! freshly-built test binary.
//!
//! Replaces the pre-port Python `tests/queue/test_skill.py`: the
//! artifacts being validated (a Markdown skill file and the Rust
//! binary's `--list-native-commands` output) have no Python in
//! the picture, so the test lives next to the binary that emits
//! the truth.
//!
//! Each test fires the freshly-built binary via
//! `CARGO_BIN_EXE_mergify` — that's the same artifact `cargo test`
//! built moments earlier, so the test always exercises the
//! current code rather than whatever happens to be on `PATH`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use regex::Regex;
use serde_yaml_ng::Value;

const REQUIRED_SECTIONS: &[&str] = &[
    "## Commands",
    "## Checking Queue Status",
    "## Inspecting a PR",
    "## Queue States",
    "## Troubleshooting",
];

/// Resolve `skills/mergify-merge-queue/SKILL.md` from the
/// repository root. `CARGO_MANIFEST_DIR` points at this crate's
/// directory; two `..` hops up to the workspace root.
fn skill_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("skills")
        .join("mergify-merge-queue")
        .join("SKILL.md")
}

fn skill_content() -> String {
    let path = skill_path();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Ask the binary for its `(group, subcommand)` pairs and collect
/// the subcommands for `group`. Spawning the binary keeps the
/// test honest — a port that adds a native subcommand and its
/// `NATIVE_COMMANDS` entry shows up automatically, no parallel
/// list to drift.
fn native_commands_for_group(group: &str) -> BTreeSet<String> {
    let binary = env!("CARGO_BIN_EXE_mergify");
    let output = Command::new(binary)
        .arg("--list-native-commands")
        .output()
        .unwrap_or_else(|e| panic!("spawn {binary} --list-native-commands: {e}"));
    assert!(
        output.status.success(),
        "mergify --list-native-commands exited {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    stdout
        .lines()
        .filter_map(|line| {
            let (g, sub) = line.split_once(char::is_whitespace)?;
            (g == group).then(|| sub.to_string())
        })
        .collect()
}

#[test]
fn skill_content_is_readable() {
    assert!(!skill_content().is_empty(), "SKILL.md must not be empty");
}

#[test]
fn skill_has_valid_frontmatter() {
    let content = skill_content();
    // Extract YAML frontmatter between --- markers — the same
    // shape Claude Code's skill loader expects.
    let re = Regex::new(r"(?s)^---\n(.+?)\n---\n").expect("frontmatter regex compiles");
    let captures = re
        .captures(&content)
        .expect("Skill must have YAML frontmatter");
    let yaml = captures.get(1).unwrap().as_str();
    let parsed: Value = serde_yaml_ng::from_str(yaml).expect("frontmatter is valid YAML");
    let mapping = parsed
        .as_mapping()
        .expect("frontmatter must be a YAML mapping");
    let name = mapping
        .get(Value::from("name"))
        .and_then(Value::as_str)
        .expect("frontmatter must have 'name'");
    assert_eq!(name, "mergify-merge-queue");
    assert!(
        mapping.get(Value::from("description")).is_some(),
        "frontmatter must have 'description'",
    );
}

#[test]
fn skill_has_required_sections() {
    let content = skill_content();
    for section in REQUIRED_SECTIONS {
        assert!(
            content.contains(section),
            "Skill is missing required section: {section}",
        );
    }
}

#[test]
fn skill_references_valid_commands() {
    let content = skill_content();
    let re = Regex::new(r"mergify queue ([\w-]+)").expect("reference regex compiles");
    // BTreeSet so iteration order — and therefore which assertion
    // trips first — is deterministic. Same for `available` below:
    // its `Debug` output ends up in the failure message and would
    // otherwise reshuffle between runs.
    let referenced: BTreeSet<String> = re
        .captures_iter(&content)
        .map(|c| c[1].to_string())
        .collect();
    let available = native_commands_for_group("queue");

    for cmd in &referenced {
        assert!(
            available.contains(cmd),
            "Skill references 'mergify queue {cmd}' but it's not a Rust-native \
             command reported by `mergify --list-native-commands`. \
             Available: {available:?}",
        );
    }
}
