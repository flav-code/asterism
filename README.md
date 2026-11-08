<p align="center"><img src="crates/app/assets/icon-512.png" width="128" alt=""></p>

# Asterism

See every AI coding agent session on your machine at once: how they talk to each other, what each one is doing,
what it costs in tokens, and what it runs in RAM and CPU.

When a dozen agent sessions run at once, the machine slows down and the OS can't tell you why: Activity Monitor
shows one big "VM Helper" and twenty identical `claude` processes. Asterism puts every process and container
back under the session that started it, and draws the sessions as a map: who started whom, who messaged whom.

![The map: sessions as bubbles, lines for who started or messaged whom](docs/map.png)

![The timeline: one row per session over the last 24 hours](docs/timeline.png)

The screenshots come from demo mode: real sessions, with every title, project and message replaced by made-up ones.

**Status:** early. Works on macOS; Windows builds but is not tested yet.

## Install

**macOS** (13 or later), with [Homebrew](https://brew.sh):

```sh
brew trust --cask flav-code/tap/asterism
brew install --cask flav-code/tap/asterism
```

**Windows**, with [Scoop](https://scoop.sh):

```powershell
scoop bucket add flav-code https://github.com/flav-code/scoop-bucket
scoop install flav-code/asterism
```

Or download it from [Releases](https://github.com/flav-code/asterism/releases): a `.dmg` for Apple silicon or Intel
Macs, an installer for Windows. Asterism updates itself: when a newer version is out, a green arrow shows at the top
right; click it to restart on the new version. Installed with Scoop, it leaves updates to `scoop update`.

The builds are not signed yet. On macOS, the first time you open it, macOS blocks it: go to System Settings → Privacy
& Security and click Open Anyway. On Windows, SmartScreen asks once: More info → Run anyway.

## Try it

```sh
cargo run --release -p asterism            # the app
cargo run -p asterism-cli -- top           # a summary in the terminal
cargo run -p asterism-cli -- top --json    # everything, as JSON
scripts/package-macos.sh                   # Asterism.app and a .dmg in dist/
```

To save the window as a PNG without screen-recording permission (GPUI renders the frame itself):

```sh
MDB_SNAPSHOT=window.png MDB_TAB=map cargo run --release -p asterism --features snapshot
```

It shows:

- **The machine:** CPU, memory used (as Activity Monitor counts it), compressed memory, swap and its traffic,
  memory pressure.
- **Who uses it:** each agent session with everything it started (containers included), the Docker VM, each app,
  the system.
- **Each session:** its processes, its builds, its containers, and its current command (the last tool call without
  a result, linked to the process running it).
- **Docker:** containers grouped by Compose project and local cluster, flagged when they have no CPU or memory
  limit, and attached to a session when its `docker` command names them or its folder holds their mounts.

## How it finds things

- **Sessions** come from the files Claude Code keeps: the live-session registry (`~/.claude/sessions`), the desktop
  app's session files, and the end of each transcript. These formats are internal and undocumented: they are only
  read, unknown fields are ignored and missing files are fine.
- **Processes** are matched to a session by the registry's process id, else by `--resume <id>`, else by folder and
  start time. Everything under a session's process counts for it; a process that detached from it (its parent is
  now `launchd` or `init`) is matched by its folder.
- **Memory** on macOS is the physical footprint (Activity Monitor's "Memory" column), not the resident size.
- **Docker** is read through the engine's API, on the socket of the current Docker context. A container goes to the
  one session whose `docker` command names it, else to the session whose folder most specifically holds its mounts
  or its Compose folder. When several sessions tie, it stays unattached.

## Privacy

Everything stays on the machine. The one request that leaves it: now and then, the installed app asks GitHub for
the latest Asterism release, to offer an update (`ASTERISM_NO_UPDATES=1` turns that off). It never reads process environments, masks anything in a command line that looks
like a secret (tokens, keys, passwords, `Authorization` headers, credentials in URLs), shows your home folder as
`~`, and changes nothing.

## Layout

- `crates/core`: the detection, with no UI dependency. Tests run on recorded data (`tests/fixtures`).
- `crates/cli`: the `asterism-cli` command.
- `crates/app`: the app (tabs Map, Timeline, Usage, Machine), built with [GPUI](https://www.gpui.rs) and [gpui-component](https://github.com/longbridge/gpui-kit).
  Sampling runs on its own thread; the window only reads the latest report.

## License

MIT, see [LICENSE](LICENSE). Asterism is not affiliated with Anthropic; it only reads the files Claude Code keeps on
your machine.

