//! Where a webhook may go (design §4.4 and §6, SSRF): the allow-list
//! `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`, checked when an endpoint is
//! made or changed, and again at every attempt, on a fresh lookup, the
//! address it checked being the one the attempt connects to
//! ([`pin`]: no DNS rebinding between the check and the connection).
//!
//! The rules ([`DestinationPolicy`]):
//!
//! - **The URL**: `https` (plain `http` only with `WA_SERVER_ENV=development`),
//!   no user name or password, a host, at most [`super::MAX_URL_LEN`]
//!   bytes. It is parsed as browsers do (WHATWG), so `https://2130706433/`
//!   and `https://0x7f.1/` are the address `127.0.0.1` they spell.
//! - **The host**: listed by name (`hooks.example.com`, or
//!   `*.example.com` for its subdomains), or else every address it
//!   resolves to inside a listed CIDR; an IP address in the URL must be
//!   inside a listed CIDR.
//! - **Every address** the host resolves to must pass, else the attempt
//!   is refused, whichever address it would have used: an IPv4-mapped,
//!   -compatible or NAT64 IPv6 address is judged as the IPv4 address it
//!   carries. A private, loopback, link-local, shared (CGNAT),
//!   documentation, benchmarking, reserved, 6to4 or Teredo address, and
//!   any IPv6 address outside the global unicast `2000::/3` (the
//!   IPv4-translated `::ffff:0:a.b.c.d` among them), passes only inside a
//!   listed CIDR (a receiver in the same cluster); an
//!   unspecified (`0.0.0.0/8`, `::`), broadcast or multicast address and
//!   the clouds' metadata addresses never do, listed or not. A public
//!   address passes for a host listed by name, or inside a listed CIDR.
//!
//! Empty, the list admits nothing: every endpoint is refused (`422` on
//! `url`) until the operator lists its destination.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;

use super::{AttemptError, MAX_URL_LEN};

/// How long a lookup may take before the attempt (or the creation) is
/// refused.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// The clouds' instance metadata addresses: never a destination, even
/// inside a listed CIDR (AWS, GCP and Azure's `169.254.169.254`, AWS ECS'
/// `169.254.170.2`, Alibaba's `100.100.100.200`, AWS' IPv6
/// `fd00:ec2::254`).
pub const METADATA_ADDRESSES: [IpAddr; 4] = [
    IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
    IpAddr::V4(Ipv4Addr::new(169, 254, 170, 2)),
    IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)),
];

/// A CIDR block of the allow-list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// `a.b.c.d/n`, `x::/n`, or a bare address (its own block).
    pub fn parse(text: &str) -> Option<Self> {
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let network: IpAddr = addr.parse().ok()?;
        let max = if network.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok()?,
            Some(_) => return None,
        };
        (prefix <= max).then_some(Self { network, prefix })
    }

    /// Whether `ip` is inside it (an IPv4 block holds IPv4 addresses only,
    /// an IPv6 one IPv6 addresses only).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// A host of the allow-list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostPattern {
    /// This host.
    Exact(String),
    /// Any subdomain of this one (`*.example.com`: `.example.com`).
    Subdomains(String),
}

impl HostPattern {
    fn matches(&self, host: &str) -> bool {
        match self {
            Self::Exact(name) => host == name,
            Self::Subdomains(suffix) => {
                host.len() > suffix.len() && host.ends_with(suffix.as_str())
            }
        }
    }
}

/// Why `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS` was refused. Names the
/// entry's position, never its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("entry {0} is neither a host name, `*.` and a host name, nor a CIDR other than /0")]
pub struct InvalidAllowList(pub usize);

/// The allow-list, and whether plain `http` is allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DestinationPolicy {
    hosts: Vec<HostPattern>,
    cidrs: Vec<Cidr>,
    allow_http: bool,
}

