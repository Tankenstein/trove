mod common;

use std::fs;
use std::path::Path;

use common::{
    Home, append, append_raw, claude, codex, find, id, messages, set_age, titles, write_lines,
};
use trove::index::{self, Progress};
use trove::search;

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn finds_chats_in_every_store() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("default claude store", &shop)],
    );
    home.claude(
        ".claude-alt",
        &shop,
        &id(2),
        &[claude::user("second claude account", &shop)],
    );
    home.claude(
        "config/claude-custom",
        &shop,
        &id(3),
        &[claude::user("claude config dir", &shop)],
    );
    home.codex(
        ".codex",
        &id(4),
        &codex::session(&id(4), &shop, "default codex home", "ok"),
    );
    home.codex(
        ".codex-alt",
        &id(5),
        &codex::session(&id(5), &shop, "second codex home", "ok"),
    );
    home.codex(
        "elsewhere/codex",
        &id(6),
        &codex::session(&id(6), &shop, "codex home env var", "ok"),
    );
    home.codex_archived(
        ".codex",
        &id(7),
        &codex::session(&id(7), &shop, "archived codex chat", "ok"),
    );
    fs::create_dir_all(home.path().join(".codex-shared")).unwrap();
    std::os::unix::fs::symlink(
        home.path().join(".codex/sessions"),
        home.path().join(".codex-shared/sessions"),
    )
    .unwrap();

    let env = home
        .env()
        .with(
            "CLAUDE_CONFIG_DIR",
            home.path().join("config/claude-custom"),
        )
        .with("CODEX_HOME", home.path().join("elsewhere/codex"));
    let conn = home.refresh_with(&env);
    let all = search::load(&conn).unwrap();
    assert_eq!(
        sorted(all.iter().map(|s| s.title.clone()).collect()),
        [
            "archived codex chat",
            "claude config dir",
            "codex home env var",
            "default claude store",
            "default codex home",
            "second claude account",
            "second codex home"
        ]
    );
    let archived = all
        .iter()
        .find(|s| s.title == "archived codex chat")
        .unwrap();
    assert!(archived.archived);
    assert_eq!(archived.agent.name(), "codex");
    assert_eq!(archived.sid, id(7));
    assert!(all.iter().all(|s| s.cwd == shop));
}

#[test]
fn ignores_what_isnt_a_chat() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("real chat", &shop)],
    );
    let sub = home
        .path()
        .join(".claude/projects")
        .join(common::encode(&shop))
        .join(id(1))
        .join("subagents/agent-a1.jsonl");
    write_lines(&sub, &[claude::user("subagent prompt", &shop)]);
    write_lines(
        &home.path().join(".claude/projects/x/notes.jsonl"),
        &[claude::user("not a session", &shop)],
    );
    write_lines(
        &home.path().join(".other-app/projects/p/readme.jsonl"),
        &[claude::user("other app", &shop)],
    );
    write_lines(
        &home.path().join(".claude/sessions/123.json"),
        &["{}".into()],
    );
    home.codex(
        ".codex",
        &id(2),
        &[
            codex::review_meta(&id(2), &shop),
            codex::user("review transcript").remove(0),
        ],
    );
    home.codex(
        ".codex",
        &id(3),
        &[codex::meta(&id(3), &shop), codex::injected(&shop)],
    );
    home.claude(
        ".claude",
        &shop,
        &id(4),
        &[
            claude::tool_call(&shop),
            claude::tool_result("output", &shop),
        ],
    );

    let conn = home.refresh();
    assert_eq!(titles(&conn, ""), ["real chat"]);
    assert!(titles(&conn, "review transcript").is_empty());
    let text: i64 = conn
        .query_row(
            "SELECT count(*) FROM fts WHERE text LIKE '%review transcript%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(text, 0, "text of hidden sessions isn't indexed");
}

#[test]
fn skips_tool_noise_and_injected_context() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[
            claude::user("why is checkout slow", &shop),
            claude::tool_call(&shop),
            claude::tool_result("SELECT * FROM orders -- zebra", &shop),
            claude::assistant("An N+1 query in the cart.", &shop),
        ],
    );
    let mut lines = codex::session(&id(2), &shop, "rename the flag", "done");
    lines.push(codex::compacted("rename the flag, and also giraffe"));
    home.codex(".codex", &id(2), &lines);

    let conn = home.refresh();
    assert_eq!(
        messages(&conn, "why is checkout slow"),
        ["why is checkout slow", "An N+1 query in the cart."]
    );
    assert!(
        titles(&conn, "zebra").is_empty(),
        "tool output isn't indexed"
    );
    assert!(
        titles(&conn, "environment_context").is_empty(),
        "injected context isn't indexed"
    );
    assert!(
        titles(&conn, "giraffe").is_empty(),
        "compacted history isn't indexed"
    );
    assert_eq!(
        messages(&conn, "rename the flag"),
        ["rename the flag", "done"]
    );
}

