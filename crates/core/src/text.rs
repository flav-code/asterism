//! Command lines: what a process is, what it runs, and how to show it without leaking secrets.
use regex::Regex;
use std::sync::LazyLock;

/// Commands shown as builds: compilers, bundlers, test runners, image builds. A whole word anywhere in the
/// command line counts ("…/.bin/vitest run" does, "/src/vitest-demo" doesn't). Edit freely.
pub const HEAVY: &[&str] = &[
    "cargo build",
    "cargo test",
    "cargo check",
    "cargo clippy",
    "cargo nextest",
    "rustc",
    "go build",
    "go test",
    "gcc",
    "g++",
    "clang",
    "clang++",
    "cc1",
    "cc1plus",
    "swiftc",
    "swift build",
    "xcodebuild",
    "javac",
    "gradle",
    "gradlew",
    "mvn",
    "dotnet build",
    "dotnet test",
    "bazel",
    "make",
    "ninja",
    "tsc",
    "vue-tsc",
    "turbo",
    "nx",
    "webpack",
    "rollup",
    "rspack",
    "vite build",
    "next build",
    "nuxt build",
    "nuxi build",
    "jest",
    "vitest",
    "mocha",
    "pytest",
    "playwright test",
    "cypress run",
    "docker build",
    "buildx build",
    "compose build",
    "podman build",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

// Rust's regex has no lookaround: the word boundaries are matched (and consumed) instead.
static HEAVY_RE: LazyLock<Regex> = LazyLock::new(|| {
    let words = HEAVY.iter().map(|w| regex::escape(w)).collect::<Vec<_>>().join("|");
    re(&format!(r"(?:^|[^\w.-])(?:{words})(?:$|[^\w.+-])"))
});
static SHELL_C: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:\S*/)?(?:ba|z|da|k|fi)?sh(?: -\w+)* -\w*c "));
static NODE_CLAUDE: LazyLock<Regex> = LazyLock::new(|| re(r"^\S*node\S* \S*@anthropic-ai/claude-code/cli\.m?js"));
static RESUME: LazyLock<Regex> = LazyLock::new(|| re(r"(?:--resume|--session-id|-r)[= ]([0-9a-f]{8}-[0-9a-f-]{27})"));
static EVAL: LazyLock<Regex> = LazyLock::new(|| re(r#"\beval '((?:[^']|'"'"')*)'(?: < /dev/null)?(?: && pwd -P >\| \S+)?$"#));
static ADD_DIR: LazyLock<Regex> = LazyLock::new(|| re(r"--add-dir[= ]"));
static NEXT_OPTION: LazyLock<Regex> = LazyLock::new(|| re(r" --?[a-zA-Z]"));
static DOCKER_CLI: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:\S*[/\\])?docker(?:-compose)?(?:\.exe)?(?: |$)"));

/// Whether a command line holds one of the `HEAVY` commands as a whole word.
pub fn is_heavy(cmd: &str) -> bool {
    HEAVY_RE.is_match(cmd)
}

/// A shell's command line holds the whole script it runs: judge what it starts, not the shell itself.
pub fn is_build(cmd: &str) -> bool {
    !SHELL_C.is_match(cmd) && is_heavy(cmd)
}

/// The program is a path that may hold spaces (macOS apps), so it is "everything before the first option".
pub fn is_claude(cmd: &str) -> bool {
    let program = cmd.split(" -").next().unwrap_or_default();
    let name = program.rsplit(['/', '\\']).next().unwrap_or_default();
    let is_path = program == name || program.starts_with('/') || program.get(1..3) == Some(":\\");
    (is_path && (name == "claude" || name == "claude.exe")) || NODE_CLAUDE.is_match(cmd)
}

/// Whether a command line runs the `docker` or `docker-compose` program.
pub fn is_docker_cli(cmd: &str) -> bool {
    DOCKER_CLI.is_match(cmd)
}

static BUILD_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\sbuild(?:\s|$)"));

/// A build to show: a docker client only when it builds an image, since what it runs shows up as its container.
pub fn is_shown_build(cmd: &str) -> bool {
    is_build(cmd) && (!is_docker_cli(cmd) || BUILD_WORD.is_match(cmd))
}

/// The session id a resumed `claude` process carries.
pub fn resume_of(cmd: &str) -> Option<&str> {
    RESUME.captures(cmd).and_then(|c| c.get(1)).map(|m| m.as_str())
}

/// Claude Code runs each Bash call as `zsh -c "source <snapshot> && … && eval '<command>' < /dev/null && pwd -P >| <file>"`:
/// show the command.
pub fn readable(cmd: &str) -> String {
    if cmd.contains("/shell-snapshots/")
        && let Some(c) = EVAL.captures(cmd)
    {
        return format!("$ {}", c[1].replace(r#"'"'"'"#, "'"));
    }
    cmd.to_string()
}

/// A command as people read it: the program by its name, not its full path ("/opt/x/bin/uv tool run" → "uv tool run").
/// The program is everything before the first option, since app paths hold spaces.
#[must_use]
pub fn short_program(cmd: &str) -> String {
    if !(cmd.starts_with('/') || cmd.starts_with('~') || cmd.get(1..3) == Some(":\\")) {
        return cmd.to_string();
    }
    let program = cmd.split(" -").next().unwrap_or(cmd);
    let name_at = program.rfind(['/', '\\']).map_or(0, |i| i + 1);
    cmd[name_at..].to_string()
}

/// The folders given with `--add-dir`.
// shortcut: ps-style command lines join argv with spaces, so a folder whose name holds " -" is cut there.
pub fn add_dirs(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = cmd;
    while let Some(m) = ADD_DIR.find(rest) {
        let after = &rest[m.end()..];
        let end = NEXT_OPTION.find(after).map_or(after.len(), |n| n.start());
        let dir = after[..end].trim();
        if !dir.is_empty() {
            out.push(dir.to_string());
        }
        rest = &after[end..];
    }
    out
}

const SECRET: &str = "token|secret|passw(?:or)?d|pwd|key|credential|auth|cookie";
static HEADER: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)\b((?:proxy-)?authorization|(?:set-)?cookie)(\s*:\s*)[^"'\n]+"#));
static SCHEME: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(Bearer|Basic|Bot|Token)\s+[\w.~+/=-]{12,}"));
static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b([a-z][\w+.-]*://)[^\s/@]+@"));
static FLAG: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r#"(?i)(^|\s)(--?[\w-]*(?:{SECRET})[\w-]*)(=|\s+)("[^"]*"|'[^']*'|[^\s-]\S*)"#)));
static PAIR: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r#"(?i)([\w.-]*(?:{SECRET})[\w.-]*["']?)([=:]\s*)("[^"]*"|'[^']*'|[^\s"',;&]+)"#)));
static KEY_SHAPES: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\b(?:sk-[\w-]{20,}|gh[pousr]_\w{20,}|github_pat_\w{20,}|glpat-[\w-]{20,}|xox[abprs]-[\w-]{10,}|AKIA[0-9A-Z]{16}|AIza[\w-]{30,}|[\w-]{20,}\.[\w-]{6,}\.[\w-]{20,})",
    )
});

/// Never shown as is: what follows a secret-looking word, auth headers, credentials in URLs, well-known key shapes.
/// The home folder becomes `~`, and the line is cut to `max` characters.
pub fn redact(cmd: &str, home: &str, max: usize) -> String {
    let s = HEADER.replace_all(cmd, "${1}${2}***");
    let s = SCHEME.replace_all(&s, "${1} ***");
    let s = URL_CREDENTIALS.replace_all(&s, "${1}***@");
    let s = FLAG.replace_all(&s, "${1}${2}${3}***");
    let s = PAIR.replace_all(&s, "${1}${2}***");
    let s = KEY_SHAPES.replace_all(&s, "***");
    let s = if home.len() > 1 { s.replace(home, "~") } else { s.into_owned() };
    if s.chars().count() > max { s.chars().take(max - 1).chain(['…']).collect() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_processes() {
        let cases = [
            ("/Applications/Some App.app/Contents/Helpers/disclaimer --pgroup -- /opt/x/claude --verbose", false),
            (
                "/Users/me/Library/Application Support/App-3/claude.app/Contents/MacOS/claude --verbose --resume=0404acf9-4d7f-436d-99f9-17995b3e8077",
                true,
            ),
            ("claude", true),
            ("node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js -r x", true),
            ("vim /tmp/claude", false),
            ("claude update", false),
            (r"C:\Users\me\.local\bin\claude.exe --resume x", true),
        ];
        for (cmd, want) in cases {
            assert_eq!(is_claude(cmd), want, "{cmd}");
        }
        assert_eq!(
            resume_of("claude --verbose --resume=0404acf9-4d7f-436d-99f9-17995b3e8077 --x"),
            Some("0404acf9-4d7f-436d-99f9-17995b3e8077")
        );
    }

    #[test]
    fn builds() {
        let heavy = [
            ("node /app/node_modules/.bin/vitest run", true),
            ("sh -c pnpm exec turbo run build --filter=api", true),
            ("docker buildx build -t x .", true),
            ("cc1plus -O2 x.cpp", true),
            ("nuxt dev --port 3000", false),
            ("/src/vitest-demo/run.sh", false),
            ("cargo run -p server", false),
        ];
        for (cmd, want) in heavy {
            assert_eq!(is_heavy(cmd), want, "{cmd}");
        }
        // a shell running `npx vitest` is not the build, the vitest under it is
        let builds = [
            ("/bin/zsh -c source ~/.snap.sh && eval 'npx vitest run'", false),
            ("sh -lc 'make -j8'", false),
            ("node /app/node_modules/.bin/vitest run", true),
        ];
        for (cmd, want) in builds {
            assert_eq!(is_build(cmd), want, "{cmd}");
        }
        // what `docker run` runs shows up as its container; an image build has no container to show
        assert!(!is_shown_build("docker run --rm img npx vitest run"));
        assert!(is_shown_build("docker buildx build -t x ."));
        assert!(is_shown_build("docker compose build web"));
    }

    #[test]
    fn folders_and_bash_commands() {
        let cmd = "/x/claude --verbose --resume=0404acf9-4d7f-436d-99f9-17995b3e8077 --add-dir=/Users/me/Library/Application Support/notes --await-initialize";
        assert_eq!(add_dirs(cmd), ["/Users/me/Library/Application Support/notes"]);
        let wrapped = r#"/bin/zsh -c source /home/me/.claude/shell-snapshots/snapshot-zsh-1.sh 2>/dev/null || true && eval 'docker run -e A='"'"'b c'"'"' x' < /dev/null && pwd -P >| /tmp/claude-1-cwd"#;
        assert_eq!(readable(wrapped), "$ docker run -e A='b c' x");
        assert_eq!(short_program("~/Library/Application Support/App/claude.app/Contents/MacOS/claude --verbose"), "claude --verbose");
        assert_eq!(short_program("/opt/homebrew/bin/uv tool uvx --python 3.12"), "uv tool uvx --python 3.12");
        assert_eq!(short_program("/Applications/Some Browser.app/Contents/MacOS/Some Browser"), "Some Browser");
        assert_eq!(short_program("node /src/server.js"), "node /src/server.js");
        assert!(is_docker_cli("/usr/local/bin/docker compose up -d"));
        assert!(!is_docker_cli("/usr/local/bin/dockerd"));
    }

    #[test]
    fn secrets_never_leak() {
        let line = format!(
            r#"curl -H "Authorization: Bearer abc.def" --password hunter2 --api-key=k1 API_TOKEN=t1 https://u:p@host/x {{"secret": "s1"}} ghp_{} --verbose"#,
            "a".repeat(36)
        );
        let out = redact(&line, "/Users/me", 300);
        for leak in ["abc.def", "hunter2", "k1", "t1", "u:p", "s1", "ghp_"] {
            assert!(!out.contains(leak), "{leak} leaks: {out}");
        }
        assert!(out.contains("--verbose") && out.contains("@host/x"), "{out}");
        let bot = format!("M{}.{}.{}", "a".repeat(25), "b".repeat(6), "c".repeat(30)); // shaped like a bot token, not one
        for (cmd, leak) in [
            (format!(r#"curl -H "Authorization: Bot {bot}" https://x/api"#), "aaaa"),
            ("curl -H 'Cookie: sid=s3cr3t; theme=dark' x".to_string(), "s3cr3t"),
            (format!("node bot.js {bot}"), "cccc"),
            ("psql postgres://app:pw@db/x".to_string(), "pw@"),
        ] {
            let out = redact(&cmd, "/Users/me", 300);
            assert!(!out.contains(leak), "{out}");
        }
        assert_eq!(redact(&"x".repeat(500), "/Users/me", 300).chars().count(), 300);
        assert_eq!(redact("cat /Users/me/notes.txt", "/Users/me", 300), "cat ~/notes.txt");
    }
}
