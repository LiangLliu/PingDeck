//! The Host and Origin guards that run first for every route, `/health` included.
//!
//! Single responsibility: keep web pages, DNS-rebinding sites and unknown LAN names away.
//! Loopback names are always accepted; LAN mode adds an IPv4 literal assigned to this machine,
//! the machine name and `<hostname>.local`; `ai.gateway.extra_allowed_hosts` apply in every mode.
//! Names are case-insensitive and may carry no port or the bound port (an extra entry with its
//! own port needs exactly that port). The 403 and the log row name the rejected Host.
//! Any `Origin` header is refused (403) in every mode, preflights included; no CORS header is
//! ever sent.

use hyper::header;

use pingdeck_model::settings::AiGatewaySettings;

use super::errors::Refusal;
use super::lan::LanSnapshot;
use super::server::Shared;

/// Names that always reach this gateway.
const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// What the guards decide.
pub(super) enum Verdict {
    /// Continue.
    Pass,
    /// Refuse with 403.
    Refuse(Refusal),
}

/// Splits `host` into its lower-case name (`[::1]` keeps its brackets) and port (`None` without
/// one); `None` for a malformed authority, user info or a port that is not a number.
fn split_host(host: &str) -> Option<(String, Option<u16>)> {
    let authority = host.trim().parse::<http::uri::Authority>().ok()?;
    let name = authority.host();
    let port = match authority.as_str().strip_prefix(name)? {
        "" => None,
        rest => Some(rest.strip_prefix(':')?.parse::<u16>().ok()?),
    };
    Some((name.to_ascii_lowercase(), port))
}

/// Whether a `Host` value (`name` or `name:port`) names this gateway: a loopback name, with LAN
/// serving (`lan`) an address assigned to this machine or its host name, each with no port or the
/// bound `port`; in every mode an `extra` entry (`host`: no port or the bound one; `host:port`:
/// exactly that port).
pub(super) fn host_allowed(
    host: &str,
    port: u16,
    lan: Option<&LanSnapshot>,
    extra: &[String],
) -> bool {
    let Some((name, request_port)) = split_host(host) else {
        return false;
    };
    let on_bound_port = request_port.is_none_or(|requested| requested == port);
    if on_bound_port
        && (LOOPBACK_NAMES.contains(&name.as_str()) || lan.is_some_and(|lan| lan.is_me(&name)))
    {
        return true;
    }
    extra.iter().any(|entry| {
        split_host(entry).is_some_and(|(entry_name, entry_port)| {
            entry_name == name
                && match entry_port {
                    None => on_bound_port,
                    Some(wanted) => request_port == Some(wanted),
                }
        })
    })
}

/// Exactly one `Host` header is required (HTTP/1.0 may have none); an absolute-form target is
/// checked as well.
fn host_ok(parts: &hyper::http::request::Parts, allowed: impl Fn(&str) -> bool) -> bool {
    let mut hosts = parts.headers.get_all(header::HOST).iter();
    let header_ok = match (hosts.next(), hosts.next()) {
        (Some(host), None) => host.to_str().is_ok_and(&allowed),
        (None, _) => parts.version == hyper::Version::HTTP_10,
        _ => false,
    };
    header_ok
        && parts
            .uri
            .authority()
            .is_none_or(|authority| allowed(authority.as_str()))
}

/// The rejected `Host` as the 403 and the log row name it.
fn host_label(parts: &hyper::http::request::Parts) -> String {
    let authority = parts.uri.authority().map(|authority| authority.as_str());
    let mut hosts = parts.headers.get_all(header::HOST).iter();
    let label = match (hosts.next(), hosts.next()) {
        (None, _) => authority.unwrap_or("(missing)").to_owned(),
        (Some(host), None) => {
            let value = String::from_utf8_lossy(host.as_bytes()).into_owned();
            match authority {
                Some(target) if target != value => format!("{value} (target {target})"),
                _ => value,
            }
        }
        _ => "(several)".to_owned(),
    };
    label.chars().take(200).collect()
}