#[test]
fn grown_files_are_parsed_from_where_they_stopped() {
    let home = Home::new();
    let shop = home.project("shop");
    let path = home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("first prompt alpha", &shop)],
    );
    let conn = home.refresh();
    assert_eq!(titles(&conn, "alpha"), ["first prompt alpha"]);

    append(
        &path,
        &[
            claude::assistant("reply bravo", &shop),
            claude::user("second prompt charlie", &shop),
        ],
    );
    let conn = home.refresh();
    assert_eq!(titles(&conn, "charlie"), ["first prompt alpha"]);
    assert_eq!(
        messages(&conn, "first prompt alpha"),
        ["first prompt alpha", "reply bravo", "second prompt charlie"]
    );

    let conn = home.refresh();
    assert_eq!(messages(&conn, "first prompt alpha").len(), 3);

    let codex_path = home.codex(
        ".codex",
        &id(2),
        &codex::session(&id(2), &shop, "codex delta", "one"),
    );
    home.refresh();
    let mut more = codex::user("codex echo");
    more.extend(codex::assistant("two"));
    append(&codex_path, &more);
    let conn = home.refresh();
    assert_eq!(
        messages(&conn, "codex delta"),
        ["codex delta", "one", "codex echo", "two"]
    );
}

#[test]
fn half_written_lines_wait_for_the_rest() {
    let home = Home::new();
    let shop = home.project("shop");
    let path = home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("complete line", &shop)],
    );
    home.refresh();
    let line = claude::user("half written", &shop);
    let (start, end) = line.split_at(line.len() / 2);
    append_raw(&path, start);
    let conn = home.refresh();
    assert!(titles(&conn, "half").is_empty());

    append_raw(&path, &format!("{end}\n"));
    let conn = home.refresh();
    assert_eq!(
        messages(&conn, "complete line"),
        ["complete line", "half written"]
    );
}

#[test]
fn replaced_and_deleted_files() {
    let home = Home::new();
    let shop = home.project("shop");
    let lines = [
        claude::user("original prompt kiwi", &shop),
        claude::assistant(&"long reply ".repeat(50), &shop),
    ];
    let path = home.claude(".claude", &shop, &id(1), &lines);
    home.refresh();

    write_lines(&path, &[claude::user("rewritten prompt mango", &shop)]);
    let conn = home.refresh();
    assert!(titles(&conn, "kiwi").is_empty());
    assert_eq!(titles(&conn, "mango"), ["rewritten prompt mango"]);

    let tmp = path.with_extension("tmp");
    write_lines(
        &tmp,
        &[
            claude::user("replacement prompt papaya", &shop),
            claude::assistant(&"x".repeat(900), &shop),
        ],
    );
    fs::rename(&tmp, &path).unwrap();
    let conn = home.refresh();
    assert!(titles(&conn, "mango").is_empty());
    assert_eq!(messages(&conn, "replacement prompt papaya").len(), 2);

    fs::remove_file(&path).unwrap();
    let conn = home.refresh();
    assert!(titles(&conn, "").is_empty());
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn titles_follow_renames() {
    let home = Home::new();
    let shop = home.project("shop");
    let path = home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("help me with the thing", &shop)],
    );
    let conn = home.refresh();
    assert_eq!(titles(&conn, ""), ["help me with the thing"]);

    append(&path, &[claude::ai_title("Generated title")]);
    assert_eq!(titles(&home.refresh(), ""), ["Generated title"]);
    append(&path, &[claude::custom_title("My chosen name")]);
    assert_eq!(titles(&home.refresh(), ""), ["My chosen name"]);
    append(&path, &[claude::ai_title("Another generated title")]);
    let conn = home.refresh();
    assert_eq!(titles(&conn, ""), ["My chosen name"]);
    assert_eq!(titles(&conn, "chosen"), ["My chosen name"]);

    let home = Home::new();
    home.codex(
        ".codex",
        &id(2),
        &codex::session(&id(2), &shop, "first codex prompt", "ok"),
    );
    assert_eq!(titles(&home.refresh(), ""), ["first codex prompt"]);
    home.codex_name(".codex", &id(2), "Auto name");
    home.codex_name(".codex", &id(2), "Renamed thread");
    let conn = home.refresh();
    assert_eq!(titles(&conn, ""), ["Renamed thread"]);
    assert_eq!(titles(&conn, "renamed"), ["Renamed thread"]);
}

