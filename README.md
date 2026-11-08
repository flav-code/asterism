# my-dev-business (working name)

See what each AI coding agent session runs on your machine, and what it costs in RAM and CPU.

When a dozen agent sessions run at once, the machine slows down and the OS can't tell you why: Activity Monitor
shows one big "VM Helper" and twenty identical `claude` processes. This tool puts every process and container
back under the session that started it: its commands, builds, test runners, Docker containers and local
Kubernetes clusters, and what the session is doing right now.

**Status:** early. The detection core and a command line work on macOS. A desktop window (GPUI) comes next,
then Windows.

## Try it

```sh
cargo run -p mdb -- top          # a summary
cargo run -p mdb -- top --json   # everything, as JSON
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

Everything stays on the machine. It never reads process environments, masks anything in a command line that looks
like a secret (tokens, keys, passwords, `Authorization` headers, credentials in URLs), shows your home folder as
`~`, and changes nothing.

## Layout

- `crates/core`: the detection, with no UI dependency. Tests run on recorded data (`tests/fixtures`).
- `crates/cli`: the `mdb` command.
