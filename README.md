# my-dev-business (working name)

See what each AI coding agent session runs on your machine, and what it costs in RAM and CPU.

When a dozen agent sessions run at once, the machine slows down and the OS can't tell you why: Activity Monitor
shows one big "VM Helper" and twenty identical `claude` processes. This tool puts every process and container
back under the session that started it: its commands, builds, test runners, Docker containers and local
Kubernetes clusters, and what the session is doing right now.

**Status:** early. The detection core, a command line and a first desktop window (GPUI) work on macOS.
Windows comes next.

## Try it

```sh
cargo run -p mdb-app             # the window, updated every 2 s
cargo run -p mdb -- top          # a summary in the terminal
cargo run -p mdb -- top --json   # everything, as JSON
```

To save the window as a PNG without screen-recording permission (GPUI renders the frame itself):

```sh
MDB_SNAPSHOT=window.png cargo run -p mdb-app --features snapshot
```

`MDB_SNAPSHOT_OPEN=1` opens the biggest session first.

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

Everything stays on the machine. It never reads process environments, masks anything in a command line that looks
like a secret (tokens, keys, passwords, `Authorization` headers, credentials in URLs), shows your home folder as
`~`, and changes nothing.

## Layout

- `crates/core`: the detection, with no UI dependency. Tests run on recorded data (`tests/fixtures`).
- `crates/cli`: the `mdb` command.
- `crates/app`: the window, built with [GPUI](https://www.gpui.rs) and [gpui-component](https://github.com/longbridge/gpui-kit).
  Sampling runs on its own thread; the window only reads the latest report.
