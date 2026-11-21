//! Running the user's command after a download finishes.
//!
//! The command line is split into words the way a shell would (quotes, backslashes), but no
//! shell ever runs it: placeholders are filled in inside a word, so a file name with spaces,
//! quotes or `;` stays one argument and can't run anything else. Someone who wants a shell
//! writes `sh -c '…' _ {path}` themselves.

use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use crate::extras::CommandResult;

/// How much of the command's output is kept.
pub const OUTPUT_TAIL: usize = 4096;

/// Splits a command line into words: whitespace separates them, `'…'` keeps everything,
/// `"…"` keeps everything but `\"` and `\\`, and a backslash outside quotes keeps the next
/// character.
pub fn split(cmd: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return Err("A ' quote isn't closed.".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(x @ ('"' | '\\')) => cur.push(x),
                            Some(x) => {
                                cur.push('\\');
                                cur.push(x);
                            }
                            None => return Err("A \" quote isn't closed.".into()),
                        },
                        Some(x) => cur.push(x),
                        None => return Err("A \" quote isn't closed.".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(x) => cur.push(x),
                    None => return Err("The command ends with a lone backslash.".into()),
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// What the placeholders stand for.
pub struct Vars<'a> {
    pub path: &'a Path,
    pub url: &'a str,
    pub size: Option<u64>,
}

/// Fills in `{path}`, `{dir}`, `{name}`, `{url}` and `{size}` in each word.
pub fn fill(words: &[String], v: &Vars) -> Vec<String> {
    let path = v.path.to_string_lossy();
    let dir = v
        .path
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = v
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let size = v.size.map(|s| s.to_string()).unwrap_or_default();
    words
        .iter()
        .map(|w| {
            // One pass, so a file name containing "{url}" isn't filled in again.
            let mut out = String::with_capacity(w.len());
            let mut rest = w.as_str();
            while let Some(i) = rest.find('{') {
                out.push_str(&rest[..i]);
                let tail = &rest[i..];
                let (value, len) = [
                    ("{path}", path.as_ref()),
                    ("{dir}", dir.as_str()),
                    ("{name}", name.as_str()),
                    ("{url}", v.url),
                    ("{size}", size.as_str()),
                ]
                .into_iter()
                .find(|(k, _)| tail.starts_with(k))
                .map_or(("{", 1), |(k, val)| (val, k.len()));
                out.push_str(value);
                rest = &tail[len..];
            }
            out.push_str(rest);
            out
        })
        .collect()
}

/// Checks a command line before it is saved: it must name a program.
pub fn check(cmd: &str) -> Result<(), String> {
    match split(cmd)?.first() {
        Some(p) if !p.is_empty() => Ok(()),
        _ => Err("The command is empty.".into()),
    }
}

/// Keeps the last `OUTPUT_TAIL` bytes of a stream.
async fn tail(mut r: impl AsyncRead + Unpin) -> Vec<u8> {
    let mut keep: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                keep.extend_from_slice(&buf[..n]);
                if keep.len() > OUTPUT_TAIL * 2 {
                    keep.drain(..keep.len() - OUTPUT_TAIL);
                }
            }
        }
    }
    if keep.len() > OUTPUT_TAIL {
        keep.drain(..keep.len() - OUTPUT_TAIL);
    }
    keep
}

/// Runs `words` (a program and its arguments) to the end, or until `cancel`.
pub async fn run(words: Vec<String>, cancel: &CancellationToken) -> CommandResult {
    let started_at = crate::manager::now_ms();
    let done = |exit_code, output: String, error: Option<String>| CommandResult {
        running: false,
        exit_code,
        output,
        error,
        started_at,
        finished_at: Some(crate::manager::now_ms()),
    };
    let Some((program, args)) = words.split_first() else {
        return done(None, String::new(), Some("The command is empty.".into()));
    };
    let mut std_cmd = crate::host::command(program);
    std_cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let why = if e.kind() == std::io::ErrorKind::NotFound {
                format!("“{program}” wasn't found")
            } else {
                format!("“{program}” couldn't start: {e}")
            };
            return done(None, String::new(), Some(why));
        }
    };
    let out = tokio::spawn(tail(child.stdout.take().expect("piped")));
    let err = tokio::spawn(tail(child.stderr.take().expect("piped")));
    let status = tokio::select! {
        s = child.wait() => s,
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return done(None, String::new(), Some("Stopped because K-Star quit.".into()));
        }
    };
    let mut output = out.await.unwrap_or_default();
    let errput = err.await.unwrap_or_default();
    if !errput.is_empty() {
        if !output.is_empty() && !output.ends_with(b"\n") {
            output.push(b'\n');
        }
        output.extend_from_slice(&errput);
    }
    if output.len() > OUTPUT_TAIL {
        output.drain(..output.len() - OUTPUT_TAIL);
    }
    let output = String::from_utf8_lossy(&output).trim_end().to_string();
    match status {
        Ok(s) => done(
            s.code(),
            output,
            s.code()
                .is_none()
                .then(|| "The command was ended by a signal.".into()),
        ),
        Err(e) => done(None, output, Some(format!("The command failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(split("a b  c").unwrap(), ["a", "b", "c"]);
        assert_eq!(split("a 'b c' \"d e\"").unwrap(), ["a", "b c", "d e"]);
        assert_eq!(
            split(r#"echo "say \"hi\"" it\'s"#).unwrap(),
            ["echo", "say \"hi\"", "it's"]
        );
        assert_eq!(split("x ''").unwrap(), ["x", ""]);
        assert_eq!(split("  ").unwrap(), Vec::<String>::new());
        assert!(split("a 'b").is_err());
        assert!(split("a \"b").is_err());
        assert!(check("").is_err());
        assert!(check("clamscan {path}").is_ok());
    }

    #[test]
    fn placeholders_stay_inside_one_argument() {
        let p = PathBuf::from("/dl/a b; rm -rf ~ {url}.txt");
        let v = Vars {
            path: &p,
            url: "https://x/{path}",
            size: Some(42),
        };
        let words = split("tool --in={path} {dir} {name} {url} {size} {other}").unwrap();
        assert_eq!(
            fill(&words, &v),
            [
                "tool",
                "--in=/dl/a b; rm -rf ~ {url}.txt",
                "/dl",
                "a b; rm -rf ~ {url}.txt",
                "https://x/{path}",
                "42",
                "{other}"
            ]
        );
    }

    #[tokio::test]
    async fn runs_and_reports() {
        let cancel = CancellationToken::new();
        let r = run(
            split("sh -c 'echo out; echo err >&2; exit 3'").unwrap(),
            &cancel,
        )
        .await;
        assert_eq!(r.exit_code, Some(3));
        assert_eq!(r.output, "out\nerr");
        assert!(r.error.is_none());
        assert!(!r.ok());
        let ok = run(vec!["true".into()], &cancel).await;
        assert!(ok.ok());
        let missing = run(vec!["kstar-no-such-program".into()], &cancel).await;
        assert!(missing.error.unwrap().contains("wasn't found"));
        let long = run(
            split("sh -c 'head -c 20000 /dev/zero | tr \"\\0\" x'").unwrap(),
            &cancel,
        )
        .await;
        assert_eq!(long.output.len(), OUTPUT_TAIL);
    }
}
