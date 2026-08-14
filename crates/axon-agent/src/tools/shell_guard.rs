//! Refusal rules for the shell tool.
//!
//! ## What this is
//!
//! A guard against the agent *accidentally* running something catastrophic —
//! wiping the filesystem, reformatting a disk, deleting an account. An LLM
//! writes commands in ordinary, readable forms, so matching those forms catches
//! the mistakes that actually happen.
//!
//! ## What this is not
//!
//! A security boundary, and it must not be deployed as one. The shell tool runs
//! arbitrary commands as the Axon process user by design; no string-level check
//! can contain that. `X=rm; $X -rf /`, `echo cm0gLXJmIC8= | base64 -d | sh`, and
//! a one-line Python script all reach the same syscalls without ever spelling
//! the blocked text. Anyone who can invoke this tool already holds the master
//! key. **The process user's own privileges are the real boundary** — run Axon
//! as an unprivileged user, in a container, or both.
//!
//! ## Why it is not a substring scan
//!
//! The original implementation tested `cmd.contains(pattern)` against the raw
//! command line, which was wrong in both directions:
//!
//! * `rm -rf /` was refused but `rm -fr /` sailed through — as did `rm  -rf /`
//!   (two spaces) and `/bin/rm -rf /`. Only one exact spelling was covered.
//! * `cat /etc/passwd`, `grep iptables /var/log/syslog`, and `ls ~/mkfs-notes`
//!   were all refused, because the pattern appeared inside an *argument*.
//!
//! Matching against a parsed program name and its flags fixes both directions at
//! once. The parsing is deliberately shallow — see [`split_commands`].

/// Why a command was refused.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal {
    /// Short rule id, for the operator-facing message and for tests.
    pub rule: &'static str,
    pub detail: String,
}

/// `None` when the command may run.
pub fn check(cmd: &str) -> Option<Refusal> {
    split_commands(cmd)
        .into_iter()
        .filter_map(|seg| parse(seg).and_then(|c| refuse(&c)))
        .next()
}

