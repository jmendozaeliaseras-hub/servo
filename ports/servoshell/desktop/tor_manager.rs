/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Manages a Tor daemon process for routing traffic through the Tor network.
//! Searches for a bundled `tor` binary first, then falls back to system PATH.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use log::{error, info, warn};

struct TorState {
    child: Option<Child>,
    socks_port: Option<u16>,
}

static TOR: LazyLock<Mutex<TorState>> = LazyLock::new(|| {
    Mutex::new(TorState {
        child: None,
        socks_port: None,
    })
});

/// Find an available port by binding to port 0.
fn find_free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .map(|listener| listener.local_addr().unwrap().port())
}

/// Find the `tor` binary. Checks bundled location first, then system PATH.
fn find_tor_binary() -> Option<String> {
    // Check bundled location relative to executable.
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            let bundled = dir.join("tor").join(if cfg!(windows) {
                "tor.exe"
            } else {
                "tor"
            });
            if bundled.exists() {
                return Some(bundled.to_string_lossy().to_string());
            }
        }
    }

    // Check system PATH using `which` on Unix or `where` on Windows.
    let check_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = Command::new(check_cmd).arg("tor").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    None
}

/// Start the Tor daemon (or return the existing port if already running).
/// Returns the SOCKS5 port if successful, or None on failure.
pub fn start_or_get_port() -> Option<u16> {
    let mut state = TOR.lock().ok()?;

    // If already running, check if the process is still alive.
    if let Some(port) = state.socks_port {
        if let Some(ref mut child) = state.child {
            match child.try_wait() {
                Ok(None) => return Some(port), // Still running.
                Ok(Some(status)) => {
                    warn!("Tor process exited with status: {status}");
                    state.child = None;
                    state.socks_port = None;
                },
                Err(e) => {
                    warn!("Failed to check Tor process status: {e}");
                },
            }
        }
    }

    let tor_binary = find_tor_binary();
    if tor_binary.is_none() {
        error!("Tor binary not found. Install Tor or place it in the application directory.");
        return None;
    }
    let tor_binary = tor_binary.unwrap();

    let socks_port = find_free_port()?;
    info!("Starting Tor on SOCKS5 port {socks_port}...");

    let mut child = Command::new(&tor_binary)
        .arg("--SocksPort")
        .arg(socks_port.to_string())
        .arg("--Log")
        .arg("notice stderr")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| error!("Failed to start Tor: {e}"))
        .ok()?;

    // Wait for Tor to bootstrap (look for "100%" in stderr).
    let stderr = child.stderr.take()?;
    let reader = BufReader::new(stderr);
    let start = Instant::now();
    let timeout = Duration::from_secs(60);

    for line in reader.lines() {
        if start.elapsed() > timeout {
            warn!("Tor bootstrap timed out after 60s");
            let _ = child.kill();
            return None;
        }
        match line {
            Ok(line) => {
                if line.contains("Bootstrapped 100%") {
                    info!("Tor bootstrapped successfully on port {socks_port}");
                    state.child = Some(child);
                    state.socks_port = Some(socks_port);
                    return Some(socks_port);
                }
                if line.contains("[err]") {
                    error!("Tor error: {line}");
                }
            },
            Err(e) => {
                warn!("Error reading Tor output: {e}");
                break;
            },
        }
    }

    warn!("Tor process ended without completing bootstrap");
    let _ = child.kill();
    None
}

/// Stop the running Tor daemon, if any.
pub fn stop() {
    if let Ok(mut state) = TOR.lock() {
        if let Some(ref mut child) = state.child {
            info!("Stopping Tor daemon...");
            let _ = child.kill();
            let _ = child.wait();
        }
        state.child = None;
        state.socks_port = None;
    }
}