/// Why a URL or an address was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not an `https` URL with a host (plain `http` outside development, a
    /// user name or password, too long, no host).
    Url,
    /// The host is not listed, and is not an address of a listed CIDR.
    NotListed,
    /// An address the host resolved to is not allowed.
    Address,
    /// The host did not resolve.
    Unresolved,
}

impl From<Refusal> for AttemptError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unresolved => Self::ResolveFailed,
            Refusal::Url | Refusal::NotListed | Refusal::Address => Self::DestinationRefused,
        }
    }
}

/// The host of a checked URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// A name, to resolve (lowercase, no trailing dot).
    Name(String),
    /// An address.
    Ip(IpAddr),
}

/// A URL the allow-list admits by its text: what an attempt resolves,
/// connects to and asks. `Debug` shows its host only (the path or query
/// may carry the receiver's token).
#[derive(Clone, PartialEq, Eq)]
pub struct Destination {
    /// Whether it is `https`.
    pub https: bool,
    /// Its host.
    pub host: Host,
    /// Its port (the scheme's default when none is written).
    pub port: u16,
    /// What the request line asks: the path and the query.
    pub path_and_query: String,
    /// The `Host` header: the host, and the port when it is not the
    /// scheme's default.
    pub authority: String,
    /// Whether its host is listed by name (so a public address passes).
    named: bool,
}

impl fmt::Debug for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Destination")
            .field("https", &self.https)
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// `ip` as the address it carries: the IPv4 address of an IPv4-mapped
/// (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`, but `::` and `::1`)
/// or NAT64 (`64:ff9b::a.b.c.d`) IPv6 address; any other unchanged.
pub fn canonical(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    let segments = v6.segments();
    let embedded = Ipv4Addr::from(u32::try_from(u128::from(v6) & 0xffff_ffff).unwrap_or(0));
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return IpAddr::V4(embedded);
    }
    if segments[..6] == [0; 6] && !v6.is_unspecified() && !v6.is_loopback() {
        return IpAddr::V4(embedded);
    }
    ip
}

/// Never a destination, listed or not: unspecified, broadcast, multicast,
/// a metadata address.
fn forbidden(ip: IpAddr) -> bool {
    if METADATA_ADDRESSES.contains(&ip) {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 0 || v4.is_broadcast() || v4.is_multicast(),
        IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast(),
    }
}