#[test]
fn resumes_from_the_launch_directory() {
    let home = Home::new();
    let shop = home.project("shop");
    let api = home.project("shop/api");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[
            claude::user("start here", &api),
            claude::user("then here", &shop),
        ],
    );
    let conn = home.refresh();
    let s = &find(&conn, "", Path::new("/"))[0];
    assert_eq!(s.cwd, shop);
    assert_eq!(
        trove::resume::command(s, trove::shell::Quoting::Posix),
        format!("cd {shop} && claude --resume {}", id(1))
    );
}

#[test]
fn without_the_launch_directory_the_first_folder_is_used() {
    let home = Home::new();
    let shop = home.project("shop");
    let api = home.project("shop/api");
    let path = home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("start in api", &api)],
    );
    let conn = home.refresh();
    assert_eq!(find(&conn, "", Path::new("/"))[0].cwd, api);

    append(&path, &[claude::user("back in shop", &shop)]);
    let conn = home.refresh();
    assert_eq!(find(&conn, "", Path::new("/"))[0].cwd, shop);
}

#[test]
fn big_files_are_parsed_in_order() {
    let home = Home::new();
    let shop = home.project("shop");
    let mut lines = vec![codex::meta(&id(1), &shop)];
    lines.extend(codex::user("needle one"));
    for i in 0..3 {
        lines.extend((0..800).map(|_| codex::filler(10_000)));
        lines.extend(codex::assistant(&format!(
            "needle {}",
            ["two", "three", "four"][i]
        )));
    }
    home.codex(".codex", &id(1), &lines);
    let conn = home.refresh();
    assert_eq!(
        messages(&conn, "needle one"),
        ["needle one", "needle two", "needle three", "needle four"]
    );
}

#[test]
fn every_word_must_match_somewhere() {
    let home = Home::new();
    let shop = home.project("shop");
    let zeppelin = home.project("zeppelin");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("stripe webhook retries", &shop)],
    );
    home.claude(
        ".claude",
        &shop,
        &id(2),
        &[claude::user("stripe invoices", &shop)],
    );
    home.claude(
        ".claude",
        &zeppelin,
        &id(3),
        &[claude::user("airship chat", &zeppelin)],
    );
    home.codex(
        ".codex",
        &id(4),
        &codex::session(&id(4), &zeppelin, "codex airship chat", "ok"),
    );
    home.claude(
        ".claude",
        &shop,
        &id(5),
        &[claude::user("see github.com/acme/shop/pull/501", &shop)],
    );
    let conn = home.refresh();

    assert_eq!(titles(&conn, "stripe webhook"), ["stripe webhook retries"]);
    assert_eq!(
        sorted(titles(&conn, "stri")),
        ["stripe invoices", "stripe webhook retries"]
    );
    assert_eq!(
        sorted(titles(&conn, "zeppelin")),
        ["airship chat", "codex airship chat"]
    );
    assert_eq!(titles(&conn, "codex zeppel"), ["codex airship chat"]);
    assert_eq!(
        titles(&conn, "pull/501"),
        ["see github.com/acme/shop/pull/501"]
    );
    assert!(titles(&conn, "stripe zeppelin").is_empty());
    assert!(titles(&conn, "nothing-matches-this").is_empty());
}

