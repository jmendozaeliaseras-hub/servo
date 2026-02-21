/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Chrome-like extension system supporting a subset of Manifest V3.
//! Loads extensions from ~/.config/servo-private/extensions/<ext-id>/manifest.json.
//! Supports content_scripts (JS/CSS injection) and chrome.storage.local via polyfill.

use std::collections::HashMap;
use std::path::PathBuf;

use log::{error, info, warn};
use serde::Deserialize;

use crate::desktop::browser_storage;

/// A parsed Chrome MV3 manifest.json (subset).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionManifest {
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub content_scripts: Vec<ContentScriptEntry>,
    pub action: Option<ActionEntry>,
    pub background: Option<BackgroundEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentScriptEntry {
    pub matches: Vec<String>,
    #[serde(default)]
    pub js: Vec<String>,
    #[serde(default)]
    pub css: Vec<String>,
    #[serde(default = "default_run_at")]
    pub run_at: String,
}

fn default_run_at() -> String {
    "document_idle".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionEntry {
    #[serde(default)]
    pub default_popup: String,
    #[serde(default)]
    pub default_icon: String,
    #[serde(default)]
    pub default_title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackgroundEntry {
    #[serde(default)]
    pub service_worker: String,
}

/// A loaded extension with its manifest, scripts, and state.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub id: String,
    pub manifest: ExtensionManifest,
    pub base_path: PathBuf,
    /// Loaded content script JS sources, keyed by relative path.
    pub js_sources: HashMap<String, String>,
    /// Loaded content script CSS sources, keyed by relative path.
    pub css_sources: HashMap<String, String>,
    pub enabled: bool,
}

/// A match pattern like `<all_urls>`, `*://*.example.com/*`, `https://example.com/path/*`.
#[derive(Debug, Clone)]
pub struct MatchPattern {
    raw: String,
    scheme: SchemeMatch,
    host: HostMatch,
    path: String,
}

#[derive(Debug, Clone)]
enum SchemeMatch {
    Any,
    Exact(String),
}

#[derive(Debug, Clone)]
enum HostMatch {
    Any,
    Exact(String),
    Suffix(String), // *.example.com
}

impl MatchPattern {
    pub fn parse(pattern: &str) -> Option<Self> {
        if pattern == "<all_urls>" {
            return Some(MatchPattern {
                raw: pattern.to_string(),
                scheme: SchemeMatch::Any,
                host: HostMatch::Any,
                path: "*".to_string(),
            });
        }

        let (scheme_str, rest) = pattern.split_once("://")?;
        let scheme = if scheme_str == "*" {
            SchemeMatch::Any
        } else {
            SchemeMatch::Exact(scheme_str.to_string())
        };

        let (host_str, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], rest[idx..].to_string()),
            None => (rest, "/*".to_string()),
        };

        let host = if host_str == "*" {
            HostMatch::Any
        } else if let Some(suffix) = host_str.strip_prefix("*.") {
            HostMatch::Suffix(suffix.to_string())
        } else {
            HostMatch::Exact(host_str.to_string())
        };

        Some(MatchPattern {
            raw: pattern.to_string(),
            scheme,
            host,
            path,
        })
    }

    pub fn matches_url(&self, url: &url::Url) -> bool {
        // Scheme check
        match &self.scheme {
            SchemeMatch::Any => {
                if url.scheme() != "http" && url.scheme() != "https" {
                    return false;
                }
            },
            SchemeMatch::Exact(s) => {
                if url.scheme() != s {
                    return false;
                }
            },
        }

        // Host check
        if let Some(host) = url.host_str() {
            match &self.host {
                HostMatch::Any => {},
                HostMatch::Exact(h) => {
                    if host != h {
                        return false;
                    }
                },
                HostMatch::Suffix(suffix) => {
                    if host != suffix.as_str() && !host.ends_with(&format!(".{}", suffix)) {
                        return false;
                    }
                },
            }
        } else {
            return false;
        }

        // Path check (simple glob: * matches anything)
        if self.path != "/*" && self.path != "*" {
            let url_path = url.path();
            if let Some(prefix) = self.path.strip_suffix('*') {
                if !url_path.starts_with(prefix) {
                    return false;
                }
            } else if url_path != self.path {
                return false;
            }
        }

        true
    }
}

