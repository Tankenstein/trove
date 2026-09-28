mod common;

use std::io::{BufRead, BufReader};
use std::process::Stdio;

use common::{Home, claude, codex, id, set_age};
use serde_json::{Value, json};

fn stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "trove failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn help_version_and_bad_options() {
    let home = Home::new();
    let help = stdout(&home.trove().arg("--help").output().unwrap());
    assert!(help.contains("Usage: trove [words...]"));
    let version = stdout(&home.trove().arg("-V").output().unwrap());
    assert_eq!(
        version.trim(),
        format!("trove {}", env!("CARGO_PKG_VERSION"))
    );

    let bad = home.trove().arg("--bogus").output().unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown option --bogus"));
}

#[test]
fn piped_output_is_one_tab_separated_line_per_match() {
    let home = Home::new();
    let shop = home.project("shop");
    let a = home.claude(
        ".claude",
        &shop,
        &id(1),
        &[
            claude::user("fix the stripe webhook", &shop),
            claude::ai_title("Stripe webhook fix"),
        ],
    );
    let b = home.codex(
        ".codex",
        &id(2),
        &codex::session(&id(2), &shop, "stripe invoices", "ok"),
    );
    home.claude(
        ".claude",
        &shop,
        &id(3),
        &[claude::user("unrelated", &shop)],
    );
    set_age(&a, 2 * 3600);
    set_age(&b, 3 * 86_400);

    let text = stdout(&home.trove().arg("stripe").output().unwrap());
    let rows: Vec<Vec<String>> = text
        .lines()
        .map(|l| l.split('\t').map(str::to_owned).collect())
        .collect();
    let (resume_a, resume_b) = (
        format!("cd {shop} && claude --resume {}", id(1)),
        format!("cd {shop} && codex resume {}", id(2)),
    );
    let expected = [
        [
            "2h",
            "claude",
            "~/Projects/shop",
            "Stripe webhook fix",
            resume_a.as_str(),
        ],
        [
            "3d",
            "codex",
            "~/Projects/shop",
            "stripe invoices",
            resume_b.as_str(),
        ],
    ];
    assert_eq!(rows, expected.map(|r| r.map(str::to_owned)));
}

#[test]
fn no_words_lists_everything_and_double_dash_ends_options() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(".claude", &shop, &id(1), &[claude::user("first", &shop)]);
    home.claude(
        ".claude",
        &shop,
        &id(2),
        &[claude::user("second -x", &shop)],
    );
    assert_eq!(stdout(&home.trove().output().unwrap()).lines().count(), 2);
    let out = stdout(&home.trove().args(["--", "-x"]).output().unwrap());
    assert_eq!(
        out.lines()
            .map(|l| l.split('\t').nth(3).unwrap())
            .collect::<Vec<_>>(),
        ["second -x"]
    );
}

#[test]
fn nothing_found_prints_nothing_and_exits_with_1() {
    let home = Home::new();
    let out = home.trove().arg("anything").output().unwrap();
    assert_eq!((out.status.code(), out.stdout.len()), (Some(1), 0));
}

fn two_chats(home: &Home) -> String {
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[
            claude::user("fix the stripe webhook", &shop),
            claude::assistant("It retries twice.", &shop),
            claude::ai_title("Stripe webhook fix"),
        ],
    );
    home.codex(
        ".codex",
        &id(2),
        &codex::session(&id(2), &shop, "otter habitats", "Rivers and coasts."),
    );
    shop
}