#[test]
fn ranking() {
    let home = Home::new();
    let shop = home.project("shop");
    let other = home.project("other");

    let a = home.claude(
        ".claude",
        &other,
        &id(1),
        &[
            claude::user("upgrade the cluster", &other),
            claude::custom_title("Kubernetes upgrade"),
        ],
    );
    let b = home.claude(
        ".claude",
        &other,
        &id(2),
        &[
            claude::user("fix the login page", &other),
            claude::assistant("unrelated to kubernetes, but anyway…", &other),
        ],
    );
    set_age(&a, 3600);
    set_age(&b, 7200);
    let old = home.claude(
        ".claude",
        &other,
        &id(3),
        &[claude::user("tune postgres vacuum", &other)],
    );
    let new = home.claude(
        ".claude",
        &other,
        &id(4),
        &[claude::user("tune postgres indexes", &other)],
    );
    set_age(&old, 90 * 86_400);
    set_age(&new, 86_400);
    let here = home.claude(
        ".claude",
        &shop,
        &id(5),
        &[claude::user("chat in shop", &shop)],
    );
    set_age(&here, 200 * 86_400);

    let conn = home.refresh();
    assert_eq!(
        titles(&conn, "kubernetes"),
        ["Kubernetes upgrade", "fix the login page"]
    );
    assert_eq!(
        titles(&conn, "postgres"),
        ["tune postgres indexes", "tune postgres vacuum"]
    );
    let listed: Vec<String> = find(&conn, "", Path::new(&shop))
        .into_iter()
        .map(|s| s.title)
        .collect();
    assert_eq!(listed[0], "chat in shop");
    assert_eq!(
        listed[1..],
        [
            "Kubernetes upgrade",
            "fix the login page",
            "tune postgres indexes",
            "tune postgres vacuum"
        ]
    );
}

#[test]
fn same_chat_in_two_stores_is_listed_once() {
    let home = Home::new();
    let shop = home.project("shop");
    let old = home.codex(
        ".codex",
        &id(1),
        &codex::session(&id(1), &shop, "migrated chat", "ok"),
    );
    home.codex(
        ".codex-alt",
        &id(1),
        &codex::session(&id(1), &shop, "migrated chat", "ok"),
    );
    set_age(&old, 86_400);
    let conn = home.refresh();
    assert_eq!(titles(&conn, "migrated"), ["migrated chat"]);
}

#[test]
fn rebuilds_a_broken_or_outdated_index() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("survives rebuilds", &shop)],
    );
    home.refresh();

    let conn = index::open(&home.db()).unwrap();
    conn.execute("UPDATE meta SET v = 'old' WHERE k = 'version'", [])
        .unwrap();
    drop(conn);
    let conn = index::open(&home.db()).unwrap();
    assert!(search::load(&conn).unwrap().is_empty());
    assert_eq!(titles(&home.refresh(), "rebuilds"), ["survives rebuilds"]);

    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", home.db().display()));
    }
    fs::write(home.db(), b"this is not a database").unwrap();
    assert_eq!(titles(&home.refresh(), "rebuilds"), ["survives rebuilds"]);
}

#[test]
fn concurrent_refreshes_dont_duplicate() {
    let home = Home::new();
    let shop = home.project("shop");
    for n in 0..20 {
        home.claude(
            ".claude",
            &shop,
            &id(n),
            &[
                claude::user(&format!("chat number {n}"), &shop),
                claude::assistant("ok", &shop),
            ],
        );
    }
    let db = home.db();
    index::open(&db).unwrap();
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                let conn = index::open(&db).unwrap();
                index::refresh(&conn, &db, &home.env(), &Progress::default(), &mut || {}).unwrap();
            });
        }
    });
    let conn = index::open(&db).unwrap();
    assert_eq!(titles(&conn, "").len(), 20);
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM fts WHERE rowid & 3 <> 0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 40);
}

#[test]
fn reports_progress_and_commits_as_it_goes() {
    let home = Home::new();
    let shop = home.project("shop");
    for n in 0..5 {
        home.claude(
            ".claude",
            &shop,
            &id(n),
            &[claude::user(&format!("chat {n}"), &shop)],
        );
    }
    let db = home.db();
    let conn = index::open(&db).unwrap();
    let progress = Progress::default();
    let mut commits = 0;
    index::refresh(&conn, &db, &home.env(), &progress, &mut || commits += 1).unwrap();
    use std::sync::atomic::Ordering::Relaxed;
    assert!(commits >= 1);
    assert!(progress.total.load(Relaxed) > 0);
    assert_eq!(progress.done.load(Relaxed), progress.total.load(Relaxed));
}

#[test]
fn records_model_start_and_last_prompt() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[
            claude::user("first ask", &shop),
            claude::assistant("ok", &shop),
            claude::user("second ask", &shop),
        ],
    );
    home.codex(
        ".codex",
        &id(2),
        &codex::session(&id(2), &shop, "codex ask", "done"),
    );
    let conn = home.refresh();
    let all = search::load(&conn).unwrap();
    let start: i64 = "2026-09-01T10:00:00Z"
        .parse::<jiff::Timestamp>()
        .unwrap()
        .as_second();

    let c = all.iter().find(|s| s.agent.name() == "claude").unwrap();
    assert_eq!(
        (
            c.model.as_str(),
            c.last_prompt.as_str(),
            c.messages,
            c.started
        ),
        ("claude-opus-5-5", "second ask", 3, start)
    );
    let x = all.iter().find(|s| s.agent.name() == "codex").unwrap();
    assert_eq!(
        (
            x.model.as_str(),
            x.last_prompt.as_str(),
            x.messages,
            x.started
        ),
        ("gpt-6-astra", "codex ask", 2, start)
    );
    assert_eq!(titles(&conn, "astra"), ["codex ask"]);
}

