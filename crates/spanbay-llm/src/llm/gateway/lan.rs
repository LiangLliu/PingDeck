//! The machine's LAN identity: its IPv4 addresses with their interface names, and its host name.
//!
//! Single responsibility: tell the Host guard which names reach this machine, the status
//! DTO which addresses to show, and the address watcher when they change, caching the
//! answer for 10 seconds so a request never waits on a probe. [`LanNames::snapshot`] is the
//! async, request-path entry; the lookup itself blocks (`getifaddrs` through `if-addrs`, the
//! `hostname` command) and runs on the blocking pool. When enumeration finds nothing, only the
//! address of the default route (a UDP connect probe, no packet is sent) is used, without an
//! interface name.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::status::LanAddress;

/// How long one lookup is trusted.
const CACHE_FOR: Duration = Duration::from_secs(10);

/// One assigned IPv4 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Interface {
    /// The interface name (`en0`), when the OS tool says it.
    pub(super) name: Option<String>,
    pub(super) ip: Ipv4Addr,
}

impl Interface {
    pub(super) fn new(name: &str, ip: Ipv4Addr) -> Self {
        Self {
            name: Some(name.to_owned()),
            ip,
        }
    }
}

/// What the machine is called on the local network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LanSnapshot {
    /// Non-loopback IPv4 addresses assigned to this machine.
    pub(super) addresses: Vec<Interface>,
    /// The machine name, lower case.
    pub(super) hostname: Option<String>,
}

impl LanSnapshot {
    /// Whether `name` (lower case, no port) is this machine: an assigned IPv4 literal, the host
    /// name, or `<hostname>.local`.
    pub(super) fn is_me(&self, name: &str) -> bool {
        if let Ok(ip) = name.parse::<Ipv4Addr>() {
            return self.addresses.iter().any(|interface| interface.ip == ip);
        }
        self.hostname.as_deref().is_some_and(|host| {
            let base = host.strip_suffix(".local").unwrap_or(host);
            name == base || name.strip_suffix(".local") == Some(base)
        })
    }

    /// The same addresses (interface names and the host name do not count): what the address
    /// watcher compares.
    pub(super) fn same_addresses(&self, other: &Self) -> bool {
        let sorted = |snapshot: &Self| {
            let mut ips: Vec<Ipv4Addr> = snapshot.addresses.iter().map(|i| i.ip).collect();
            ips.sort();
            ips
        };
        sorted(self) == sorted(other)
    }

    /// The addresses in `self` that `before` did not have.
    pub(super) fn added_since(&self, before: &Self) -> Vec<Ipv4Addr> {
        self.addresses
            .iter()
            .map(|interface| interface.ip)
            .filter(|ip| !before.addresses.iter().any(|old| old.ip == *ip))
            .collect()
    }

    /// The names that reach this machine: private-range addresses
    /// first, then `<hostname>.local`, then the other addresses (the UI folds those under "Show
    /// other addresses"; `private` is that flag).
    pub(super) fn rows(&self) -> Vec<LanAddress> {
        let row = |interface: &Interface| LanAddress {
            host: interface.ip.to_string(),
            interface: interface.name.clone(),
            private: interface.ip.is_private(),
        };
        let mut rows: Vec<LanAddress> = self
            .addresses
            .iter()
            .filter(|interface| interface.ip.is_private())
            .map(row)
            .collect();
        if let Some(host) = &self.hostname {
            rows.push(LanAddress {
                host: format!("{}.local", host.strip_suffix(".local").unwrap_or(host)),
                interface: None,
                private: true,
            });
        }
        rows.extend(
            self.addresses
                .iter()
                .filter(|interface| !interface.ip.is_private())
                .map(row),
        );
        rows
    }
}

type Source = Box<dyn Fn() -> LanSnapshot + Send + Sync>;

/// A cached [`LanSnapshot`].
pub(super) struct LanNames {
    cache: Mutex<Option<(Instant, Arc<LanSnapshot>)>>,
    source: Mutex<Source>,
}

impl LanNames {
    /// Reads the real machine.
    pub(super) fn system() -> Self {
        Self {
            cache: Mutex::new(None),
            source: Mutex::new(Box::new(probe)),
        }
    }

    /// Answers `snapshot` from now on instead of probing (tests have no LAN to rely on).
    #[cfg(test)]
    pub(super) fn pin(&self, snapshot: LanSnapshot) {
        *self.source.lock().unwrap_or_else(PoisonError::into_inner) =
            Box::new(move || snapshot.clone());
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn fresh(&self) -> Option<Arc<LanSnapshot>> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .as_ref()
            .filter(|(at, _)| at.elapsed() < CACHE_FOR)
            .map(|(_, snapshot)| Arc::clone(snapshot))
    }

