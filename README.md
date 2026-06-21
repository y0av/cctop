# cctop

A [btop](https://github.com/aristocratos/btop)-style terminal monitor for your Claude usage and running Claude Code agents.

![cctop](assets/screenshot.png)

Instead of keeping the **claude.ai usage tab** open in your browser, run `cctop` and watch your plan limits, token spend, and every running Claude Code agent right in your terminal — live.

## Install

**Quick install** — prebuilt binary, no Rust needed.

Linux / macOS (Intel & Apple Silicon):

```sh
curl -fsSL https://raw.githubusercontent.com/y0av/cctop/master/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/y0av/cctop/master/install.ps1 | iex
```

The rest of the options need a [Rust toolchain](https://rustup.rs) (`cargo`). No system libraries — TLS is bundled (rustls).

**From git:**

```sh
cargo install --git https://github.com/y0av/cctop
```

**From source:**

```sh
git clone https://github.com/y0av/cctop
cd cctop
cargo install --path .
```

**Just build, don't install:**

```sh
git clone https://github.com/y0av/cctop && cd cctop
cargo build --release
./target/release/cctop
```

Either install puts `cctop` on your `PATH` (`~/.cargo/bin`). Then just run:

```sh
cctop
```

## What it shows

- **Plan** — your live 5-hour and weekly limits (the same gauges as *claude.ai/settings/usage*) with reset timers. Falls back to a local estimate when offline.
- **Live agents** — every running Claude Code session as a process row: project, model, busy/idle, uptime, memory, and a live token-burn sparkline.
- **Usage** — today's tokens and cost, main-vs-subagent split, and lifetime breakdowns by model and project.

Token data is read locally from `~/.claude` (and any extra dirs you point it at — see [Multiple config dirs](#multiple-config-dirs)); the live plan gauges reuse your existing Claude Code OAuth login (Pro/Max). Nothing leaves your machine except the same usage request the CLI already makes.

Works on Linux, macOS and Windows.

## Keys

`q` quit · `↑ ↓` select · `s` cycle sort · `r` refresh

## Flags

| flag | what it does |
|------|--------------|
| `--demo` | run with synthetic data — no account needed |
| `--no-net` | local data only, never touch the network |
| `--once` | print a one-shot text snapshot and exit |
| `--config-dir DIR` | also read another Claude config dir (repeatable) — see [Multiple config dirs](#multiple-config-dirs) |

## Multiple config dirs

By default cctop reads the same config dir Claude Code itself uses: `$CLAUDE_CONFIG_DIR` if set, otherwise `~/.claude`.

If you run Claude Code under more than one config dir — a sandbox launched with a custom `CLAUDE_CONFIG_DIR`, or a second account — point cctop at the extras to watch **all** of their agents and token usage in one view. Pass the dir that contains `projects/` and `sessions/`:

```sh
# repeatable flag
cctop --config-dir ~/envs/sandbox/claude

# or, persistently, via an env var — a path list (':' on Unix, ';' on Windows)
export CCTOP_CONFIG_DIRS=~/envs/sandbox/claude:~/envs/other/claude
cctop
```

Live agents and token totals are merged across every dir (duplicates collapse, missing dirs are ignored); the header account and live plan gauges follow the base dir. When more than one dir is active the footer shows `src:N`. With nothing extra set, behavior is unchanged — just `~/.claude`.

## License

MIT
