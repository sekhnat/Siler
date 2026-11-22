//! `siler-pac`: answers "which proxy for this address?" from a proxy script (PAC).
//!
//! Scripts found on the network are untrusted, so they run here and not inside Siler: one
//! question per process, in QuickJS with nothing but the standard PAC functions, under
//! limits on memory, time and files.
//!
//! Standard input is `{"script", "url", "host"}`; standard output is `{"proxy"}` or
//! `{"error"}`.

use rquickjs::{CatchResultExt, Context, Function, Runtime, Value};
use rustix::process::{Resource, Rlimit, setrlimit};
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const MAX_INPUT: u64 = 4 * 1024 * 1024;
const MAX_ANSWER: usize = 4096;
const JS_MEMORY: usize = 32 * 1024 * 1024;
const JS_STACK: usize = 512 * 1024;
/// For the whole question, name lookups included.
const TIME: Duration = Duration::from_secs(4);
/// The kernel's limits, for what the interpreter doesn't catch (a long native loop).
const CPU_SECONDS: u64 = 6;
const ADDRESS_SPACE: u64 = 512 * 1024 * 1024;
const OPEN_FILES: u64 = 64;

const STANDARD: &str = include_str!("standard.js");

#[derive(serde::Deserialize)]
struct Question {
    script: String,
    url: String,
    host: String,
}

fn confine() -> Result<(), String> {
    let limit = |resource, soft: u64, hard: u64| {
        setrlimit(
            resource,
            Rlimit {
                current: Some(soft),
                maximum: Some(hard),
            },
        )
        .map_err(|e| format!("can't set the limits: {e}"))
    };
    limit(Resource::Cpu, CPU_SECONDS, CPU_SECONDS + 1)?;
    limit(Resource::As, ADDRESS_SPACE, ADDRESS_SPACE)?;
    limit(Resource::Nofile, OPEN_FILES, OPEN_FILES)?;
    limit(Resource::Fsize, 0, 0)?;
    limit(Resource::Core, 0, 0)?;
    rustix::thread::set_no_new_privs(true).map_err(|e| format!("can't drop privileges: {e}"))
}

/// The first IPv4 address of a name; the PAC format predates IPv6.
fn resolve(host: String) -> String {
    if host.is_empty() || host.len() > 255 {
        return String::new();
    }
    (host.as_str(), 0)
        .to_socket_addrs()
        .ok()
        .and_then(|mut found| found.find(|a| a.is_ipv4()))
        .map(|a| a.ip().to_string())
        .unwrap_or_default()
}

/// The address this computer reaches the internet from; connecting UDP sends nothing.
fn my_address() -> String {
    let found = || -> Option<IpAddr> {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("198.41.0.4:53").ok()?;
        Some(socket.local_addr().ok()?.ip())
    };
    found().map_or_else(|| "127.0.0.1".to_string(), |ip| ip.to_string())
}

fn answer(q: &Question) -> Result<String, String> {
    let internal = |e: rquickjs::Error| format!("the script engine failed: {e}");
    let runtime = Runtime::new().map_err(internal)?;
    runtime.set_memory_limit(JS_MEMORY);
    runtime.set_max_stack_size(JS_STACK);
    let until = Instant::now() + TIME;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= until)));
    let context = Context::full(&runtime).map_err(internal)?;
    context.with(|ctx| {
        let failed = |e: rquickjs::CaughtError<'_>| {
            if Instant::now() >= until {
                "the proxy script took too long".to_string()
            } else {
                format!("the proxy script failed: {e}")
            }
        };
        let globals = ctx.globals();
        for (name, f) in [
            ("__resolve", Function::new(ctx.clone(), resolve)),
            ("__myAddress", Function::new(ctx.clone(), my_address)),
        ] {
            globals.set(name, f.map_err(internal)?).map_err(internal)?;
        }
        ctx.eval::<(), _>(STANDARD).catch(&ctx).map_err(failed)?;
        // Scripts are written for sloppy mode.
        let mut sloppy = rquickjs::context::EvalOptions::default();
        sloppy.strict = false;
        ctx.eval_with_options::<Value, _>(q.script.as_str(), sloppy)
            .catch(&ctx)
            .map_err(failed)?;
        let find: Function = globals
            .get("FindProxyForURL")
            .map_err(|_| "the proxy script has no FindProxyForURL".to_string())?;
        let result: Value = find
            .call((q.url.as_str(), q.host.as_str()))
            .catch(&ctx)
            .map_err(failed)?;
        let text = match result.as_string() {
            Some(s) => s.to_string().map_err(internal)?,
            None if result.is_null() || result.is_undefined() => String::new(),
            None => return Err("the proxy script didn't answer with text".to_string()),
        };
        if text.len() > MAX_ANSWER {
            return Err("the proxy script's answer is too long".to_string());
        }
        Ok(text)
    })
}

fn main() {
    let result = confine().and_then(|()| {
        let mut input = Vec::new();
        std::io::stdin()
            .take(MAX_INPUT)
            .read_to_end(&mut input)
            .map_err(|e| format!("can't read the question: {e}"))?;
        let question: Question =
            serde_json::from_slice(&input).map_err(|e| format!("can't read the question: {e}"))?;
        answer(&question)
    });
    let out = match result {
        Ok(proxy) => serde_json::json!({ "proxy": proxy }),
        Err(error) => serde_json::json!({ "error": error.chars().take(500).collect::<String>() }),
    };
    if writeln!(std::io::stdout(), "{out}").is_err() {
        std::process::exit(1);
    }
}