    /// Looks the machine up now, whatever the cache holds, and caches the answer. Blocks.
    pub(super) fn refresh_blocking(&self) -> Arc<LanSnapshot> {
        let snapshot = Arc::new((self.source.lock().unwrap_or_else(PoisonError::into_inner))());
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), Arc::clone(&snapshot)));
        snapshot
    }

    /// The snapshot, looked up again when older than 10 s. Blocks while looking.
    pub(super) fn snapshot_blocking(&self) -> Arc<LanSnapshot> {
        self.fresh().unwrap_or_else(|| self.refresh_blocking())
    }

    /// [`LanNames::snapshot_blocking`] for the request path: a cached answer returns at once,
    /// a lookup runs on the blocking pool.
    pub(super) async fn snapshot(self: &Arc<Self>) -> Arc<LanSnapshot> {
        if let Some(snapshot) = self.fresh() {
            return snapshot;
        }
        let this = Arc::clone(self);
        // A failed task (a panic in the probe) means "no LAN identity": only loopback passes.
        tokio::task::spawn_blocking(move || this.snapshot_blocking())
            .await
            .unwrap_or_default()
    }

    /// [`LanNames::refresh_blocking`] on the blocking pool (the address watcher's entry).
    pub(super) async fn refresh(self: &Arc<Self>) -> Option<Arc<LanSnapshot>> {
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || this.refresh_blocking())
            .await
            .ok()
    }
}

fn probe() -> LanSnapshot {
    let mut addresses: Vec<Interface> = Vec::new();
    for interface in if_addrs::get_if_addrs().unwrap_or_default() {
        if let IpAddr::V4(ip) = interface.ip() {
            let interface = Interface::new(&interface.name, ip);
            if usable(ip) && !addresses.contains(&interface) {
                addresses.push(interface);
            }
        }
    }
    if addresses.is_empty() {
        addresses = primary_ipv4()
            .map(|ip| Interface { name: None, ip })
            .into_iter()
            .collect();
    }
    LanSnapshot {
        addresses,
        hostname: hostname(),
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether `ip` is an address a client could be told to use.
fn usable(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified()
}

/// The IPv4 address the OS would use to reach the internet. Connecting a UDP socket only picks
/// a route; nothing is sent, so 192.0.2.1 (TEST-NET-1) need not be reachable.
fn primary_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if usable(ip) => Some(ip),
        _ => None,
    }
}

/// The machine name from the `hostname` command, else the environment.
fn hostname() -> Option<String> {
    command_output("hostname", &[])
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> LanSnapshot {
        LanSnapshot {
            addresses: vec![
                Interface::new("bridge100", Ipv4Addr::new(100, 64, 0, 1)),
                Interface::new("en0", Ipv4Addr::new(192, 168, 1, 20)),
                Interface::new("en5", Ipv4Addr::new(10, 0, 0, 5)),
            ],
            hostname: Some("mac".into()),
        }
    }

    #[test]
    fn only_assigned_addresses_and_this_machines_names_are_me() {
        let snapshot = snapshot();
        for name in ["192.168.1.20", "100.64.0.1", "mac", "mac.local"] {
            assert!(snapshot.is_me(name), "{name}");
        }
        for name in ["192.168.1.21", "mac.example", "evil.mac.local", "", "local"] {
            assert!(!snapshot.is_me(name), "{name}");
        }
    }

    #[test]
    fn a_host_name_that_already_ends_in_local_is_not_doubled() {
        let snapshot = LanSnapshot {
            addresses: Vec::new(),
            hostname: Some("mac.local".into()),
        };
        assert!(snapshot.is_me("mac.local") && snapshot.is_me("mac"));
        let rows = snapshot.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].host, "mac.local");
    }

    #[test]
    fn rows_put_private_addresses_first_and_flag_the_rest() {
        let rows: Vec<(String, Option<String>, bool)> = snapshot()
            .rows()
            .into_iter()
            .map(|row| (row.host, row.interface, row.private))
            .collect();
        assert_eq!(
            rows,
            [
                ("192.168.1.20".into(), Some("en0".into()), true),
                ("10.0.0.5".into(), Some("en5".into()), true),
                ("mac.local".into(), None, true),
                ("100.64.0.1".into(), Some("bridge100".into()), false),
            ]
        );
    }

    #[test]
    fn address_changes_ignore_order_and_names() {
        let before = snapshot();
        let mut after = snapshot();
        after.addresses.reverse();
        after.hostname = Some("other".into());
        assert!(before.same_addresses(&after));
        after
            .addresses
            .push(Interface::new("en7", Ipv4Addr::new(192, 168, 9, 9)));
        assert!(!before.same_addresses(&after));
        assert_eq!(after.added_since(&before), [Ipv4Addr::new(192, 168, 9, 9)]);
    }

    #[test]
    fn a_lookup_is_cached_until_refreshed_or_the_source_is_pinned_again() {
        let names = LanNames::system();
        names.pin(snapshot());
        let first = names.snapshot_blocking();
        let second = names.snapshot_blocking();
        assert!(
            Arc::ptr_eq(&first, &second),
            "the second call hits the cache"
        );
        assert_eq!(*first, snapshot());
        assert!(!Arc::ptr_eq(&first, &names.refresh_blocking()));
        names.pin(LanSnapshot::default());
        assert_eq!(*names.snapshot_blocking(), LanSnapshot::default());
    }
}
