//! Which skills a tool call *reads*: the basis of inferred activations for
//! agents without a skill tool (Codex).
//!
//! Only real reads count: a known reader program (`cat`, `sed` without
//! `-i`, `head`, `rg`…) with a `…/<name>/SKILL.md` argument or `<` input,
//! or a read-file style tool with such a `path`. Patches, redirect
//! targets, `touch` / `rm` / `mv` / `cp` and every other program do not.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// Programs whose path arguments are files they read.
const READERS: &[&str] = &[
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "bat",
    "batcat",
    "nl",
    "sed",
    "awk",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "view",
    "get-content",
    "gc",
];

/// Prefix words that run the next word as the program.
const WRAPPERS: &[&str] = &["sudo", "command", "env", "nice", "time", "nohup"];

const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "fish", "pwsh", "powershell"];

/// Skill names read by tool `tool` with input `input` (JSON arguments or
/// free text), in order, without duplicates.
pub(crate) fn skills_read(tool: &str, input: &str) -> Vec<String> {
    if tool == "apply_patch" || input.contains("*** Begin Patch") {
        return Vec::new();
    }
    let mut paths = Vec::new();
    match serde_json::from_str::<Value>(input) {
        Ok(Value::Object(args)) => {
            if let Some(command) = args.get("command").or_else(|| args.get("cmd")) {
                match command {
                    Value::Array(argv) => {
                        let argv: Vec<String> = argv
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect();
                        argv_reads(&argv, &mut paths);
                    }
                    Value::String(script) => script_reads(script, &mut paths),
                    _ => {}
                }
            } else if is_read_tool(tool) {
                for key in ["path", "file_path", "filePath", "file"] {
                    if let Some(p) = args.get(key).and_then(Value::as_str) {
                        paths.push(p.to_string());
                    }
                }
            }
        }
        Ok(Value::Array(argv)) => {
            let argv: Vec<String> = argv
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            argv_reads(&argv, &mut paths);
        }
        _ if is_shell_tool(tool) => script_reads(input, &mut paths),
        _ => {}
    }
    let mut names: Vec<String> = Vec::new();
    for path in paths {
        if let Some(name) = skill_of(&path)
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

fn is_read_tool(tool: &str) -> bool {
    let t = tool.to_ascii_lowercase();
    t.contains("read") || t == "view" || t == "open" || t == "cat"
}

fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool,
        "shell" | "local_shell" | "exec_command" | "exec" | "bash" | "container.exec"
    )
}

/// The skill of a path whose last two components are `<name>/SKILL.md`.
fn skill_of(path: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[/\\])([A-Za-z0-9][A-Za-z0-9._:-]*)[/\\]SKILL\.md$").expect("valid regex")
    });
    RE.captures(path.trim()).map(|c| c[1].to_string())
}

/// An argv: `bash -lc <script>` is parsed as a script, anything else as
/// one simple command.
fn argv_reads(argv: &[String], paths: &mut Vec<String>) {
    let Some(first) = argv.first() else {
        return;
    };
    if SHELLS.contains(&program_name(first).as_str()) {
        let mut rest = argv[1..].iter();
        while let Some(arg) = rest.next() {
            let flag = arg.trim_start_matches('-').to_ascii_lowercase();
            if arg.starts_with('-') && (flag.ends_with('c') || flag == "command") {
                if let Some(script) = rest.next() {
                    script_reads(script, paths);
                }
                return;
            }
        }
        return;
    }
    let cmd = Command {
        words: argv.to_vec(),
        inputs: Vec::new(),
    };
    command_reads(&cmd, paths);
}

fn script_reads(script: &str, paths: &mut Vec<String>) {
    for cmd in parse(script) {
        command_reads(&cmd, paths);
    }
}

fn command_reads(cmd: &Command, paths: &mut Vec<String>) {
    let mut words = cmd.words.iter().skip_while(|w| is_assignment(w)).peekable();
    while let Some(w) = words.peek() {
        if WRAPPERS.contains(&program_name(w).as_str()) {
            words.next();
            while words
                .peek()
                .is_some_and(|w| w.starts_with('-') || is_assignment(w))
            {
                words.next();
            }
        } else {
            break;
        }
    }
    let Some(program) = words.next() else {
        return;
    };
    let program = program_name(program);
    if !READERS.contains(&program.as_str()) {
        return;
    }
    let args: Vec<&String> = words.collect();
    paths.extend(reader_files(&program, &args));
    paths.extend(cmd.inputs.iter().cloned());
}