#[test]
fn urls_match_without_their_extras() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user(
            "review https://github.com/acme/shop/pull/501 please",
            &shop,
        )],
    );
    home.claude(
        ".claude",
        &shop,
        &id(2),
        &[claude::user(
            "and https://github.com/acme/shop/pull/502",
            &shop,
        )],
    );
    let conn = home.refresh();
    for q in [
        "https://github.com/acme/shop/pull/501",
        "https://github.com/acme/shop/pull/501#discussion_r2398123",
        "https://github.com/acme/shop/pull/501?w=1",
        "https://github.com/acme/shop/pull/501/files",
        "https://github.com/acme/shop/pull/501/files#diff-abc123",
        "https://github.com/acme/shop/pull/501/",
        "http://github.com/acme/shop/pull/501",
        "github.com/acme/shop/pull/501",
    ] {
        assert_eq!(
            titles(&conn, q),
            ["review https://github.com/acme/shop/pull/501 please"],
            "{q}"
        );
    }
    assert!(titles(&conn, "https://github.com/acme/shop/pull/999").is_empty());
}

#[test]
fn quoted_phrases_match_in_order() {
    let home = Home::new();
    let shop = home.project("shop");
    home.claude(
        ".claude",
        &shop,
        &id(1),
        &[claude::user("the retry path is broken", &shop)],
    );
    home.claude(
        ".claude",
        &shop,
        &id(2),
        &[claude::user("path to retry later", &shop)],
    );
    let conn = home.refresh();
    assert_eq!(
        titles(&conn, "\"retry path\""),
        ["the retry path is broken"]
    );
    assert_eq!(
        sorted(titles(&conn, "retry path")),
        ["path to retry later", "the retry path is broken"]
    );
}

#[test]
fn previews_show_the_start_the_matches_and_the_end() {
    let home = Home::new();
    let shop = home.project("shop");
    let texts: Vec<String> = (1..=20)
        .map(|n| {
            if n == 10 {
                "the needle is here".to_owned()
            } else {
                format!("message {n}")
            }
        })
        .collect();
    let lines: Vec<String> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i % 2 == 0 {
                claude::user(t, &shop)
            } else {
                claude::assistant(t, &shop)
            }
        })
        .collect();
    home.claude(".claude", &shop, &id(1), &lines);
    let conn = home.refresh();
    let sessions = search::load(&conn).unwrap();
    let q = search::query(&conn, "needle").unwrap();
    let hits = search::search(&conn, &sessions, &q, Path::new("/"), common::now()).unwrap();
    let texts: Vec<String> = search::preview(&conn, &q, sessions[0].id, &hits[0].rows)
        .unwrap()
        .into_iter()
        .map(|e| e.text)
        .collect();
    let expected = [1, 2, 9, 10, 11, 15, 16, 17, 18, 19, 20].map(|n| {
        if n == 10 {
            "the needle is here".to_owned()
        } else {
            format!("message {n}")
        }
    });
    assert_eq!(texts, expected);
}

#[test]
fn longer_chats_rank_a_little_higher() {
    let home = Home::new();
    let shop = home.project("shop");
    let short = home.claude(
        ".claude",
        &shop,
        &id(1),
        &[
            claude::custom_title("Short chat"),
            claude::user("deploy the billing service", &shop),
        ],
    );
    let mut lines = vec![
        claude::custom_title("Long chat"),
        claude::user("deploy the billing service", &shop),
    ];
    lines.extend((0..40).map(|n| claude::assistant(&format!("step {n}"), &shop)));
    let long = home.claude(".claude", &shop, &id(2), &lines);
    let titled = home.claude(
        ".claude",
        &shop,
        &id(3),
        &[
            claude::custom_title("Billing"),
            claude::user("something else", &shop),
        ],
    );
    for path in [&short, &long, &titled] {
        set_age(path, 3600);
    }
    let conn = home.refresh();
    assert_eq!(
        titles(&conn, "billing"),
        ["Billing", "Long chat", "Short chat"]
    );
}
