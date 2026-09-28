# trove

Find any coding agent chat on your machine (Claude Code, Codex, OpenCode, Copilot CLI, Pi,
Gemini CLI) and pick up where you left off: in the right folder, with the conversation resumed.

```
$ trove stripe webhook
  stripe webhook                                                                3 of 214
 ───────────────────────────────────────────────────────────────────────────────────────
▌ Retry failed Stripe webhooks          claude · opus 5.5 · 42 msgs · Mar 2 · 2d
▌ shop  the stripe webhook fires twice after a timeout, can you find out why?

  Local Stripe setup for a teammate    codex · gpt-6-astra · 9 msgs · Feb 18 · 2w
  shop-api  …forward webhook events to localhost with the Stripe CLI…
───────────────────────────────────────────────────────────────────────────────────────
~/code/shop · claude · opus 5.5 · 42 messages · started Mar 2 14:10 · active 2d ago
you     the stripe webhook fires twice after a timeout, can you find out why?
claude  The handler isn't idempotent: a retry after the 10 s timeout runs it again…
  ⋯
you     ship the idempotency key change
claude  Done. Webhook events are now deduplicated on their event id…
```

Type to search titles, folders and messages. <kbd>Enter</kbd> resumes the chat, <kbd>Esc</kbd> quits.

Each chat shows its title with its agent, model, size, start date and last activity, then its
folder and the part that matches (or your last prompt). While searching, longer chats rank a
little higher when matches are otherwise equal. The preview shows how the selected chat started
and ended, or while searching, where it matches and the messages around each match. It sits
beside the list on terminals 130 columns or wider, and under it otherwise. Moving the selection
never moves anything else.

## Install

```
cargo install --path .
```

This installs the `trove` binary. The first time you resume a chat, `trove` offers to add one
line to your shell config (zsh, bash or fish), so that your shell stays in the chat's folder
after the chat ends. Without it, resuming still works; you just end up back where you started.

## Usage

```
trove [words...]
trove --json [words...]
trove --show [--json] ID...
```

- Every word must appear somewhere in the chat: its title, its folder, the agent or model name
  (`claude`, `opencode`, `opus`, `gpt-6`), or any prompt or reply. Words match as prefixes.
  `trove codex billing` finds Codex chats about billing.
- `"quoted phrases"`, and words with punctuation like `pull/501`, match their parts in order.
- URLs match without their query string, fragment or trailing slash, and if nothing mentions
  the exact page, a shorter form of it: `…/pull/501/files#diff-3` finds chats that mention
  `…/pull/501`. Shortening stops at an id, so `…/pull/999` doesn't match other pull requests.
- With no words, recent chats are listed, with those from the current folder first.
- Keys: type to search, <kbd>↑</kbd>/<kbd>↓</kbd> (or <kbd>Ctrl-P</kbd>/<kbd>Ctrl-N</kbd>)
  to move, <kbd>Enter</kbd> to resume, <kbd>Esc</kbd> to quit. <kbd>Ctrl-U</kbd> and
  <kbd>Ctrl-W</kbd> clear the query or its last word.
- When output is piped, matches are printed instead, one per line, tab-separated: age, agent,
  folder, title, resume command. `trove migration | head -1 | cut -f5` prints the command for
  the best match.
- `--json` prints matches as JSON instead, one chat per line, and never opens the picker, even
  in a terminal. Each has the chat's `id`, `agent`, `model`, `folder`, `title`, `started` and
  `updated` (in UTC), `messages` (how many), `archived`, `file`, `resume` (the command), and
  `matches`: up to three of the messages that match, each with its `role` and an excerpt.
- `--show ID...` prints the chats with those ids whole: a heading with the title, the chat's
  details, then every prompt and reply. With `--json`, each is one line of JSON, with its
  messages in `transcript`.
- `trove` exits with 1 when nothing matches or an id isn't found.

## For agents

Coding agents can use `trove` to find and read your earlier chats, including ones with other
agents. To tell them about it, add this to your `CLAUDE.md` or `AGENTS.md`:

```
To find earlier conversations, run `trove --json <words>`: one JSON object per chat, best match
first. To read chats, run `trove --show <id>...`.
```