/// How a reader's argv is laid out.
struct Spec {
    /// The first positional is a pattern / script, unless an option gave it.
    pattern_first: bool,
    /// Short options that supply the pattern (they take an operand).
    pattern_short: &'static str,
    /// Long options that supply the pattern (they take an operand).
    pattern_long: &'static [&'static str],
    /// Other short options taking an operand (`-n 5`, `-A3`).
    short_operands: &'static str,
    /// Other long options taking an operand when not written `--x=v`.
    long_operands: &'static [&'static str],
}

fn spec(program: &str) -> Spec {
    const NONE: &[&str] = &[];
    let plain = |short: &'static str, long: &'static [&'static str]| Spec {
        pattern_first: false,
        pattern_short: "",
        pattern_long: NONE,
        short_operands: short,
        long_operands: long,
    };
    match program {
        "grep" | "egrep" | "fgrep" => Spec {
            pattern_first: true,
            pattern_short: "ef",
            pattern_long: &["regexp", "file"],
            short_operands: "ABCmdD",
            long_operands: &[
                "after-context",
                "before-context",
                "context",
                "max-count",
                "include",
                "exclude",
                "exclude-dir",
                "exclude-from",
                "label",
                "devices",
                "directories",
                "binary-files",
            ],
        },
        "rg" => Spec {
            pattern_first: true,
            pattern_short: "ef",
            pattern_long: &["regexp", "file"],
            short_operands: "ABCmgtTEjMrd",
            long_operands: &[
                "after-context",
                "before-context",
                "context",
                "max-count",
                "glob",
                "iglob",
                "type",
                "type-not",
                "type-add",
                "encoding",
                "threads",
                "max-columns",
                "max-depth",
                "max-filesize",
                "replace",
                "sort",
                "sortr",
                "pre",
                "pre-glob",
                "ignore-file",
                "path-separator",
                "colors",
                "context-separator",
                "field-match-separator",
                "field-context-separator",
            ],
        },
        "sed" => Spec {
            pattern_first: true,
            pattern_short: "ef",
            pattern_long: &["expression", "file"],
            short_operands: "l",
            long_operands: &["line-length"],
        },
        "awk" => Spec {
            pattern_first: true,
            pattern_short: "f",
            pattern_long: &["file"],
            short_operands: "vF",
            long_operands: &["assign", "field-separator"],
        },
        "head" | "tail" => plain("ncs", &["lines", "bytes", "pid", "sleep-interval"]),
        "less" | "more" => plain("bhjkoOpPtTxyz", &["log-file", "pattern", "tag", "tabs"]),
        "bat" | "batcat" => plain(
            "lHrm",
            &[
                "language",
                "highlight-line",
                "line-range",
                "map-syntax",
                "theme",
                "style",
                "tabs",
                "wrap",
                "terminal-width",
                "pager",
                "file-name",
                "diff-context",
            ],
        ),
        "nl" => plain("bdfhilnsvw", &[]),
        _ => plain("", NONE),
    }
}

/// The files a reader invocation reads, from its arguments.
fn reader_files(program: &str, args: &[&String]) -> Vec<String> {
    if program == "get-content" || program == "gc" {
        return powershell_files(args);
    }
    if program == "sed"
        && args
            .iter()
            .any(|a| a.starts_with("-i") || *a == "--in-place" || a.starts_with("--in-place="))
    {
        return Vec::new();
    }
    let spec = spec(program);
    let mut pattern_given = false;
    let mut positionals = Vec::new();
    let mut it = args.iter();
    let mut options_done = false;
    while let Some(arg) = it.next() {
        if options_done || !arg.starts_with('-') || arg.as_str() == "-" {
            positionals.push(arg.to_string());
            continue;
        }
        if arg.as_str() == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            let is_pattern = spec.pattern_long.contains(&name);
            pattern_given |= is_pattern;
            if !attached && (is_pattern || spec.long_operands.contains(&name)) {
                it.next();
            }
            continue;
        }
        // A short cluster: `-n`, `-n5`, `-inA3`, `-e PATTERN`.
        let cluster: Vec<char> = arg[1..].chars().collect();
        for (i, c) in cluster.iter().enumerate() {
            let is_pattern = spec.pattern_short.contains(*c);
            if is_pattern || spec.short_operands.contains(*c) {
                pattern_given |= is_pattern;
                if i + 1 == cluster.len() {
                    it.next();
                }
                break;
            }
        }
    }
    if spec.pattern_first && !pattern_given && !positionals.is_empty() {
        positionals.remove(0);
    }
    positionals
}

