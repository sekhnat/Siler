//! The helper, run as Siler runs it.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(script: &str, url: &str, host: &str) -> Result<String, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_siler-pac"))
        .env_clear()
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({ "script": script, "url": url, "host": host }).to_string();
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    let out = child.wait_with_output().unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| format!("no answer ({})", out.status))?;
    match (v["proxy"].as_str(), v["error"].as_str()) {
        (Some(p), None) => Ok(p.to_string()),
        (None, Some(e)) => Err(e.to_string()),
        _ => panic!("both or neither: {v}"),
    }
}

/// Whether the expression is true for this address.
fn holds(expr: &str, host: &str) -> bool {
    let script =
        format!("function FindProxyForURL(url, host) {{ return ({expr}) ? 'yes' : 'no'; }}");
    run(&script, &format!("http://{host}/a"), host).unwrap() == "yes"
}

fn fails(script: &str) -> String {
    run(script, "http://h/", "h").unwrap_err()
}

#[test]
fn the_answer_comes_back() {
    let script = "function FindProxyForURL(u, h) { return 'PROXY p.example:8080; DIRECT'; }";
    assert_eq!(
        run(script, "http://h/", "h").unwrap(),
        "PROXY p.example:8080; DIRECT"
    );
}

#[test]
fn names() {
    for (expr, host) in [
        ("isPlainHostName(host)", "intranet"),
        ("dnsDomainIs(host, '.example.com')", "www.example.com"),
        ("dnsDomainIs(host, 'Example.COM')", "example.com"),
        ("localHostOrDomainIs(host, 'www.example.com')", "www"),
        (
            "localHostOrDomainIs(host, 'www.example.com')",
            "www.example.com",
        ),
        ("dnsDomainLevels(host) === 2", "www.example.com"),
        ("shExpMatch(host, '*.example.com')", "a.b.example.com"),
        ("shExpMatch(host, 'a?c')", "abc"),
        ("shExpMatch(url, '*://*/a')", "h"),
        ("shExpMatch('x+y.(z)', 'x+y.(*)')", "h"),
    ] {
        assert!(holds(expr, host), "{expr} for {host}");
    }
    for (expr, host) in [
        ("isPlainHostName(host)", "www.example.com"),
        ("dnsDomainIs(host, '.example.com')", "badexample.com"),
        (
            "localHostOrDomainIs(host, 'www.example.com')",
            "www.other.com",
        ),
        ("shExpMatch(host, '*.example.com')", "example.org"),
        ("shExpMatch('axc', 'a.c')", "h"),
    ] {
        assert!(!holds(expr, host), "{expr} for {host}");
    }
}

#[test]
fn addresses() {
    for (expr, host) in [
        ("isInNet(host, '10.0.0.0', '255.0.0.0')", "10.20.30.40"),
        ("isInNet(host, '127.0.0.0', '255.0.0.0')", "localhost"),
        (
            "isInNet('255.255.255.255', '255.255.0.0', '255.255.0.0')",
            "h",
        ),
        ("dnsResolve(host) === '127.0.0.1'", "localhost"),
        ("dnsResolve(host) === '10.1.2.3'", "10.1.2.3"),
        ("isResolvable(host)", "localhost"),
        ("convert_addr('192.168.1.2') === 3232235778", "h"),
        ("/^\\d+\\.\\d+\\.\\d+\\.\\d+$/.test(myIpAddress())", "h"),
    ] {
        assert!(holds(expr, host), "{expr} for {host}");
    }
    for (expr, host) in [
        ("isInNet(host, '10.0.0.0', '255.0.0.0')", "11.0.0.1"),
        ("isInNet(host, 'junk', '255.0.0.0')", "10.0.0.1"),
        ("isResolvable(host)", "nothing.invalid"),
    ] {
        assert!(!holds(expr, host), "{expr} for {host}");
    }
}

#[test]
fn days_dates_and_times() {
    for expr in [
        "weekdayRange('SUN', 'SAT')",
        "weekdayRange('MON', 'SUN', 'GMT')",
        "weekdayRange(['SUN','MON','TUE','WED','THU','FRI','SAT'][new Date().getDay()])",
        "dateRange(1, 31)",
        "dateRange('JAN', 'DEC')",
        "dateRange('JUL', 'JUN')",
        "dateRange(2000, 2199)",
        "dateRange(1, 'JAN', 31, 'DEC')",
        "dateRange('JAN', 2000, 'DEC', 2199, 'GMT')",
        "dateRange(1, 'JAN', 2000, 31, 'DEC', 2199)",
        "dateRange(new Date().getDate())",
        "dateRange(new Date().getFullYear())",
        "timeRange(0, 23)",
        "timeRange(0, 0, 23, 59)",
        "timeRange(0, 0, 0, 23, 59, 59, 'GMT')",
        "timeRange(new Date().getHours())",
    ] {
        assert!(holds(expr, "h"), "{expr}");
    }
    for expr in [
        "dateRange(1990, 1999)",
        "dateRange(1, 'JAN', 1990, 31, 'DEC', 1999)",
        "weekdayRange('NOPE')",
        "timeRange((new Date().getHours() + 12) % 24)",
    ] {
        assert!(!holds(expr, "h"), "{expr}");
    }
}

#[test]
fn broken_scripts() {
    assert!(fails("var x = 1;").contains("FindProxyForURL"));
    assert!(fails("function FindProxyForURL( {").contains("script"));
    assert!(fails("function FindProxyForURL(){ throw new Error('boom') }").contains("boom"));
    assert!(fails("function FindProxyForURL(){ return 5 }").contains("text"));
}

#[test]
fn hostile_scripts() {
    for script in [
        "function FindProxyForURL(){ for(;;){} }",
        "for(;;){}",
        "function FindProxyForURL(){ var a=[]; for(;;) a.push(new Array(100000).join('x')+a.length) }",
        "function FindProxyForURL(){ function f(){ f() } f() }",
        "function FindProxyForURL(){ return new Array(1 << 20).join('x') }",
    ] {
        let start = Instant::now();
        fails(script);
        assert!(start.elapsed() < Duration::from_secs(10), "{script}");
    }
}

#[test]
fn nothing_else_is_reachable() {
    for name in [
        "std",
        "os",
        "require",
        "fetch",
        "process",
        "load",
        "XMLHttpRequest",
    ] {
        assert!(
            holds(&format!("typeof {name} === 'undefined'"), "h"),
            "{name}"
        );
    }
}
