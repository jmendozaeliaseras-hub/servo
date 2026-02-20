/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! DNS-over-HTTPS resolver for privacy-preserving DNS resolution.
//! Uses provider IP addresses directly to avoid bootstrap DNS queries.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use log::{debug, warn};
use parking_lot::Mutex;

struct CacheEntry {
    addresses: Vec<IpAddr>,
    expires: Instant,
}

static DOH_CACHE: LazyLock<Mutex<HashMap<String, CacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Resolve a hostname via DNS-over-HTTPS. Returns the first resolved IP,
/// or None on failure (caller should fall back to system DNS).
pub async fn resolve(hostname: &str) -> Option<IpAddr> {
    // Don't resolve IP addresses or localhost.
    if hostname.parse::<IpAddr>().is_ok() || hostname == "localhost" {
        return None;
    }

    // Check cache first.
    {
        let cache = DOH_CACHE.lock();
        if let Some(entry) = cache.get(hostname) {
            if entry.expires > Instant::now() {
                return entry.addresses.first().copied();
            }
        }
    }

    let hostname = hostname.to_string();
    tokio::task::spawn_blocking(move || resolve_blocking(&hostname))
        .await
        .ok()?
}

fn resolve_blocking(hostname: &str) -> Option<IpAddr> {
    let prefs = servo_config::prefs::get();
    let provider = prefs.network_dns_over_https_provider.clone();
    drop(prefs);

    let (addr, sni, path) = match provider.as_str() {
        "google" => (
            "8.8.8.8:443",
            "dns.google",
            format!("/resolve?name={hostname}&type=A"),
        ),
        "quad9" => (
            "9.9.9.9:443",
            "dns.quad9.net",
            format!("/dns-query?name={hostname}&type=A"),
        ),
        _ => (
            "1.1.1.1:443",
            "cloudflare-dns.com",
            format!("/dns-query?name={hostname}&type=A"),
        ),
    };

    let sock_addr: SocketAddr = addr.parse().ok()?;
    let mut tcp = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(3)).ok()?;
    tcp.set_read_timeout(Some(Duration::from_secs(5))).ok()?;

    // TLS handshake using webpki roots (no system verifier needed for known DoH IPs).
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let server_name = rustls_pki_types::ServerName::try_from(sni)
        .ok()?
        .to_owned();
    let mut conn =
        rustls::ClientConnection::new(Arc::new(config), server_name).ok()?;
    let mut tls = rustls::Stream::new(&mut conn, &mut tcp);

    // Send minimal HTTP/1.1 GET request.
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {sni}\r\nAccept: application/dns-json\r\nConnection: close\r\n\r\n"
    );
    tls.write_all(request.as_bytes()).ok()?;

    // Read response.
    let mut response = Vec::new();
    tls.read_to_end(&mut response).ok()?;

    let response_str = String::from_utf8_lossy(&response);
    let body_start = response_str.find("\r\n\r\n")? + 4;
    let body = &response_str[body_start..];

    parse_doh_json(body, hostname)
}

/// Parse a DNS-over-HTTPS JSON response to extract IP addresses.
/// Expected format (Cloudflare/Google): {"Answer":[{"data":"1.2.3.4","TTL":300, ...}]}
fn parse_doh_json(body: &str, hostname: &str) -> Option<IpAddr> {
    let mut addresses = Vec::new();
    let mut ttl: u64 = 300; // Default 5 minutes.

    // Simple JSON parser: find all "data":"<ip>" fields.
    let mut search = body;
    while let Some(pos) = search.find("\"data\"") {
        search = &search[pos + 6..];
        // Skip to the value.
        let colon = search.find(':')?;
        search = &search[colon + 1..];
        // Skip whitespace and opening quote.
        let quote_start = search.find('"')?;
        search = &search[quote_start + 1..];
        let quote_end = search.find('"')?;
        let value = &search[..quote_end];
        search = &search[quote_end + 1..];

        if let Ok(ip) = value.parse::<IpAddr>() {
            addresses.push(ip);
        }
    }

    // Try to find TTL.
    if let Some(pos) = body.find("\"TTL\"") {
        let after = &body[pos + 5..];
        if let Some(colon) = after.find(':') {
            let after_colon = after[colon + 1..].trim_start();
            let num_end = after_colon
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after_colon.len());
            if let Ok(t) = after_colon[..num_end].parse::<u64>() {
                ttl = t.max(60); // Minimum 60s cache.
            }
        }
    }

    if addresses.is_empty() {
        debug!("DoH: no addresses resolved for {hostname}");
        return None;
    }

    // Cache the result.
    {
        let mut cache = DOH_CACHE.lock();
        cache.insert(
            hostname.to_string(),
            CacheEntry {
                addresses: addresses.clone(),
                expires: Instant::now() + Duration::from_secs(ttl),
            },
        );
    }

    debug!("DoH: resolved {hostname} to {:?} (TTL {ttl}s)", addresses[0]);
    Some(addresses[0])
}