/// Not a public address: private, loopback, link-local, shared, reserved,
/// documentation, benchmarking, protocol assignments, 6to4, Teredo,
/// unique-local, site-local (IANA's special-purpose registries), and every
/// IPv6 address outside the global unicast block `2000::/3` (the
/// IPv4-translated `::ffff:0:0:0/96`, the dummy prefix `100:0:0:1::/64`,
/// SRv6's `5f00::/16`, the reserved rest), or inside `3fff::/20`
/// (documentation).
pub fn is_special(ip: IpAddr) -> bool {
    const V4: [(u32, u8); 14] = [
        (0x0000_0000, 8),  // 0.0.0.0/8 "this network"
        (0x0a00_0000, 8),  // 10.0.0.0/8
        (0x6440_0000, 10), // 100.64.0.0/10 shared (CGNAT)
        (0x7f00_0000, 8),  // 127.0.0.0/8 loopback
        (0xa9fe_0000, 16), // 169.254.0.0/16 link-local
        (0xac10_0000, 12), // 172.16.0.0/12
        (0xc000_0000, 24), // 192.0.0.0/24 protocol assignments
        (0xc000_0200, 24), // 192.0.2.0/24 documentation
        (0xc058_6300, 24), // 192.88.99.0/24 6to4 relay
        (0xc0a8_0000, 16), // 192.168.0.0/16
        (0xc612_0000, 15), // 198.18.0.0/15 benchmarking
        (0xc633_6400, 24), // 198.51.100.0/24 documentation
        (0xcb00_7100, 24), // 203.0.113.0/24 documentation
        (0xe000_0000, 3),  // 224.0.0.0/4 multicast and 240.0.0.0/4 reserved
    ];
    const V6: [(u128, u8); 13] = [
        (0, 128),                                        // ::
        (1, 128),                                        // ::1
        (0xffff_0000_0000, 96),                          // ::ffff:0:0/96 (judged as IPv4 first)
        (0x0064_ff9b_0000_0000_0000_0000_0000_0000, 96), // NAT64 (judged as IPv4 first)
        (0x0064_ff9b_0001_0000_0000_0000_0000_0000, 48), // local-use NAT64
        (0x0100_0000_0000_0000_0000_0000_0000_0000, 64), // discard-only
        (0x2001_0000_0000_0000_0000_0000_0000_0000, 23), // protocol assignments, Teredo
        (0x2001_0db8_0000_0000_0000_0000_0000_0000, 32), // documentation
        (0x2002_0000_0000_0000_0000_0000_0000_0000, 16), // 6to4
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 20), // documentation (RFC 9637)
        (0xfc00_0000_0000_0000_0000_0000_0000_0000, 7),  // unique-local
        (0xfe80_0000_0000_0000_0000_0000_0000_0000, 9), // link-local fe80::/10 and site-local fec0::/10
        (0xff00_0000_0000_0000_0000_0000_0000_0000, 8), // multicast
    ];
    match ip {
        IpAddr::V4(v4) => V4.iter().any(|(net, prefix)| {
            let block = Cidr {
                network: IpAddr::V4(Ipv4Addr::from(*net)),
                prefix: *prefix,
            };
            block.contains(IpAddr::V4(v4))
        }),
        IpAddr::V6(v6) => {
            // Only global unicast is public: whatever else a later RFC
            // carves out of the reserved space is refused already.
            let global_unicast = u128::from(v6) >> 125 == 0b001;
            !global_unicast
                || V6.iter().any(|(net, prefix)| {
                    let block = Cidr {
                        network: IpAddr::V6(Ipv6Addr::from(*net)),
                        prefix: *prefix,
                    };
                    block.contains(IpAddr::V6(v6))
                })
        }
    }
}