/// Manages all loaded extensions.
pub struct ExtensionManager {
    extensions: HashMap<String, LoadedExtension>,
    extensions_dir: PathBuf,
}

impl ExtensionManager {
    pub fn new() -> Self {
        let extensions_dir = dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("servo-private")
            .join("extensions");
        std::fs::create_dir_all(&extensions_dir).ok();

        let mut mgr = ExtensionManager {
            extensions: HashMap::new(),
            extensions_dir,
        };
        mgr.load_all();
        mgr
    }

    /// Scan extensions directory and load all valid extensions.
    pub fn load_all(&mut self) {
        self.extensions.clear();

        let entries = match std::fs::read_dir(&self.extensions_dir) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Could not read extensions directory: {}", e);
                return;
            },
        };

        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }

            let id = entry
                .file_name()
                .to_string_lossy()
                .to_string();

            match std::fs::read_to_string(&manifest_path) {
                Ok(json) => match serde_json::from_str::<ExtensionManifest>(&json) {
                    Ok(manifest) => {
                        let mut js_sources = HashMap::new();
                        let mut css_sources = HashMap::new();

                        // Load content script files.
                        for cs in &manifest.content_scripts {
                            for js_file in &cs.js {
                                let js_path = entry.path().join(js_file);
                                if let Ok(src) = std::fs::read_to_string(&js_path) {
                                    js_sources.insert(js_file.clone(), src);
                                } else {
                                    warn!("Extension {}: could not load JS file {}", id, js_file);
                                }
                            }
                            for css_file in &cs.css {
                                let css_path = entry.path().join(css_file);
                                if let Ok(src) = std::fs::read_to_string(&css_path) {
                                    css_sources.insert(css_file.clone(), src);
                                } else {
                                    warn!(
                                        "Extension {}: could not load CSS file {}",
                                        id, css_file
                                    );
                                }
                            }
                        }

                        let enabled = browser_storage::is_extension_enabled(&id);

                        info!(
                            "Loaded extension: {} v{} ({}), enabled={}",
                            manifest.name, manifest.version, id, enabled
                        );

                        self.extensions.insert(
                            id.clone(),
                            LoadedExtension {
                                id,
                                manifest,
                                base_path: entry.path(),
                                js_sources,
                                css_sources,
                                enabled,
                            },
                        );
                    },
                    Err(e) => {
                        error!("Extension {}: invalid manifest.json: {}", id, e);
                    },
                },
                Err(e) => {
                    error!("Extension {}: could not read manifest.json: {}", id, e);
                },
            }
        }
    }

    /// Get all loaded extensions.
    pub fn extensions(&self) -> &HashMap<String, LoadedExtension> {
        &self.extensions
    }

    /// Get content scripts that match the given URL (from enabled extensions only).
    pub fn get_content_scripts_for_url(&self, url: &url::Url) -> Vec<(&LoadedExtension, &ContentScriptEntry)> {
        let mut result = Vec::new();
        for ext in self.extensions.values() {
            if !ext.enabled {
                continue;
            }
            for cs in &ext.manifest.content_scripts {
                for pattern_str in &cs.matches {
                    if let Some(pattern) = MatchPattern::parse(pattern_str) {
                        if pattern.matches_url(url) {
                            result.push((ext, cs));
                            break;
                        }
                    }
                }
            }
        }
        result
    }

    /// Build a combined injection script for the given URL.
    /// Returns None if no content scripts match.
    pub fn build_injection_script(&self, url: &url::Url) -> Option<String> {
        let matches = self.get_content_scripts_for_url(url);
        if matches.is_empty() {
            return None;
        }

        let mut script = String::new();

        // Add polyfill first.
        script.push_str(CHROME_API_POLYFILL);
        script.push('\n');

        // Inject CSS via style elements.
        for (ext, cs) in &matches {
            for css_file in &cs.css {
                if let Some(css_src) = ext.css_sources.get(css_file) {
                    let escaped = css_src
                        .replace('\\', "\\\\")
                        .replace('`', "\\`")
                        .replace("${", "\\${");
                    script.push_str(&format!(
                        "(function() {{ var s = document.createElement('style'); s.textContent = `{}`; document.head.appendChild(s); }})();\n",
                        escaped
                    ));
                }
            }
        }

        // Inject JS content scripts.
        for (ext, cs) in &matches {
            for js_file in &cs.js {
                if let Some(js_src) = ext.js_sources.get(js_file) {
                    script.push_str(&format!(
                        "// Extension: {} ({})\n(function() {{\n{}\n}})();\n",
                        ext.manifest.name, ext.id, js_src
                    ));
                }
            }
        }

        Some(script)
    }

    /// Enable or disable an extension.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        browser_storage::set_extension_enabled(id, enabled);
        if let Some(ext) = self.extensions.get_mut(id) {
            ext.enabled = enabled;
        }
    }

    /// Remove an extension from disk and database.
    pub fn remove_extension(&mut self, id: &str) {
        if let Some(ext) = self.extensions.remove(id) {
            if let Err(e) = std::fs::remove_dir_all(&ext.base_path) {
                error!("Failed to remove extension directory: {}", e);
            }
        }
        browser_storage::remove_extension_data(id);
    }

    /// Get the number of enabled extensions.
    pub fn enabled_count(&self) -> usize {
        self.extensions.values().filter(|e| e.enabled).count()
    }

    /// Get the extensions directory path.
    pub fn extensions_dir(&self) -> &PathBuf {
        &self.extensions_dir
    }
}