/// Split a command line on the separators that begin a new command: `;`, `|`,
/// `&`, and newlines (so `&&` and `||` fall out naturally).
///
/// This is not a shell parser and does not pretend to be one — it does not
/// understand quoting around separators, command substitution, or here-docs. It
/// does not need to: per the module docs, the goal is recognising a plainly
/// written destructive command, not defeating deliberate obfuscation.
fn split_commands(cmd: &str) -> Vec<&str> {
    cmd.split(|c| matches!(c, ';' | '|' | '&' | '\n'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// One command reduced to the parts the rules match on.
#[derive(Debug)]
struct Cmd<'a> {
    /// Program name with any directory prefix removed (`/bin/rm` → `rm`).
    program: &'a str,
    /// Clustered short flags, expanded: `-rf` contributes `r` and `f`.
    short_flags: String,
    /// Long flags without the leading dashes.
    long_flags: Vec<&'a str>,
    operands: Vec<&'a str>,
}

/// Wrappers that pass their remaining arguments straight through to another
/// program, so the interesting program name is the *next* token.
const PASSTHROUGH: &[&str] = &[
    "sudo", "doas", "env", "nohup", "command", "builtin", "exec", "time", "nice", "setsid",
    "stdbuf", "ionice",
];

/// Wrapper flags that take their value as a *separate* following token. That
/// value must be skipped too, or it is mistaken for the program name — without
/// this, `sudo -u root rm -rf /` parses as the program `root` and slips through.
const FLAGS_WITH_VALUE: &[&str] = &[
    "-u", "-g", "-p", "-C", "-h", "-U", "-r", "-t", "-T", "-n", "-c", "-S",
];

fn parse(segment: &str) -> Option<Cmd<'_>> {
    let mut tokens = segment.split_whitespace().map(unquote);

    // Skip leading `VAR=value` assignments, passthrough wrappers, and the
    // wrappers' own flags, to reach the program actually being run.
    let program = loop {
        let tok = tokens.next()?;
        if is_assignment(tok) {
            continue;
        }
        if tok.starts_with('-') {
            if FLAGS_WITH_VALUE.contains(&tok) {
                tokens.next();
            }
            continue;
        }
        if PASSTHROUGH.contains(&basename(tok)) {
            continue;
        }
        break basename(tok);
    };

    let mut cmd = Cmd {
        program,
        short_flags: String::new(),
        long_flags: Vec::new(),
        operands: Vec::new(),
    };
    for tok in tokens {
        if let Some(long) = tok.strip_prefix("--") {
            if long.is_empty() {
                continue; // bare `--` ends option parsing
            }
            cmd.long_flags.push(long.split('=').next().unwrap_or(long));
        } else if let Some(short) = tok.strip_prefix('-') {
            if short.is_empty() {
                cmd.operands.push(tok); // a lone `-` is an operand (stdin)
            } else {
                cmd.short_flags.push_str(short);
            }
        } else {
            cmd.operands.push(tok);
        }
    }
    Some(cmd)
}

fn is_assignment(tok: &str) -> bool {
    match tok.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

fn unquote(tok: &str) -> &str {
    tok.trim_matches(|c| c == '"' || c == '\'')
}

impl Cmd<'_> {
    fn has_flag(&self, short: char, long: &str) -> bool {
        self.short_flags.contains(short) || self.long_flags.iter().any(|f| *f == long)
    }
}

/// Does this operand name the filesystem root or the user's whole home?
fn is_catastrophic_target(op: &str) -> bool {
    match op {
        "~" | "$HOME" | "${HOME}" | "/" => true,
        // `/`, `/*`, `//`, `/.`, `/*/` … anything that is only root plus glob.
        _ => {
            op.starts_with('/') && op.chars().all(|c| matches!(c, '/' | '*' | '.'))
                || op.starts_with("~/") && op[2..].chars().all(|c| matches!(c, '/' | '*' | '.'))
        }
    }
}

fn refuse(c: &Cmd) -> Option<Refusal> {
    let recursive = c.has_flag('r', "recursive") || c.has_flag('R', "recursive");

    // Recursive delete aimed at root or $HOME.
    //
    // `-f` is deliberately NOT required. It only suppresses prompts, and the
    // agent's shell has no tty to prompt on — so `rm -r /` destroys just as
    // much as `rm -rf /`. Nothing legitimate ever recursively deletes the
    // filesystem root, so there is no false positive to trade away here.
    if c.program == "rm" && recursive && c.operands.iter().any(|op| is_catastrophic_target(op)) {
        return Some(Refusal {
            rule: "rm-root",
            detail: "recursive delete of the filesystem root or home directory".into(),
        });
    }

    // Filesystem creation wipes whatever device it is pointed at.
    if c.program == "mkfs" || c.program.starts_with("mkfs.") {
        return Some(Refusal {
            rule: "mkfs",
            detail: format!("`{}` reformats a device", c.program),
        });
    }

    // Raw device imaging.
    if c.program == "dd" && c.operands.iter().any(|op| op.starts_with("if=")) {
        return Some(Refusal {
            rule: "dd",
            detail: "raw device imaging with `dd if=`".into(),
        });
    }

    // Recursive permission/ownership rewrites.
    if matches!(c.program, "chmod" | "chown" | "chgrp") && recursive {
        return Some(Refusal {
            rule: "recursive-perms",
            detail: format!("recursive `{}` rewrites permissions in bulk", c.program),
        });
    }

    // Account and firewall management — locks the operator out of their own box.
    if matches!(
        c.program,
        "passwd" | "userdel" | "groupdel" | "iptables" | "ip6tables" | "ufw" | "nft"
    ) {
        return Some(Refusal {
            rule: "system-admin",
            detail: format!("`{}` alters accounts or firewall rules", c.program),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(cmd: &str) -> bool {
        check(cmd).is_some()
    }

    fn rule(cmd: &str) -> &'static str {
        check(cmd).expect("expected a refusal").rule
    }

    /// The spelling the old substring list covered must still be refused.
    #[test]
    fn still_blocks_the_classic_spelling() {
        assert_eq!(rule("rm -rf /"), "rm-root");
        assert_eq!(rule("rm -rf /*"), "rm-root");
    }

    /// Every one of these reached the shell under the substring list: the flags
    /// were reordered, split, spelled out, spaced differently, or the program
    /// was given by absolute path.
    #[test]
    fn blocks_the_variants_substring_matching_missed() {
        for cmd in [
            "rm -fr /",
            "rm  -rf  /",
            "rm -r -f /",
            "rm -f -r /",
            "rm --recursive --force /",
            "/bin/rm -rf /",
            "sudo rm -rf /",
            "rm -rf ~",
            "rm -rf $HOME",
            "echo hi; rm -fr /",
            "true && rm -fr /*",
            // No `-f`: with no tty there is nothing to prompt on, so this is
            // just as destructive.
            "rm -r /",
            "rm --recursive /",
        ] {
            assert!(blocked(cmd), "should have been refused: {cmd}");
        }
    }

    /// The other direction: these are ordinary, harmless commands that the
    /// substring list refused because the pattern appeared in an argument.
    #[test]
    fn allows_commands_that_merely_mention_a_pattern() {
        for cmd in [
            "cat /etc/passwd",
            "grep iptables /var/log/syslog",
            "ls ~/notes/mkfs-writeup.md",
            "echo 'run ufw status to check'",
            "git log --grep=passwd",
            "rg 'chmod -R' src/",
            "cat notes-about-dd-if=disk.txt",
        ] {
            assert!(!blocked(cmd), "should have been allowed: {cmd}");
        }
    }

    /// Deleting a specific directory is the tool working as intended; only the
    /// root/home targets are refused.
    #[test]
    fn allows_targeted_recursive_deletes() {
        for cmd in [
            "rm -rf /tmp/build",
            "rm -rf ./node_modules",
            "rm -rf ~/scratch/old",
            "rm -rf /var/tmp/cache/*",
        ] {
            assert!(!blocked(cmd), "should have been allowed: {cmd}");
        }
    }

    #[test]
    fn blocks_filesystem_and_device_operations() {
        assert_eq!(rule("mkfs.ext4 /dev/sda1"), "mkfs");
        assert_eq!(rule("/sbin/mkfs -t ext4 /dev/sdb"), "mkfs");
        assert_eq!(rule("dd if=/dev/sda of=/dev/sdb"), "dd");
    }

    #[test]
    fn blocks_recursive_permission_changes() {
        assert_eq!(rule("chmod -R 777 /etc"), "recursive-perms");
        assert_eq!(rule("chown -R nobody /srv"), "recursive-perms");
        assert_eq!(rule("chmod --recursive 777 /etc"), "recursive-perms");
        // Non-recursive is fine.
        assert!(!blocked("chmod 644 notes.txt"));
        assert!(!blocked("chown me:me file"));
    }

    #[test]
    fn blocks_account_and_firewall_management() {
        assert_eq!(rule("userdel bob"), "system-admin");
        assert_eq!(rule("sudo ufw disable"), "system-admin");
        assert_eq!(rule("iptables -F"), "system-admin");
    }

    #[test]
    fn env_assignments_and_wrappers_do_not_hide_the_program() {
        assert_eq!(rule("FOO=bar rm -rf /"), "rm-root");
        assert_eq!(rule("env LC_ALL=C rm -fr /"), "rm-root");
        assert_eq!(rule("nohup mkfs.ext4 /dev/sda"), "mkfs");
        assert_eq!(rule("sudo -u root userdel bob"), "system-admin");
        // The flag's value must not be mistaken for the program name.
        assert_eq!(rule("sudo -u root rm -rf /"), "rm-root");
        assert_eq!(rule("nice -n 10 mkfs.ext4 /dev/sda"), "mkfs");
    }

    #[test]
    fn ordinary_commands_are_untouched() {
        for cmd in [
            "ls -la",
            "cargo test --workspace",
            "git status",
            "df -h | head -5",
            "python3 -c 'print(1)'",
            "",
        ] {
            assert!(!blocked(cmd), "should have been allowed: {cmd}");
        }
    }

    /// Documented limitation, asserted so it is a known quantity rather than a
    /// surprise: obfuscation defeats this guard, which is why it is not a
    /// security boundary. If a future change makes one of these blocked, that
    /// is an improvement — update the test.
    #[test]
    fn obfuscation_is_out_of_scope() {
        assert!(!blocked("X=rm; $X -rf /"));
        assert!(!blocked("echo cm0gLXJmIC8= | base64 -d | sh"));
    }
}
