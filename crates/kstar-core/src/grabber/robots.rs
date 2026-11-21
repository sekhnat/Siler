//! robots.txt: which paths a site asks crawlers to leave alone.

/// The user agents of a group, and its (allow, pattern) rules.
type Group = (Vec<String>, Vec<(bool, String)>);

/// The rules for K-Star from one robots.txt (its own group, or the `*` group).
#[derive(Debug, Clone, Default)]
pub struct Robots {
    /// (allow, pattern)
    rules: Vec<(bool, String)>,
}

impl Robots {
    pub fn parse(text: &str, agent: &str) -> Robots {
        let agent = agent.to_ascii_lowercase();
        // Groups: one or more user-agent lines followed by rules.
        let mut groups: Vec<Group> = Vec::new();
        let mut in_agents = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !in_agents {
                        groups.push((Vec::new(), Vec::new()));
                        in_agents = true;
                    }
                    if let Some(g) = groups.last_mut() {
                        g.0.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    in_agents = false;
                    if let Some(g) = groups.last_mut() {
                        // An empty Disallow allows everything.
                        if !value.is_empty() {
                            g.1.push((key == "allow", value.to_string()));
                        }
                    }
                }
                _ => in_agents = false,
            }
        }
        let own = groups
            .iter()
            .filter(|(agents, _)| {
                agents
                    .iter()
                    .any(|a| a != "*" && agent.contains(a.as_str()))
            })
            .flat_map(|(_, r)| r.clone())
            .collect::<Vec<_>>();
        let rules = if own.is_empty() {
            groups
                .iter()
                .filter(|(agents, _)| agents.iter().any(|a| a == "*"))
                .flat_map(|(_, r)| r.clone())
                .collect()
        } else {
            own
        };
        Robots { rules }
    }

    /// Whether `path` (with its query) may be fetched: the longest matching rule decides,
    /// and Allow wins a tie.
    pub fn allowed(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pat) in &self.rules {
            if matches(pat, path) {
                let len = pat.len();
                match best {
                    Some((l, a)) if l > len || (l == len && a) => {}
                    _ => best = Some((len, *allow)),
                }
            }
        }
        best.is_none_or(|(_, a)| a)
    }
}

/// A robots.txt pattern: a prefix, where `*` matches anything and a final `$` anchors it.
fn matches(pat: &str, path: &str) -> bool {
    let (pat, anchored) = match pat.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pat, false),
    };
    fn go(p: &[u8], s: &[u8], anchored: bool) -> bool {
        match p.split_first() {
            None => !anchored || s.is_empty(),
            Some((b'*', rest)) => (0..=s.len()).any(|i| go(rest, &s[i..], anchored)),
            Some((c, rest)) => s.first() == Some(c) && go(rest, &s[1..], anchored),
        }
    }
    go(pat.as_bytes(), path.as_bytes(), anchored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let r = Robots::parse(
            "User-agent: *\nDisallow: /private/\nDisallow: /*.pdf$\nAllow: /private/open\n\nUser-agent: badbot\nDisallow: /\n",
            "K-Star",
        );
        assert!(r.allowed("/"));
        assert!(r.allowed("/docs/a.html"));
        assert!(!r.allowed("/private/x"));
        assert!(r.allowed("/private/open/y"));
        assert!(!r.allowed("/a/b.pdf"));
        assert!(r.allowed("/a/b.pdf?x=1"));
        let own = Robots::parse(
            "User-agent: *\nDisallow: /\n\nUser-agent: kstar\nDisallow: /tmp\n",
            "K-Star kstar",
        );
        assert!(own.allowed("/docs"));
        assert!(!own.allowed("/tmp/1"));
        assert!(Robots::parse("User-agent: *\nDisallow:\n", "kstar").allowed("/x"));
    }
}