/// Runs the Host guard, then the Origin guard.
pub(super) async fn check(
    shared: &Shared,
    gateway: &AiGatewaySettings,
    parts: &hyper::http::request::Parts,
) -> Verdict {
    let extra = &gateway.extra_allowed_hosts;
    let allowed = if shared.lan {
        let snapshot = shared.lan_names.snapshot().await;
        host_ok(parts, |host| {
            host_allowed(host, shared.port, Some(&snapshot), extra)
        })
    } else {
        host_ok(parts, |host| host_allowed(host, shared.port, None, extra))
    };
    if !allowed {
        return Verdict::Refuse(Refusal::Host {
            host: host_label(parts),
        });
    }
    // No CORS at all: a browser page always sends `Origin`, so any `Origin` is refused.
    if parts.headers.contains_key(header::ORIGIN) {
        return Verdict::Refuse(Refusal::Origin);
    }
    Verdict::Pass
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::super::lan::Interface;
    use super::*;

    fn lan() -> LanSnapshot {
        LanSnapshot {
            addresses: vec![Interface::new("en0", Ipv4Addr::new(192, 168, 1, 20))],
            hostname: Some("mac".into()),
        }
    }

    #[test]
    fn loopback_names_pass_with_no_port_or_the_bound_port_in_any_case() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:4141",
            "localhost",
            "LocalHost:4141",
            "[::1]",
            "[::1]:4141",
        ] {
            assert!(
                host_allowed(host, 4141, None, &[]),
                "{host} must be accepted"
            );
        }
        for host in [
            "",
            "evil.example",
            "evil.example:4141",
            "rebind.test:4141",
            "127.0.0.1:4142",
            "127.0.0.1:",
            "127.0.0.1:abc",
            "localhost.evil.example",
            "localhost.",
            "127.0.0.1.evil.example",
            "user@127.0.0.1",
            "[::1",
            "[::2]",
            "0.0.0.0:4141",
            "192.168.1.20:4141",
            "mac.local:4141",
            "mac:4141",
        ] {
            assert!(
                !host_allowed(host, 4141, None, &[]),
                "{host:?} must be refused"
            );
        }
    }

    #[test]
    fn lan_mode_adds_exactly_the_assigned_address_and_the_machine_names() {
        let snapshot = lan();
        for host in [
            "127.0.0.1:4141",
            "localhost",
            "192.168.1.20:4141",
            "192.168.1.20",
            "MAC:4141",
            "mac.local",
            "Mac.Local:4141",
        ] {
            assert!(
                host_allowed(host, 4141, Some(&snapshot), &[]),
                "{host} must pass"
            );
        }
        for host in [
            "192.168.1.21:4141",
            "10.0.0.5",
            "evil.example",
            "rebind.test:4141",
            "mac.local:4142",
            "mac.local:",
            "mac.example",
            ":4141",
            "",
        ] {
            assert!(
                !host_allowed(host, 4141, Some(&snapshot), &[]),
                "{host:?} must be refused"
            );
        }
    }

    #[test]
    fn extra_hosts_apply_in_every_mode_and_a_port_in_the_entry_is_exact() {
        let extra = vec![
            "Gateway.Example".to_owned(),
            "abc.ngrok.app:80".to_owned(),
            "host.docker.internal".to_owned(),
        ];
        for lan in [None, Some(lan())] {
            let lan = lan.as_ref();
            for host in [
                "gateway.example",
                "GATEWAY.example:4141",
                "abc.ngrok.app:80",
                "host.docker.internal:4141",
            ] {
                assert!(host_allowed(host, 4141, lan, &extra), "{host} must pass");
            }
            for host in [
                "gateway.example:4142",
                "abc.ngrok.app",
                "abc.ngrok.app:4141",
                "other.example",
                "docker.internal",
            ] {
                assert!(!host_allowed(host, 4141, lan, &extra), "{host} must fail");
            }
        }
    }

    fn parts(request: hyper::Request<()>) -> hyper::http::request::Parts {
        request.into_parts().0
    }

    #[test]
    fn exactly_one_host_header_is_required_except_for_http_1_0() {
        let any = |_: &str| true;
        let one = parts(
            hyper::Request::get("/")
                .header("host", "a")
                .body(())
                .unwrap(),
        );
        assert!(host_ok(&one, any));
        let two = parts(
            hyper::Request::get("/")
                .header("host", "a")
                .header("host", "a")
                .body(())
                .unwrap(),
        );
        assert!(!host_ok(&two, any));
        let none_11 = parts(hyper::Request::get("/").body(()).unwrap());
        let none_10 = parts(
            hyper::Request::get("/")
                .version(hyper::Version::HTTP_10)
                .body(())
                .unwrap(),
        );
        assert!(!host_ok(&none_11, any));
        assert!(host_ok(&none_10, any));
    }

    #[test]
    fn an_absolute_form_target_is_checked_too() {
        let allowed = |host: &str| host_allowed(host, 4141, None, &[]);
        let good = parts(
            hyper::Request::get("http://127.0.0.1:4141/health")
                .header("host", "127.0.0.1:4141")
                .body(())
                .unwrap(),
        );
        assert!(host_ok(&good, allowed));
        let rebind = parts(
            hyper::Request::get("http://evil.example/health")
                .header("host", "127.0.0.1:4141")
                .body(())
                .unwrap(),
        );
        assert!(!host_ok(&rebind, allowed));
    }
}