/// `Get-Content [-Path] <file>… [-TotalCount N] …` (case-insensitive).
fn powershell_files(args: &[&String]) -> Vec<String> {
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let Some(name) = arg.strip_prefix('-') else {
            files.push(arg.to_string());
            continue;
        };
        let name = name.to_ascii_lowercase();
        if matches!(name.as_str(), "path" | "literalpath" | "lp") {
            if let Some(file) = it.next() {
                files.push(file.to_string());
            }
        } else if !matches!(name.as_str(), "raw" | "force" | "wait" | "asbytestream") {
            it.next();
        }
    }
    files
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn program_name(word: &str) -> String {
    word.rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

/// A simple command: its words and the files redirected into it (`<`).
#[derive(Debug, Default, PartialEq)]
struct Command {
    words: Vec<String>,
    inputs: Vec<String>,
}

enum Tok {
    Word(String),
    Sep,
    /// `<`: the next word is read.
    ReadFrom,
    /// `>`, `>>`, `<<`…: the next word is not a read.
    Skip,
}

/// Split a POSIX-ish shell script into simple commands. Good enough for
/// what agents run; unknown constructs just end a command.
fn parse(script: &str) -> Vec<Command> {
    let mut commands = Vec::new();
    let mut current = Command::default();
    let mut pending: Option<bool> = None; // Some(true) = read target, Some(false) = skip
    for tok in tokenize(script) {
        match tok {
            Tok::Word(w) => match pending.take() {
                Some(true) => current.inputs.push(w),
                Some(false) => {}
                None => current.words.push(w),
            },
            Tok::Sep => {
                pending = None;
                if !current.words.is_empty() || !current.inputs.is_empty() {
                    commands.push(std::mem::take(&mut current));
                }
            }
            Tok::ReadFrom => pending = Some(true),
            Tok::Skip => pending = Some(false),
        }
    }
    if !current.words.is_empty() || !current.inputs.is_empty() {
        commands.push(current);
    }
    commands
}

fn tokenize(script: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = script.chars().peekable();
    let flush = |toks: &mut Vec<Tok>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            toks.push(Tok::Word(std::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(&n) = chars.peek()
                                && matches!(n, '"' | '\\' | '$' | '`')
                            {
                                word.push(n);
                                chars.next();
                            } else {
                                word.push('\\');
                            }
                        }
                        other => word.push(other),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next()
                    && n != '\n'
                {
                    word.push(n);
                }
            }
            ' ' | '\t' | '\r' => flush(&mut toks, &mut word, &mut in_word),
            '\n' | ';' | '|' | '&' | '(' | ')' | '`' => {
                flush(&mut toks, &mut word, &mut in_word);
                toks.push(Tok::Sep);
            }
            '>' => {
                // `2>`: the digits are a file descriptor, not a word.
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                flush(&mut toks, &mut word, &mut in_word);
                if matches!(chars.peek(), Some('>' | '|')) {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `>&2`, `2>&1`: duplicates a descriptor.
                    chars.next();
                    while chars
                        .peek()
                        .is_some_and(|d| d.is_ascii_digit() || *d == '-')
                    {
                        chars.next();
                    }
                } else {
                    toks.push(Tok::Skip);
                }
            }
            '<' => {
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                flush(&mut toks, &mut word, &mut in_word);
                if chars.peek() == Some(&'<') {
                    // Here-doc / here-string: the next word is a delimiter.
                    chars.next();
                    if chars.peek() == Some(&'<') {
                        chars.next();
                    }
                    toks.push(Tok::Skip);
                } else {
                    toks.push(Tok::ReadFrom);
                }
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    flush(&mut toks, &mut word, &mut in_word);
    toks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Vec<String> {
        let args = serde_json::json!({ "command": ["bash", "-lc", script] }).to_string();
        skills_read("shell", &args)
    }

    #[test]
    fn reads_count() {
        assert_eq!(shell("cat ~/.codex/skills/pdf/SKILL.md"), ["pdf"]);
        assert_eq!(
            shell("sed -n '1,80p' /s/a/SKILL.md && head -n 5 \"/s/b c/../b/SKILL.md\""),
            ["a", "b"]
        );
        assert_eq!(shell("rg -n foo skills/x/SKILL.md | head"), ["x"]);
        assert_eq!(shell("FOO=1 sudo -E cat < /s/y/SKILL.md"), ["y"]);
        assert_eq!(shell("cat /s/a/SKILL.md /s/a/SKILL.md 2>&1"), ["a"]);
        assert_eq!(
            skills_read("exec_command", r#"{"cmd":"cat /s/z/SKILL.md"}"#),
            ["z"]
        );
        assert_eq!(
            skills_read(
                "local_shell",
                r#"["sed","-n","1,40p","skills/notes/SKILL.md"]"#
            ),
            ["notes"]
        );
        assert_eq!(
            skills_read("read_file", r#"{"path":"/s/r/SKILL.md"}"#),
            ["r"]
        );
        assert_eq!(shell("rg x /s/foo/SKILL.md"), ["foo"]);
        assert_eq!(
            shell("grep -n -A 3 -e '/s/no/SKILL.md' /s/yes/SKILL.md"),
            ["yes"]
        );
        assert_eq!(shell("grep -inA3 name /s/g/SKILL.md"), ["g"]);
        assert_eq!(
            shell("head -n 5 /s/h/SKILL.md; tail -n5 /s/t/SKILL.md"),
            ["h", "t"]
        );
        assert_eq!(shell("sed -e 1,20p -n /s/s/SKILL.md"), ["s"]);
        assert_eq!(shell("awk 'NR<5' /s/k/SKILL.md"), ["k"]);
        assert_eq!(shell("bat -l md /s/b/SKILL.md"), ["b"]);
        assert_eq!(
            shell("Get-Content -TotalCount 5 -Path /s/p/SKILL.md"),
            ["p"]
        );
        assert_eq!(shell("cat -- /s/c/SKILL.md"), ["c"]);
    }

    #[test]
    fn writes_and_other_programs_do_not() {
        for script in [
            "touch /s/a/SKILL.md",
            "rm -f /s/a/SKILL.md",
            "mv /s/a/SKILL.md /tmp/x",
            "cp /tmp/x /s/a/SKILL.md",
            "echo hi > /s/a/SKILL.md",
            "cat /tmp/new >> /s/a/SKILL.md",
            "cat <<EOF > /s/a/SKILL.md\nbody\nEOF",
            "sed -i 's/a/b/' /s/a/SKILL.md",
            "vim /s/a/SKILL.md",
            "echo /s/a/SKILL.md",
            "ls /s/a/SKILL.md",
            "grep '/s/a/SKILL.md' /tmp/log",
            "rg -F /s/a/SKILL.md /tmp",
            "sed 's|/s/a/SKILL.md|x|' /tmp/file",
            "awk '/s/a/SKILL.md' /tmp/file",
            "head -n /s/a/SKILL.md",
            "bat -l /s/a/SKILL.md",
        ] {
            assert!(shell(script).is_empty(), "{script}");
        }
        assert!(
            skills_read(
                "apply_patch",
                "*** Begin Patch\n*** Update File: /s/a/SKILL.md"
            )
            .is_empty()
        );
        assert!(skills_read("write_file", r#"{"path":"/s/a/SKILL.md"}"#).is_empty());
        assert!(skills_read("web_search", r#"{"query":"cat /s/a/SKILL.md"}"#).is_empty());
    }
}
