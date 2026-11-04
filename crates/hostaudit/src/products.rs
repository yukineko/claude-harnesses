//! The monitored products of canon v1.1 §4.1 (S8 / S9): W21 version commands,
//! dpkg package-name mapping, docker image mapping and version parsing.
//! Adding a product is a canon revision (canon v1.1 §4.1), i.e. a change here.

/// (endoflife.date slug, W21 command) — CONTRACT §12.3, in this order.
pub const W21: [(&str, &str); 20] = [
    ("nodejs", "node --version"),
    ("python", "python3 --version"),
    ("python", "python2 --version"),
    ("java", "java -version"),
    ("ruby", "ruby -v"),
    ("docker-engine", "docker --version"),
    ("containerd", "containerd --version"),
    ("openssl", "openssl version"),
    ("php", "php -v"),
    ("nginx", "nginx -v"),
    ("apache-http-server", "apache2 -v"),
    ("apache-http-server", "httpd -v"),
    ("postgresql", "postgres --version"),
    ("postgresql", "/usr/lib/postgresql/*/bin/postgres --version"),
    ("mysql", "mysqld --version"),
    ("mariadb", "mariadbd --version"),
    ("redis", "redis-server --version"),
    ("mongodb", "mongod --version"),
    ("tomcat", "catalina.sh version"),
    ("go", "go version"),
];

/// W22: the product and the W21 arguments for a listening executable's
/// basename (CONTRACT §12.3: same args as that product's W21 command).
pub fn by_basename(base: &str) -> Option<(&'static str, &'static str)> {
    W21.iter().find_map(|(slug, cmd)| {
        let (exe, args) = cmd.split_once(' ')?;
        let b = exe.rsplit('/').next().unwrap_or(exe);
        (b == base).then_some((*slug, args))
    })
}

/// The leading dotted number of `s` (`21.0.5+11-1` → `21.0.5`); `None` if `s`
/// does not start with a digit.
pub fn leading_version(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut last_dot = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            out.push(c);
            last_dot = false;
        } else if c == '.' && !out.is_empty() && !last_dot {
            out.push(c);
            last_dot = true;
        } else {
            break;
        }
    }
    while out.ends_with('.') {
        out.pop();
    }
    (!out.is_empty()).then_some(out)
}

/// Version of a W21/W22 output (stdout + stderr) for `slug`. Returns the slug
/// actually observed (`mysqld --version` printing `MariaDB` is mariadb, never
/// mysql — canon v1.1 §4.1) and the version, or `None` if unparseable.
pub fn parse_output(slug: &'static str, text: &str) -> Option<(&'static str, String)> {
    let slug = if slug == "mysql" && text.contains("MariaDB") {
        "mariadb"
    } else {
        slug
    };
    let markers: &[&str] = match slug {
        "nodejs" => &["v"],
        "python" => &["Python "],
        "java" => &["version \""],
        "ruby" => &["ruby "],
        "docker-engine" => &["Docker version "],
        "openssl" => &["OpenSSL "],
        "php" => &["PHP "],
        "nginx" => &["nginx/"],
        "apache-http-server" => &["Apache/"],
        "postgresql" => &["(PostgreSQL) "],
        "mysql" | "mariadb" => &["Ver "],
        "redis" => &["v="],
        "mongodb" => &["db version v"],
        "tomcat" => &["Tomcat/"],
        "go" => &["go version go"],
        // containerd prints `containerd <repo> v2.2.1 <hash>`: first token
        // that is a (v-prefixed) dotted number.
        "containerd" => {
            return text
                .split_whitespace()
                .filter_map(|t| {
                    let t = t.strip_prefix('v').unwrap_or(t);
                    leading_version(t).filter(|v| v.contains('.') && v.len() == t.len())
                })
                .next()
                .map(|v| (slug, v));
        }
        _ => return None,
    };
    for line in text.lines() {
        let line = line.trim();
        for m in markers {
            // nodejs: the whole line is `v25.6.0`.
            if *m == "v" {
                if let Some(rest) = line.strip_prefix('v') {
                    if let Some(v) = leading_version(rest) {
                        return Some((slug, v));
                    }
                }
                continue;
            }
            if let Some(pos) = line.find(m) {
                if let Some(v) = leading_version(&line[pos + m.len()..]) {
                    return Some((slug, v));
                }
            }
        }
    }
    None
}

fn is_xy(s: &str) -> bool {
    let mut parts = s.split('.');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && a.chars().all(|c| c.is_ascii_digit())
        && b.chars().all(|c| c.is_ascii_digit())
}

