//! Proxy scripts (PAC): getting one, and asking `siler-pac` what it says. Scripts found on
//! the network are untrusted, so none runs inside Siler.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

use super::{Kind, Proxy, Script, Via, wpad};

const MAX_SCRIPT: usize = 1024 * 1024;
const MAX_OUTPUT: u64 = 64 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The helper limits itself; this is for one that hangs all the same.
const HELPER_TIMEOUT: Duration = Duration::from_secs(12);
const KEEP_GIVEN: Duration = Duration::from_secs(30 * 60);
/// Shorter, as the network may change.
const KEEP_FOUND: Duration = Duration::from_secs(5 * 60);
const KEEP_ANSWER: Duration = Duration::from_secs(5 * 60);
/// A failure is repeated this long, not worked out again for every connection.
const KEEP_FAILURE: Duration = Duration::from_secs(5);
const KEEP_NOTHING_FOUND: Duration = Duration::from_secs(60);
const MAX_ANSWERS: usize = 512;

pub(super) struct Loaded {
    pub from: String,
    text: String,
}

/// What a script said about one address, and where that script came from.
#[derive(Clone)]
pub(super) struct Answer {
    pub via: Vec<Via>,
    pub from: String,
}

struct Kept<T> {
    until: Instant,
    value: Result<T, String>,
}

#[derive(Default)]
struct State {
    script: Option<Kept<Arc<Loaded>>>,
    answers: HashMap<String, Kept<Answer>>,
}

/// One script, or the search for one.
pub(super) struct Runner {
    script: Script,
    helper: Arc<RwLock<Option<PathBuf>>>,
    /// Held while working, so that many connections ask once.
    state: tokio::sync::Mutex<State>,
}

/// What a script is shown of an address: no login or fragment, and only the origin of an
/// https one.
fn shown_to_script(url: &Url) -> String {
    let mut url = url.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    if matches!(url.scheme(), "https" | "ftps" | "sftp") {
        url.set_path("/");
        url.set_query(None);
    }
    url.to_string()
}

/// Reads `PROXY a:8080; SOCKS5 b; DIRECT`.
pub(super) fn parse_answer(text: &str) -> Option<Vec<Via>> {
    if text.trim().is_empty() {
        return Some(vec![Via::Direct]);
    }
    let mut out = Vec::new();
    for part in text.split(';') {
        let mut words = part.split_whitespace();
        let Some(what) = words.next() else { continue };
        let kind = match what.to_ascii_uppercase().as_str() {
            "DIRECT" => {
                out.push(Via::Direct);
                continue;
            }
            "PROXY" | "HTTP" => Kind::Http,
            "HTTPS" => Kind::Https,
            // Plain SOCKS has always meant version 4 in proxy scripts.
            "SOCKS" | "SOCKS4" => Kind::Socks4,
            "SOCKS4A" => Kind::Socks4a,
            "SOCKS5" => Kind::Socks5,
            _ => continue,
        };
        if let Some(addr) = words.next().filter(|a| !a.contains("://"))
            // A login inside an answer would be the script's, so it is dropped.
            && let Some((proxy, _)) = Proxy::parse(&format!("{}://{addr}", kind.scheme()))
        {
            out.push(Via::Proxy(proxy));
        }
    }
    (!out.is_empty()).then_some(out)
}

pub(super) async fn fetch(url: &str) -> Result<String, String> {
    use futures_util::StreamExt;
    let failed = |e: reqwest::Error| format!("{url}: {}", crate::error::describe(&e));
    // Not through `Net::send`: there is no route until the script is here.
    let client = reqwest::Client::builder()
        .http1_only()
        .no_proxy()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(failed)?;
    let resp = client.get(url).send().await.map_err(failed)?;
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status().as_u16()));
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.map_err(failed)?);
        if body.len() > MAX_SCRIPT {
            return Err(format!("{url}: larger than a proxy script may be"));
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

async fn read_file(path: &Path) -> Result<String, String> {
    let shown = path.display();
    let failed = |e: std::io::Error| format!("{shown}: {e}");
    let meta = tokio::fs::metadata(path).await.map_err(failed)?;
    if !meta.is_file() || meta.len() > MAX_SCRIPT as u64 {
        return Err(format!("{shown}: not a proxy script's file"));
    }
    let bytes = tokio::fs::read(path).await.map_err(failed)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn load_given(at: &str) -> Result<Loaded, String> {
    let path = if let Ok(url) = Url::parse(at) {
        match url.scheme() {
            "http" | "https" => {
                return Ok(Loaded {
                    text: fetch(at).await?,
                    from: super::without_login(at),
                });
            }
            "file" => url
                .to_file_path()
                .map_err(|()| format!("{at}: not a file on this computer"))?,
            _ => return Err(format!("{at}: not an http, https or file address")),
        }
    } else if let Some(rest) = at.strip_prefix("~/") {
        dirs::home_dir().ok_or("no home folder")?.join(rest)
    } else if at.starts_with('/') {
        PathBuf::from(at)
    } else {
        return Err(format!("“{at}” is neither an address nor a file's path"));
    };
    Ok(Loaded {
        text: read_file(&path).await?,
        from: at.to_string(),
    })
}

fn helper_path(set: &RwLock<Option<PathBuf>>) -> Option<PathBuf> {
    const NAME: &str = "siler-pac";
    if let Some(p) = std::env::var_os("SILER_PAC").filter(|p| !p.is_empty()) {
        return Some(p.into());
    }
    let set = set.read().unwrap().clone();
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(NAME)));
    // A packaged install keeps it out of PATH.
    let packaged = Some(Path::new("/usr/lib/siler").join(NAME));
    [set, beside, packaged]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

#[derive(serde::Deserialize)]
struct Output {
    proxy: Option<String>,
    error: Option<String>,
}

async fn run(helper: &Path, script: &str, url: &Url) -> Result<String, String> {
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let input = serde_json::json!({
        "script": script,
        "url": shown_to_script(url),
        "host": host,
    })
    .to_string();

    let mut command = crate::host::command(helper);
    // Of Siler's environment it needs the time zone, and no secret of it.
    command.env_clear().current_dir("/");
    if let Some(tz) = std::env::var_os("TZ") {
        command.env("TZ", tz);
    }
    let mut child = tokio::process::Command::from(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("siler-pac didn't start: {e}"))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("siler-pac has no pipes".into());
    };
    let work = async {
        stdin.write_all(input.as_bytes()).await?;
        drop(stdin);
        let mut out = Vec::new();
        stdout.take(MAX_OUTPUT).read_to_end(&mut out).await?;
        child.wait().await?;
        Ok::<_, std::io::Error>(out)
    };
    let out = tokio::time::timeout(HELPER_TIMEOUT, work)
        .await
        .map_err(|_| "it took too long".to_string())?
        .map_err(|e| format!("siler-pac failed: {e}"))?;
    let out: Output = serde_json::from_slice(&out)
        .map_err(|_| "it was stopped for using too much time or memory".to_string())?;
    match out {
        Output {
            proxy: Some(proxy), ..
        } => Ok(proxy),
        Output { error, .. } => Err(error.unwrap_or_else(|| "it gave no answer".into())),
    }
}

