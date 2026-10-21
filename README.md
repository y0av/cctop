# cctop

A [btop](https://github.com/aristocratos/btop)-style terminal monitor for Claude plan limits, token spend and running Claude Code agents.

![cctop](assets/screenshot.png)

## Install

Linux and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/y0av/cctop/master/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/y0av/cctop/master/install.ps1 | iex
```

With Cargo (no system libraries needed):

```sh
cargo install --git https://github.com/y0av/cctop
```

Run `cctop`, or `cctop --demo` to try it without an account.

## Features

- **Plan limits**: live 5-hour, weekly and per-model weekly gauges with reset timers and extra-usage credits, matching claude.ai/settings/usage. A `▲cap` warning appears when the current rate would reach 100% before the reset.
- **Token graph**: input and output throughput across all sessions, with rate, peak, total and cost over a 5m, 15m, 1h or 6h window. Tokens are counted when a turn finishes, so long turns show as bursts.
- **Live agents**: every running Claude Code session with model, state, context fill, current tool call, running subagents, and the CPU, memory and GPU of its whole process tree.
- **Drill-down**: the selected agent's tool call, context and compactions, top processes, listening ports, subagents, git status and session totals.
- **System**: CPU, memory, swap, GPU and pressure stalls, with alerts for the busiest agent, thrashing, orphaned processes and near-full context.
- **Activity**: a 24-hour token chart, daily, weekly and lifetime totals with cost, and breakdowns by model and project.

Token data is read locally. Live plan data reuses your Claude Code login (Pro or Max) and makes the same usage request the CLI already makes; offline, cctop estimates usage from local transcripts. Runs on Linux, macOS and Windows. Pressure stalls, listening ports and orphan tracking are Linux only.

## Keys

| key | action |
|-----|--------|
| `↑` `↓` `j` `k`, mouse | select an agent |
| `enter`, `d` | toggle the drill-down |
| `s` | cycle sort: burn, cpu, mem, uptime |
| `t` | cycle theme |
| `z` | cycle the token graph window |
| `1` to `4` | toggle the system, plan, token and bottom panels |
| `x` | send SIGTERM to the selected agent, after confirmation |
| `r` | refresh plan data |
| `q`, `esc` | quit |

`x` stops only the `claude` process of a live agent. On an orphan row (`†orph`) it stops every process the dead session left behind. It is disabled in `--demo`.

## Themes

![themes](assets/themes.png)

`cyber` (default), `claude`, `matrix`, `dracula`, `nord`, `synthwave` and `mono`. Press `t` to cycle (the choice is saved) or start with `--theme NAME`.

## Options

| flag | description |
|------|-------------|
| `--demo` | run with synthetic data |
| `--once` | print a plain-text snapshot and exit |
| `--theme NAME` | start with a theme |
| `--config-dir DIR` | also read another Claude config dir (repeatable) |
| `--refresh MS` | local refresh interval, default 1000 |
| `--no-net` | use local data only |
| `--no-update-check` | skip the daily release check |

## Multiple config dirs

cctop reads `$CLAUDE_CONFIG_DIR`, or `~/.claude` when it is unset. To include sandboxes or other accounts, add their config dirs (the ones containing `projects/`):

```sh
cctop --config-dir ~/envs/sandbox/claude

# or persistently, separated by ':' (';' on Windows)
export CCTOP_CONFIG_DIRS=~/envs/sandbox/claude:~/envs/work/claude
```

Agents and token totals are merged across dirs, and each dir with its own login gets its own plan section.

![multi-account](assets/multi-account.png)

## Updating

cctop does not update itself. When a newer release exists, the header shows `↑` and the new version. Re-run the installer, or `cargo install --git https://github.com/y0av/cctop --force`.

The check queries GitHub releases at most once a day and downloads nothing. It is skipped with `--no-net`, `--demo`, `--no-update-check` or `CCTOP_NO_UPDATE_CHECK=1`.

## License

MIT