#[test]
fn json_lines_describe_each_match() {
    let home = Home::new();
    let shop = two_chats(&home);
    let text = stdout(&home.trove().args(["--json", "stripe"]).output().unwrap());
    let chats: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(chats.len(), 1, "{text}");
    let file = home
        .path()
        .join(".claude/projects")
        .join(common::encode(&shop))
        .join(format!("{}.jsonl", id(1)));
    assert_eq!(
        chats[0],
        json!({
            "id": id(1),
            "agent": "claude",
            "model": "claude-opus-5-5",
            "folder": shop,
            "title": "Stripe webhook fix",
            "started": "2026-09-01T10:00:00Z",
            "updated": chats[0]["updated"],
            "messages": 2,
            "archived": false,
            "file": file,
            "resume": format!("cd {shop} && claude --resume {}", id(1)),
            "matches": [{"role": "user", "text": "fix the stripe webhook"}],
        })
    );
    assert!(chats[0]["updated"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn show_prints_whole_chats_in_the_order_given() {
    let home = Home::new();
    let shop = two_chats(&home);
    let text = stdout(
        &home
            .trove()
            .args(["--show", &id(2), &id(1), &id(2)])
            .output()
            .unwrap(),
    );
    let titles: Vec<&str> = text.lines().filter(|l| l.starts_with("# ")).collect();
    assert_eq!(titles, ["# otter habitats", "# Stripe webhook fix"]);
    for expected in [
        "agent: claude (opus 5.5)\n",
        &format!("folder: {shop}\n"),
        &format!("id: {}\n", id(1)),
        "## you\n\nfix the stripe webhook\n\n## claude\n\nIt retries twice.\n",
        "## you\n\notter habitats\n\n## codex\n\nRivers and coasts.\n",
    ] {
        assert!(
            text.contains(expected),
            "{expected:?} missing from:\n{text}"
        );
    }

    let text = stdout(
        &home
            .trove()
            .args(["--show", "--json", &id(1)])
            .output()
            .unwrap(),
    );
    let chat: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(
        chat["transcript"],
        json!([
            {"role": "user", "text": "fix the stripe webhook"},
            {"role": "assistant", "text": "It retries twice."},
        ])
    );
    assert_eq!(
        (chat["id"].as_str(), chat.get("matches")),
        (Some(&*id(1)), None)
    );
}

#[test]
fn unknown_ids_are_reported_and_exit_with_1() {
    let home = Home::new();
    two_chats(&home);
    let out = home
        .trove()
        .args(["--show", "nope", &id(1)])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("# Stripe webhook fix\n"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no chat with id nope"));

    let bare = home.trove().arg("--show").output().unwrap();
    assert_eq!(bare.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bare.stderr).contains("--show needs"));
}

#[test]
fn sees_changes_between_runs() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("first chat", &shop)],
    );
    assert_eq!(stdout(&home.trove().output().unwrap()).lines().count(), 1);
    home.codex(
        ".codex",
        &id(2),
        &codex::session(&id(2), &shop, "second chat", "ok"),
    );
    assert_eq!(stdout(&home.trove().output().unwrap()).lines().count(), 2);
}

#[test]
fn index_is_private_and_in_the_cache_dir() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("secret plans", &shop)],
    );
    stdout(&home.trove().output().unwrap());
    let cache = if cfg!(target_os = "macos") {
        home.path().join("Library/Caches")
    } else {
        home.path().join(".cache")
    };
    let db = cache.join("trove/index.db");
    assert!(db.exists(), "{} missing", db.display());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(db.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn closing_the_pipe_early_is_fine() {
    let home = Home::new();
    let shop = home.project("shop");
    for n in 0..800 {
        home.claude(
            ".claude",
            &shop,
            &id(n),
            &[claude::user(
                &format!("chat {n} {}", "padding ".repeat(10)),
                &shop,
            )],
        );
    }
    let mut child = home
        .trove()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    assert!(first.contains("chat"));
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn piped_output_never_carries_terminal_escapes() {
    let home = Home::new();
    let dir = home.project("esc\x1b[31mred\x07");
    home.claude(
        ".claude",
        &dir,
        &id(1),
        &[claude::user("title \x1b]0;pwned\x07 here", &dir)],
    );
    let out = home.trove().output().unwrap();
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text:?}");
    assert!(
        !out.stdout.iter().any(|&b| b == 0x1b || b == 0x07),
        "{text:?}"
    );
    let out = home.trove().args(["--show", &id(1)]).output().unwrap();
    let text = stdout(&out);
    assert!(text.contains("title ?]0;pwned? here"), "{text:?}");
    assert!(
        !out.stdout.iter().any(|&b| b == 0x1b || b == 0x07),
        "{text:?}"
    );
}