impl Runner {
    pub fn new(script: Script, helper: Arc<RwLock<Option<PathBuf>>>) -> Runner {
        Runner {
            script,
            helper,
            state: Default::default(),
        }
    }

    /// The routes for `url`, or why the script couldn't say.
    pub async fn ask(&self, url: &Url) -> Result<Answer, String> {
        let detected = self.script == Script::Detect;
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if state.script.as_ref().is_none_or(|s| s.until <= now) {
            let value = match &self.script {
                Script::Given(at) => load_given(at).await,
                Script::Detect => wpad::find().await.map(|(from, text)| Loaded { from, text }),
            };
            let keep = match (&value, detected) {
                (Ok(_), false) => KEEP_GIVEN,
                (Ok(_), true) => KEEP_FOUND,
                (Err(_), false) => KEEP_FAILURE,
                (Err(_), true) => KEEP_NOTHING_FOUND,
            };
            match &value {
                Ok(s) => tracing::info!("proxy script: {}", s.from),
                Err(e) => tracing::info!("no proxy script: {e}"),
            }
            state.answers.clear();
            state.script = Some(Kept {
                until: Instant::now() + keep,
                value: value.map(Arc::new),
            });
        }
        let script = match state.script.as_ref() {
            Some(kept) => kept.value.clone()?,
            None => return Err("no script".into()),
        };

        let asked = shown_to_script(url);
        if let Some(kept) = state.answers.get(&asked).filter(|k| k.until > now) {
            return kept.value.clone();
        }
        let value = match helper_path(&self.helper) {
            None => Err("its helper, siler-pac, is missing".to_string()),
            Some(helper) => run(&helper, &script.text, url).await.and_then(|said| {
                let via = parse_answer(&said)
                    .ok_or_else(|| format!("its answer “{said}” names no proxy"))?;
                Ok(Answer {
                    via,
                    from: script.from.clone(),
                })
            }),
        };
        if let Err(e) = &value {
            tracing::warn!("proxy script {}: {e}", script.from);
        }
        if state.answers.len() >= MAX_ANSWERS {
            state.answers.clear();
        }
        let keep = if value.is_ok() {
            KEEP_ANSWER
        } else {
            KEEP_FAILURE
        };
        state.answers.insert(
            asked,
            Kept {
                until: Instant::now() + keep,
                value: value.clone(),
            },
        );
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        let shown =
            |text| parse_answer(text).map(|v| v.iter().map(Via::to_string).collect::<Vec<_>>());
        assert_eq!(
            shown("PROXY a:8080; HTTPS b;SOCKS c; socks5 [::1]:99 ; DIRECT").unwrap(),
            [
                "http://a:8080",
                "https://b:443",
                "socks4://c:1080",
                "socks5h://[::1]:99",
                "DIRECT"
            ]
        );
        assert_eq!(shown("").unwrap(), ["DIRECT"]);
        assert_eq!(shown("PROXY u:p@a:1").unwrap(), ["http://a:1"]);
        assert_eq!(shown("NONSENSE; PROXY; PROXY http://x"), None);
    }

    #[test]
    fn what_a_script_sees() {
        let shown = |s| shown_to_script(&Url::parse(s).unwrap());
        assert_eq!(shown("https://u:p@h:99/a/b?t=1#x"), "https://h:99/");
        assert_eq!(shown("http://u:p@h/a?t=1#x"), "http://h/a?t=1");
    }
}