impl DestinationPolicy {
    /// The list `list` (comma-separated host names, `*.`-prefixed host
    /// names and CIDRs; empty or blank: nothing), plain `http` allowed or
    /// not (`WA_SERVER_ENV=development` only).
    ///
    /// # Errors
    ///
    /// [`InvalidAllowList`]: an entry is none of those (`/0` included: a
    /// list admits destinations, never every address).
    pub fn parse(list: &str, allow_http: bool) -> Result<Self, InvalidAllowList> {
        let mut policy = Self {
            allow_http,
            ..Self::default()
        };
        for (i, entry) in list
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .enumerate()
        {
            let position = i + 1;
            if entry.contains('/') || entry.parse::<IpAddr>().is_ok() {
                let cidr = Cidr::parse(entry).ok_or(InvalidAllowList(position))?;
                if cidr.prefix == 0 {
                    return Err(InvalidAllowList(position));
                }
                policy.cidrs.push(cidr);
                continue;
            }
            let name = entry.to_ascii_lowercase();
            let name = name.trim_end_matches('.');
            let (pattern, bare) = match name.strip_prefix("*.") {
                Some(rest) => (HostPattern::Subdomains(format!(".{rest}")), rest),
                None => (HostPattern::Exact(name.to_owned()), name),
            };
            let valid = !bare.is_empty()
                && bare.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                });
            if !valid {
                return Err(InvalidAllowList(position));
            }
            policy.hosts.push(pattern);
        }
        Ok(policy)
    }

    /// Whether it admits nothing.
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty() && self.cidrs.is_empty()
    }

    /// Whether plain `http` is allowed.
    pub fn allows_http(&self) -> bool {
        self.allow_http
    }

    /// `url`, checked by its text: the scheme, no credentials, a host
    /// that is listed or an address of a listed CIDR (see the module).
    ///
    /// # Errors
    ///
    /// [`Refusal::Url`], [`Refusal::NotListed`], [`Refusal::Address`] (an
    /// address in the URL that is not allowed).
    pub fn check_url(&self, url: &str) -> Result<Destination, Refusal> {
        if url.len() > MAX_URL_LEN {
            return Err(Refusal::Url);
        }
        let parsed = url::Url::parse(url).map_err(|_| Refusal::Url)?;
        let https = match parsed.scheme() {
            "https" => true,
            "http" if self.allow_http => false,
            _ => return Err(Refusal::Url),
        };
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(Refusal::Url);
        }
        let port = parsed.port_or_known_default().ok_or(Refusal::Url)?;
        let (host, named) = match parsed.host().ok_or(Refusal::Url)? {
            url::Host::Domain(name) => {
                let name = name.trim_end_matches('.').to_ascii_lowercase();
                if name.is_empty() {
                    return Err(Refusal::Url);
                }
                let named = self.hosts.iter().any(|pattern| pattern.matches(&name));
                (Host::Name(name), named)
            }
            url::Host::Ipv4(v4) => (Host::Ip(IpAddr::V4(v4)), false),
            url::Host::Ipv6(v6) => (Host::Ip(IpAddr::V6(v6)), false),
        };
        if !named && self.cidrs.is_empty() {
            return Err(Refusal::NotListed);
        }
        let host_text = parsed.host_str().ok_or(Refusal::Url)?.to_owned();
        let authority = match parsed.port() {
            Some(port) => format!("{host_text}:{port}"),
            None => host_text,
        };
        let mut path_and_query = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            path_and_query.push('?');
            path_and_query.push_str(query);
        }
        let destination = Destination {
            https,
            host,
            port,
            path_and_query,
            authority,
            named,
        };
        if let Host::Ip(ip) = destination.host
            && !self.admits(&destination, ip)
        {
            return Err(if self.in_cidr(ip) {
                Refusal::Address
            } else {
                Refusal::NotListed
            });
        }
        Ok(destination)
    }

    fn in_cidr(&self, ip: IpAddr) -> bool {
        let carried = canonical(ip);
        self.cidrs
            .iter()
            .any(|cidr| cidr.contains(carried) || cidr.contains(ip))
    }

    /// Whether `destination` may connect to `ip` (one of its host's
    /// addresses): see the module.
    pub fn admits(&self, destination: &Destination, ip: IpAddr) -> bool {
        let carried = canonical(ip);
        if forbidden(carried) || forbidden(ip) {
            return false;
        }
        if self.in_cidr(ip) {
            return true;
        }
        // The address an IPv6 form carries is the one judged: a mapped
        // public address is public.
        if is_special(carried) {
            return false;
        }
        destination.named
    }
}

/// Why a lookup failed (nothing of the host).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the host did not resolve")]
pub struct ResolveError;

/// Looks a host up: the system's resolver in the service
/// (`meta-whatsapp-server-http`'s), a stub in tests. Each call is a fresh
/// lookup: [`pin`] checks what it answers, and the attempt connects to it.
#[async_trait]
pub trait Resolver: Send + Sync + 'static {
    /// The addresses of `host` (a name, never an address).
    ///
    /// # Errors
    ///
    /// [`ResolveError`]: no address.
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, ResolveError>;
}

/// A destination and the address an attempt connects to: the one its
/// check admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pinned {
    /// The URL, checked.
    pub destination: Destination,
    /// Where to connect: never another address than this one.
    pub address: SocketAddr,
}