/// JavaScript polyfill providing chrome.runtime and chrome.storage.local APIs.
/// Uses fetch() to servo:ext-api/* routes (GET with query params) for browser communication.
const CHROME_API_POLYFILL: &str = r#"
(function() {
    if (window.chrome && window.chrome.runtime) return;

    window.chrome = window.chrome || {};
    window.browser = window.browser || {};

    chrome.runtime = {
        id: 'servo-extension',
        getURL: function(path) {
            return 'servo:ext-res/' + path;
        },
        sendMessage: function(msg, callback) {
            if (callback) callback(undefined);
        },
        onMessage: {
            addListener: function() {},
            removeListener: function() {},
        },
    };

    chrome.storage = {
        local: {
            get: function(keys, callback) {
                var keyList = typeof keys === 'string' ? [keys] : (Array.isArray(keys) ? keys : Object.keys(keys || {}));
                var url = 'servo:ext-api/storage/get?keys=' + encodeURIComponent(keyList.join(','));
                fetch(url)
                .then(function(r) { return r.json(); })
                .then(function(data) { if (callback) callback(data); })
                .catch(function() { if (callback) callback({}); });
            },
            set: function(items, callback) {
                var promises = Object.keys(items).map(function(key) {
                    return fetch('servo:ext-api/storage/set?key=' + encodeURIComponent(key) + '&value=' + encodeURIComponent(JSON.stringify(items[key])));
                });
                Promise.all(promises)
                .then(function() { if (callback) callback(); })
                .catch(function() { if (callback) callback(); });
            },
            remove: function(keys, callback) {
                var keyList = typeof keys === 'string' ? [keys] : keys;
                fetch('servo:ext-api/storage/remove?keys=' + encodeURIComponent(keyList.join(',')))
                .then(function() { if (callback) callback(); })
                .catch(function() { if (callback) callback(); });
            },
            clear: function(callback) {
                fetch('servo:ext-api/storage/clear')
                .then(function() { if (callback) callback(); })
                .catch(function() { if (callback) callback(); });
            },
            onChanged: {
                addListener: function() {},
                removeListener: function() {},
            },
        },
    };

    // Mirror to browser.* for WebExtension compatibility.
    browser.runtime = chrome.runtime;
    browser.storage = chrome.storage;
})();
"#;