fn is_num(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// The §4.1 dpkg package-name mapping → product slug.
pub fn dpkg_product(name: &str) -> Option<&'static str> {
    match name {
        "nodejs" => return Some("nodejs"),
        "docker.io" | "docker-ce" => return Some("docker-engine"),
        "containerd" | "containerd.io" => return Some("containerd"),
        "openssl" => return Some("openssl"),
        "nginx" => return Some("nginx"),
        "apache2" => return Some("apache-http-server"),
        "redis-server" => return Some("redis"),
        "mongodb-org-server" => return Some("mongodb"),
        _ => {}
    }
    if name.strip_prefix("python").is_some_and(is_xy) {
        return Some("python");
    }
    if name.strip_prefix("ruby").is_some_and(is_xy) {
        return Some("ruby");
    }
    if let Some(r) = name.strip_prefix("openjdk-") {
        if r.split_once('-')
            .is_some_and(|(n, rest)| is_num(n) && !rest.is_empty())
        {
            return Some("java");
        }
    }
    if let Some(r) = name.strip_prefix("libssl") {
        if r.starts_with(|c: char| c.is_ascii_digit())
            && r.chars().all(|c| c.is_ascii_digit() || c == '.')
        {
            return Some("openssl");
        }
    }
    if let Some(r) = name.strip_prefix("php") {
        if r.split_once('-')
            .is_some_and(|(xy, rest)| is_xy(xy) && !rest.is_empty())
        {
            return Some("php");
        }
    }
    if name.strip_prefix("postgresql-").is_some_and(is_num) {
        return Some("postgresql");
    }
    if name
        .strip_prefix("mysql-server-core-")
        .or_else(|| name.strip_prefix("mysql-server-"))
        .is_some_and(is_xy)
    {
        return Some("mysql");
    }
    if name.starts_with("mariadb-server") {
        return Some("mariadb");
    }
    if name.strip_prefix("tomcat").is_some_and(is_num) {
        return Some("tomcat");
    }
    if let Some(r) = name.strip_prefix("golang-") {
        if r.strip_suffix("-go").is_some_and(is_xy) {
            return Some("go");
        }
    }
    None
}

/// dpkg version → product version: epoch dropped, leading dotted number
/// (`5:27.3.1-1~ubuntu` → `27.3.1`, `20.20.2-1nodesource1` → `20.20.2`).
pub fn dpkg_version(v: &str) -> Option<String> {
    let v = match v.split_once(':') {
        Some((e, rest)) if is_num(e) => rest,
        _ => v,
    };
    leading_version(v)
}

/// The §4.1 docker image (last repository path segment) → product slug.
pub fn image_product(repo_last: &str) -> Option<&'static str> {
    Some(match repo_last {
        "node" => "nodejs",
        "python" => "python",
        "openjdk" | "eclipse-temurin" => "java",
        "ruby" => "ruby",
        "php" => "php",
        "nginx" => "nginx",
        "httpd" => "apache-http-server",
        "postgres" => "postgresql",
        "mysql" => "mysql",
        "mariadb" => "mariadb",
        "redis" => "redis",
        "mongo" => "mongodb",
        "tomcat" => "tomcat",
        "golang" => "go",
        _ => return None,
    })
}

/// A W23 image line → (product, `Ok(version)` | `Err(why)`), or `None` when the
/// image is not a monitored product.
pub fn parse_image(img: &str) -> Option<(&'static str, Result<String, String>)> {
    let img = img.trim();
    let (name_tag, digest) = match img.split_once('@') {
        Some((n, d)) => (n, Some(d)),
        None => (img, None),
    };
    let slash = name_tag.rfind('/').map(|i| i + 1).unwrap_or(0);
    let (repo, tag) = match name_tag[slash..].rfind(':') {
        Some(i) => (&name_tag[..slash + i], Some(&name_tag[slash + i + 1..])),
        None => (name_tag, None),
    };
    let last = repo.rsplit('/').next().unwrap_or(repo);
    let slug = image_product(last)?;
    let v = match tag {
        None if digest.is_some() => Err(format!(
            "image {img} is pinned by digest only; no version tag"
        )),
        None => Err(format!("image {img} has no tag")),
        Some("latest") => Err(format!(
            "image {img} uses the `latest` tag; version unknown"
        )),
        Some(t) => {
            leading_version(t).ok_or_else(|| format!("image tag of {img} has no leading version"))
        }
    };
    Some((slug, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            dpkg_version("5:27.3.1-1~ubuntu.22.04~jammy").as_deref(),
            Some("27.3.1")
        );
        assert_eq!(
            dpkg_version("21.0.5+11-1ubuntu1").as_deref(),
            Some("21.0.5")
        );
        assert_eq!(dpkg_version("weird-version"), None);
        assert_eq!(
            parse_output(
                "containerd",
                "containerd github.com/containerd/containerd/v2 v2.2.1 abc"
            ),
            Some(("containerd", "2.2.1".into()))
        );
        assert_eq!(
            parse_output("mysql", "mysqld  Ver 10.11.8-MariaDB for debian"),
            Some(("mariadb", "10.11.8".into()))
        );
        assert_eq!(parse_output("nodejs", "something unexpected"), None);
    }

    #[test]
    fn images() {
        assert_eq!(
            parse_image("redis:7.2-alpine"),
            Some(("redis", Ok("7.2".into())))
        );
        assert!(matches!(
            parse_image("redis:latest"),
            Some(("redis", Err(_)))
        ));
        assert!(matches!(
            parse_image("postgres@sha256:00"),
            Some(("postgresql", Err(_)))
        ));
        assert!(matches!(
            parse_image("reg:5000/lib/mysql:5.7"),
            Some(("mysql", Ok(_)))
        ));
        assert_eq!(parse_image("myapp:1.0"), None);
    }

    #[test]
    fn dpkg_names() {
        for (n, s) in [
            ("python3.12", "python"),
            ("openjdk-21-jre-headless", "java"),
            ("libssl3", "openssl"),
            ("php8.3-cli", "php"),
            ("mysql-server-core-8.0", "mysql"),
            ("tomcat10", "tomcat"),
            ("golang-1.22-go", "go"),
        ] {
            assert_eq!(dpkg_product(n), Some(s), "{n}");
        }
        for n in [
            "python3",
            "openssh-server",
            "linux-image-5.15.0-100-generic",
            "libssl-dev",
        ] {
            assert_eq!(dpkg_product(n), None, "{n}");
        }
    }
}