/// `url` checked by its text, then its host looked up once with
/// `resolver` (within [`RESOLVE_TIMEOUT`]) and every address it answers
/// checked: the first, pinned, is the one the attempt connects to. An
/// address in the URL is pinned without a lookup.
///
/// # Errors
///
/// A [`Refusal`]: the URL, its host, an address, or the lookup.
pub async fn pin(
    policy: &DestinationPolicy,
    resolver: &dyn Resolver,
    url: &str,
) -> Result<Pinned, Refusal> {
    let destination = policy.check_url(url)?;
    let addresses = match &destination.host {
        Host::Ip(ip) => vec![*ip],
        Host::Name(name) => {
            match tokio::time::timeout(RESOLVE_TIMEOUT, resolver.resolve(name, destination.port))
                .await
            {
                Ok(Ok(addresses)) if !addresses.is_empty() => addresses,
                _ => return Err(Refusal::Unresolved),
            }
        }
    };
    if !addresses.iter().all(|ip| policy.admits(&destination, *ip)) {
        return Err(Refusal::Address);
    }
    let address = SocketAddr::new(addresses[0], destination.port);
    Ok(Pinned {
        destination,
        address,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn policy(list: &str) -> DestinationPolicy {
        DestinationPolicy::parse(list, false).unwrap()
    }

    /// A resolver answering what it was given, one answer per lookup in
    /// turn (the last repeated): a rebinding host is public, then private.
    struct Answers(Mutex<Vec<Vec<IpAddr>>>);

    #[async_trait]
    impl Resolver for Answers {
        async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, ResolveError> {
            let mut answers = self.0.lock().unwrap();
            if answers.len() > 1 {
                Ok(answers.remove(0))
            } else {
                answers.first().cloned().ok_or(ResolveError)
            }
        }
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// The URL's own rules: `https` only (plain `http` in development), no
    /// credentials, a host; a host not listed is refused before any
    /// lookup, and so is every URL with an empty list. Decisive: the
    /// scheme check, the credentials check, the name match.
    #[test]
    fn a_url_is_https_with_a_listed_host() {
        let p = policy("hooks.example.com, *.medusa.example");
        assert!(p.check_url("https://hooks.example.com/wa?x=1").is_ok());
        assert!(p.check_url("https://HOOKS.example.com./wa").is_ok());
        assert!(p.check_url("https://shop.medusa.example/wa").is_ok());
        assert!(p.check_url("https://a.b.medusa.example/").is_ok());
        assert_eq!(
            p.check_url("https://medusa.example/").unwrap_err(),
            Refusal::NotListed
        );
        assert_eq!(
            p.check_url("https://evil.example/").unwrap_err(),
            Refusal::NotListed
        );
        assert_eq!(
            p.check_url("https://hooks.example.com.evil.example/")
                .unwrap_err(),
            Refusal::NotListed
        );
        for bad in [
            "http://hooks.example.com/",
            "ftp://hooks.example.com/",
            "https://user:pw@hooks.example.com/",
            "https://user@hooks.example.com/",
            "hooks.example.com/wa",
            "file:///etc/passwd",
            "",
        ] {
            assert_eq!(p.check_url(bad).unwrap_err(), Refusal::Url, "{bad}");
        }
        let long = format!("https://hooks.example.com/{}", "a".repeat(MAX_URL_LEN));
        assert_eq!(p.check_url(&long).unwrap_err(), Refusal::Url);
        let dev = DestinationPolicy::parse("hooks.example.com", true).unwrap();
        assert!(!dev.check_url("http://hooks.example.com/").unwrap().https);
        assert_eq!(
            DestinationPolicy::default()
                .check_url("https://hooks.example.com/")
                .unwrap_err(),
            Refusal::NotListed,
            "an empty list admits nothing"
        );
        let d = p
            .check_url("https://hooks.example.com:8443/wa?x=1")
            .unwrap();
        assert_eq!(
            (d.port, d.authority.as_str()),
            (8443, "hooks.example.com:8443")
        );
        assert_eq!(d.path_and_query, "/wa?x=1");
        assert!(!format!("{d:?}").contains("x=1"), "Debug omits the query");
    }

    /// Every private, loopback, link-local, shared, reserved or
    /// documentation address is refused for a host listed by name (in
    /// IPv4 and IPv6, and in every IPv6 form carrying an IPv4 one), and
    /// passes only inside a listed CIDR; unspecified, broadcast, multicast
    /// and metadata addresses never pass. Decisive: `is_special`,
    /// `forbidden`, `canonical`.
    #[test]
    fn non_public_addresses_need_a_listed_cidr_and_some_never_pass() {
        let p = policy("hooks.example.com");
        let d = p.check_url("https://hooks.example.com/").unwrap();
        for special in [
            "10.0.0.1",
            "172.16.5.4",
            "172.31.255.255",
            "192.168.1.1",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "fec0::1",
            "fc00::1",
            "fd12:3456::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "2001::1",
            "2001:db8::1",
            // IPv4-translated (SIIT): `::ffff:0:127.0.0.1`, which a
            // translator on the path turns into loopback.
            "::ffff:0:7f00:1",
            "::ffff:0:a9fe:a9fe",
            "64:ff9b:1::a00:1",
            "100::1",
            "100:0:0:1::1",
            "5f00::1",
            "3fff::1",
            "4000::1",
            "1000::1",
            "e000::1",
        ] {
            assert!(!p.admits(&d, ip(special)), "{special}");
        }
        for never in [
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "::",
            "ff02::1",
            "169.254.169.254",
            "169.254.170.2",
            "100.100.100.200",
            "fd00:ec2::254",
            "::ffff:0.0.0.0",
        ] {
            assert!(!p.admits(&d, ip(never)), "{never}");
            let listed = policy("hooks.example.com, 0.0.0.0/1, 128.0.0.0/1, ::/1, 8000::/1");
            let d = listed.check_url("https://hooks.example.com/").unwrap();
            assert!(!listed.admits(&d, ip(never)), "{never}, listed");
        }
        for public in [
            "93.184.216.34",
            "8.8.8.8",
            "2606:4700::1111",
            "2a00:1450:4001::1",
            "3ffe::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(p.admits(&d, ip(public)), "{public}");
        }
        let cluster = policy("hooks.example.com, 10.0.0.0/8, 127.0.0.0/8, fd00::/8");
        let d = cluster.check_url("https://hooks.example.com/").unwrap();
        for listed in ["10.1.2.3", "127.0.0.1", "::ffff:10.1.2.3", "fd12::1"] {
            assert!(cluster.admits(&d, ip(listed)), "{listed}");
        }
        assert!(
            !cluster.admits(&d, ip("192.168.1.1")),
            "another private block"
        );
        assert!(
            !cluster.admits(&d, ip("fd00:ec2::254")),
            "metadata, inside a listed block"
        );
    }

    /// An address in the URL, in any spelling WHATWG reads (decimal, hex,
    /// octal, IPv6 brackets, IPv4-mapped), is that address: refused
    /// unless a listed CIDR holds it. A host not listed by name passes
    /// only if every address it resolves to is inside a listed CIDR.
    #[tokio::test]
    async fn addresses_in_the_url_and_unlisted_names_need_a_cidr() {
        let p = policy("hooks.example.com");
        for url in [
            "https://127.0.0.1/",
            "https://2130706433/",
            "https://0x7f.1/",
            "https://0177.0.0.1/",
            "https://[::1]/",
            "https://[::ffff:7f00:1]/",
            "https://93.184.216.34/",
        ] {
            assert!(p.check_url(url).is_err(), "{url}");
        }
        let cidr = policy("203.0.113.0/24, 127.0.0.0/8");
        assert!(cidr.check_url("https://203.0.113.10/").is_ok());
        assert!(cidr.check_url("https://2130706433/").is_ok());
        assert_eq!(
            cidr.check_url("https://198.51.100.1/").unwrap_err(),
            Refusal::NotListed
        );
        let inside = Answers(Mutex::new(vec![vec![ip("203.0.113.10")]]));
        assert!(
            pin(&cidr, &inside, "https://receiver.internal/")
                .await
                .is_ok()
        );
        let outside = Answers(Mutex::new(vec![vec![
            ip("203.0.113.10"),
            ip("198.51.100.1"),
        ]]));
        assert_eq!(
            pin(&cidr, &outside, "https://receiver.internal/")
                .await
                .unwrap_err(),
            Refusal::Address,
            "one address outside the list refuses the host"
        );
    }

    /// The spellings a review tries (roadmap M2c's security review): the
    /// short and trailing-dot IPv4 forms are the address they spell; a
    /// look-alike name (Cyrillic `а`, punycode once parsed), a name that
    /// merely ends like a listed one, a listed name under another
    /// domain, and a name with credentials before it are refused; a
    /// listed name written with an ideographic full stop or percent
    /// escapes is that name (WHATWG), so it passes and its own lookup
    /// decides. Decisive: the `.` the `*.` pattern keeps, WHATWG parsing.
    #[test]
    fn host_spellings_are_judged_as_what_they_parse_to() {
        let p = policy("hooks.example.com, *.medusa.example");
        for url in [
            "https://127.1/",
            "https://0x7f000001/",
            "https://127.0.0.1./",
            "https://017700000001/",
            "https://[::ffff:127.0.0.1]/",
            "https://[::ffff:0:7f00:1]/",
            "https://[0:0:0:0:0:ffff:7f00:1]/",
            "https://[64:ff9b::7f00:1]/",
        ] {
            assert_eq!(p.check_url(url).unwrap_err(), Refusal::NotListed, "{url}");
        }
        let loopback = policy("127.0.0.0/8");
        for url in [
            "https://127.1/",
            "https://0x7f000001/",
            "https://127.0.0.1./",
        ] {
            let d = loopback.check_url(url).unwrap();
            assert_eq!(d.host, Host::Ip(ip("127.0.0.1")), "{url}");
        }
        for url in [
            "https://hooks.ex\u{0430}mple.com/",
            "https://xn--hooks-example-com/",
            "https://evilmedusa.example/",
            "https://medusa.example.evil.example/",
            "https://hooks.example.com.evil.example/",
            "https://hooks.example.co/",
            "https://evil.example\\@hooks.example.com/",
        ] {
            assert_eq!(p.check_url(url).unwrap_err(), Refusal::NotListed, "{url}");
        }
        assert_eq!(
            p.check_url("https://evil.example@hooks.example.com/")
                .unwrap_err(),
            Refusal::Url,
            "userinfo"
        );
        for url in [
            "https://hooks\u{3002}example.com/",
            "https://hooks%2Eexample.com/",
            "https://HOOKS.EXAMPLE.COM:443/",
        ] {
            let d = p.check_url(url).unwrap();
            assert_eq!(d.host, Host::Name("hooks.example.com".to_owned()), "{url}");
            assert_eq!(d.authority, "hooks.example.com", "{url}");
        }
    }

    /// CIDR edges: the first and last address of a block are in it, the
    /// ones either side are not; `/32` and `/128` hold one address;
    /// host bits in the network are masked.
    #[test]
    fn cidr_blocks_hold_their_edges_only() {
        let block = Cidr::parse("192.168.4.0/22").unwrap();
        assert!(block.contains(ip("192.168.4.0")));
        assert!(block.contains(ip("192.168.7.255")));
        assert!(!block.contains(ip("192.168.3.255")));
        assert!(!block.contains(ip("192.168.8.0")));
        let one = Cidr::parse("203.0.113.7/32").unwrap();
        assert!(one.contains(ip("203.0.113.7")));
        assert!(!one.contains(ip("203.0.113.6")));
        assert!(!one.contains(ip("203.0.113.8")));
        assert_eq!(Cidr::parse("203.0.113.7"), Some(one));
        let half = Cidr::parse("0.0.0.0/1").unwrap();
        assert!(half.contains(ip("127.255.255.255")));
        assert!(!half.contains(ip("128.0.0.0")));
        let masked = Cidr::parse("10.1.2.3/8").unwrap();
        assert!(masked.contains(ip("10.200.0.1")));
        let v6 = Cidr::parse("fd00:1::/32").unwrap();
        assert!(v6.contains(ip("fd00:1:ffff:ffff:ffff:ffff:ffff:ffff")));
        assert!(!v6.contains(ip("fd00:2::")));
        let host = Cidr::parse("fd00::1/128").unwrap();
        assert!(host.contains(ip("fd00::1")));
        assert!(!host.contains(ip("fd00::2")));
        for bad in ["10.0.0.0/", "10.0.0.0/+8", "10.0.0.0/-1", "::/129", "10.0.0/8"] {
            assert_eq!(Cidr::parse(bad), None, "{bad}");
        }
    }

    /// M2.4's rebinding case at its source: the first lookup answers a
    /// public address and the check passes; the second answers a private
    /// one and the check refuses it, whichever address comes first; the
    /// pinned address is the checked one; a failed lookup is
    /// `Unresolved`. Decisive: `pin` checking every address it answers.
    #[tokio::test]
    async fn each_pin_checks_a_fresh_lookup() {
        let p = policy("hooks.example.com");
        let resolver = Answers(Mutex::new(vec![
            vec![ip("93.184.216.34")],
            vec![ip("93.184.216.35"), ip("10.0.0.5")],
            vec![ip("10.0.0.5")],
        ]));
        let first = pin(&p, &resolver, "https://hooks.example.com:8443/wa")
            .await
            .unwrap();
        assert_eq!(first.address, "93.184.216.34:8443".parse().unwrap());
        assert_eq!(
            pin(&p, &resolver, "https://hooks.example.com/wa")
                .await
                .unwrap_err(),
            Refusal::Address
        );
        assert_eq!(
            pin(&p, &resolver, "https://hooks.example.com/wa")
                .await
                .unwrap_err(),
            Refusal::Address
        );
        let empty = Answers(Mutex::new(Vec::new()));
        assert_eq!(
            pin(&p, &empty, "https://hooks.example.com/")
                .await
                .unwrap_err(),
            Refusal::Unresolved
        );
    }

    /// The list's own syntax: names, `*.` names, CIDRs and bare
    /// addresses; anything else, and `/0`, refuses the start, naming the
    /// entry's position.
    #[test]
    fn the_allow_list_parses_hosts_and_cidrs() {
        let p = policy(" hooks.example.com ,*.medusa.example,10.0.0.0/8, 192.0.2.7, fd00::/8 ,");
        assert_eq!(p.hosts.len(), 2);
        assert_eq!(p.cidrs.len(), 3);
        assert!(DestinationPolicy::parse("", false).unwrap().is_empty());
        for (bad, at) in [
            ("hooks.example.com, 0.0.0.0/0", 2),
            ("::/0", 1),
            ("10.0.0.0/33", 1),
            ("10.0.0.0/x", 1),
            ("exa mple.com", 1),
            ("a..b", 1),
            ("*.", 1),
            ("https://hooks.example.com", 1),
        ] {
            assert_eq!(
                DestinationPolicy::parse(bad, false),
                Err(InvalidAllowList(at)),
                "{bad}"
            );
        }
        assert!(
            Cidr::parse("10.0.0.0/8")
                .unwrap()
                .contains(ip("10.255.0.1"))
        );
        assert!(!Cidr::parse("10.0.0.0/8").unwrap().contains(ip("11.0.0.1")));
        assert!(
            !Cidr::parse("10.0.0.0/8")
                .unwrap()
                .contains(ip("::ffff:10.0.0.1"))
        );
        assert!(Cidr::parse("fd00::/8").unwrap().contains(ip("fdff::1")));
    }
}