To have one agent take over a chat you had with another, start it in the chat's folder and
give it the id: *"Continue the Codex chat 0192a3b4-…: read it with `trove --show`, then check
`git status` for where it left off."* The chat has every prompt and reply, but not the tool
calls, so the new agent should look at the files to see what was done.

An agent that reads a chat reads everything in it: secrets you pasted, and text from elsewhere,
like web pages, which can carry instructions of their own.

## Supported agents

Stores are recognised by their layout, so nothing needs configuring. Resuming changes to the
chat's folder, then runs the agent's own resume command.

| Agent | Chats | Resumed with |
|---|---|---|
| Claude Code | `projects/<folder>/<id>.jsonl` in `$CLAUDE_CONFIG_DIR`, `~/.claude`, `~/.config/claude`, or any other dot-directory in your home (such as a second account's `~/.claude-2`) | `claude --resume <id>` |
| Codex | `sessions/…/rollout-*.jsonl` and `archived_sessions/` in `$CODEX_HOME`, `~/.codex`, or any other dot-directory (such as `~/.codex-2`) | `codex resume <id>` |
| OpenCode* | the SQLite database `~/.local/share/opencode/opencode.db` (or under `$XDG_DATA_HOME`), OpenCode 1.2 and later | `opencode --session <id>` |
| Copilot CLI* | `~/.copilot/session-state/<id>/events.jsonl` (or under `$COPILOT_HOME`), titled by `workspace.yaml` | `copilot --resume=<id>` |
| Pi* | `~/.pi/agent/sessions/<folder>/<time>_<id>.jsonl` (or under `$PI_CODING_AGENT_DIR`) | `pi --session <file>` |
| Gemini CLI* | `~/.gemini/tmp/<project>/chats/session-*.jsonl` (or under `$GEMINI_CLI_HOME`), Gemini CLI 0.39 and later | `gemini --resume <id>` |

\* Untested: written from each agent's source code and docs, and tested only against files and
databases made to match them, not against a real install. Reports and fixes are welcome.

Stores shared through symlinks are read once. Subagent and review sessions aren't listed.
Archived chats are shown dimmed; chats whose folder no longer exists show the folder in red.

Only prompts and replies are searchable. Tool calls and output, injected context (instructions
files, environment details, system reminders) and compacted history replays are left out.

## How it works

`trove` keeps a search index in your cache directory (`~/Library/Caches/trove/index.db` on
macOS, `~/.cache/trove/index.db` on Linux), readable only by you. It never writes to the
agents' files, and opens their databases read-only.

Each launch checks every chat file's size and modification time against the index. Chat files
are only ever appended to, so a grown file is read only from where the last run stopped; files
that were replaced are read again, and deleted ones are dropped. A database is looked at again
only when it changed, and then only its changed chats are read. Typically this takes a few
milliseconds. The picker shows results immediately and refreshes them when the update finishes.

The index is only a cache: delete it any time and it's rebuilt on the next run. The first build
takes a second or two, even for tens of gigabytes of chats.

## Uninstall

Delete the binary (`cargo uninstall trove-cli`), the index, `~/.config/trove/`, and the line
marked `# trove` in your shell config (or `~/.config/fish/functions/trove.fish`).

## Development

```
cargo test
```

The tests build fake home directories with each agent's stores in them. They cover parsing,
incremental indexing and search through the library, the binary's piped output, and the picker
in a pseudo-terminal, where fake `claude` and `codex` executables record where they were
started. The zsh and bash hooks are tested there too; fish is tested when it's installed.

### Adding an agent

Everything trove knows about an agent is in one implementation of the `Agent` trait
(`src/agents/mod.rs`): where its chats are, how to read them, and how to resume one. To support
another agent, add a module next to the others in `src/agents/`, implement the trait, and list
it in `AGENTS`. Discovery, the index, search and the picker pick it up from there.

`Agent` says where the chats are and how to resume one; its `format` says how to read them.
Most agents keep each chat in an append-only file of JSON lines: implement `Lines`, with `line`
to read one line and `header` for what the file says about the whole chat, like a header line.
An agent that keeps all its chats in a database implements `Database` instead, with `versions`
and `read` (see `opencode.rs`).

## License

MIT
