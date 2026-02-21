/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Loads resources using a mapping from well-known shortcuts to resource: urls.
//! Recognized shortcuts:
//! - servo:default-user-agent
//! - servo:experimental-preferences
//! - servo:config
//! - servo:help
//! - servo:newtab
//! - servo:preferences

use std::future::Future;
use std::pin::Pin;

use headers::{ContentType, HeaderMapExt};
use servo::UserAgentPlatform;
use servo::protocol_handler::{
    DoneChannel, FetchContext, NetworkError, ProtocolHandler, Request, ResourceFetchTiming,
    Response, ResponseBody,
};

use crate::desktop::browser_storage;
use crate::desktop::protocols::resource::ResourceProtocolHandler;
use crate::prefs::EXPERIMENTAL_PREFS;

#[derive(Default)]
pub struct ServoProtocolHandler {}

impl ProtocolHandler for ServoProtocolHandler {
    fn privileged_paths(&self) -> &'static [&'static str] {
        &["config", "preferences"]
    }

    fn is_fetchable(&self) -> bool {
        true
    }

    fn load(
        &self,
        request: &mut Request,
        done_chan: &mut DoneChannel,
        context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let url = request.current_url();

        match url.path() {
            "config" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/config.html",
            ),
            "newtab" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/newtab.html",
            ),
            "preferences" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/preferences.html",
            ),
            "license" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/license.html",
            ),
            "test-battery" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/test-battery.html",
            ),
            "vpn-guide" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/vpn-guide.html",
            ),
            "help" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/help.html",
            ),

            "experimental-preferences" => {
                let pref_list = EXPERIMENTAL_PREFS
                    .iter()
                    .map(|pref| format!("\"{pref}\""))
                    .collect::<Vec<String>>()
                    .join(",");
                json_response(request, format!("[{pref_list}]"))
            },

            "default-user-agent" => {
                let user_agent = UserAgentPlatform::default().to_user_agent_string();
                json_response(request, format!("\"{user_agent}\""))
            },

            "extensions" => ResourceProtocolHandler::response_for_path(
                request,
                done_chan,
                context,
                "/extensions.html",
            ),

            "extensions-data" => {
                let extensions_dir = dirs::config_dir()
                    .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
                    .join("servo-private")
                    .join("extensions");

                let mut ext_list = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&extensions_dir) {
                    for entry in entries.flatten() {
                        if !entry.path().is_dir() {
                            continue;
                        }
                        let manifest_path = entry.path().join("manifest.json");
                        if !manifest_path.exists() {
                            continue;
                        }
                        let id = entry.file_name().to_string_lossy().to_string();
                        let enabled = browser_storage::is_extension_enabled(&id);
                        if let Ok(json) = std::fs::read_to_string(&manifest_path) {
                            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&json) {
                                let name = manifest.get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&id)
                                    .to_string();
                                let version = manifest.get("version")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("0.0.0")
                                    .to_string();
                                let description = manifest.get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                ext_list.push(serde_json::json!({
                                    "id": id,
                                    "name": name,
                                    "version": version,
                                    "description": description,
                                    "enabled": enabled,
                                }));
                            }
                        }
                    }
                }
                json_response(request, serde_json::Value::Array(ext_list).to_string())
            },

            path if path.starts_with("ext-api/") => {
                handle_ext_api(request, path)
            },

            _ => Box::pin(std::future::ready(Response::network_error(
                NetworkError::ResourceLoadError("Invalid shortcut".to_owned()),
            ))),
        }
    }
}

fn handle_ext_api(
    request: &Request,
    path: &str,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    let url = request.current_url();
    let query_pairs: std::collections::HashMap<String, String> =
        url.as_url().query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect();

    // Default extension id — in a real implementation, this would come from the
    // extension context. For now, use the 'ext' query param or "default".
    let ext_id = query_pairs
        .get("ext")
        .cloned()
        .unwrap_or_else(|| "default".to_string());

    let sub_path = path.strip_prefix("ext-api/").unwrap_or("");

    match sub_path {
        "storage/get" => {
            let keys_str = query_pairs.get("keys").cloned().unwrap_or_default();
            let keys: Vec<String> = keys_str
                .split(',')
                .filter(|s: &&str| !s.is_empty())
                .map(|s: &str| s.to_string())
                .collect();
            let values = browser_storage::extension_storage_get_keys(&ext_id, &keys);
            let json = serde_json::to_string(&values).unwrap_or_else(|_| "{}".to_string());
            json_response(request, json)
        },
        "storage/set" => {
            let key = query_pairs.get("key").cloned().unwrap_or_default();
            let value = query_pairs.get("value").cloned().unwrap_or_default();
            if !key.is_empty() {
                browser_storage::extension_storage_set(&ext_id, &key, &value);
            }
            json_response(request, r#"{"ok":true}"#.to_string())
        },
        "storage/remove" => {
            let keys_str = query_pairs.get("keys").cloned().unwrap_or_default();
            for key in keys_str.split(',').filter(|s: &&str| !s.is_empty()) {
                browser_storage::extension_storage_remove(&ext_id, key);
            }
            json_response(request, r#"{"ok":true}"#.to_string())
        },
        "storage/clear" => {
            browser_storage::extension_storage_clear(&ext_id);
            json_response(request, r#"{"ok":true}"#.to_string())
        },
        _ => Box::pin(std::future::ready(Response::network_error(
            NetworkError::ResourceLoadError("Unknown ext-api endpoint".to_owned()),
        ))),
    }
}

fn json_response(
    request: &Request,
    body: String,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    let mut response = Response::new(
        request.current_url(),
        ResourceFetchTiming::new(request.timing_type()),
    );
    response.headers.typed_insert(ContentType::json());
    *response.body.lock() = ResponseBody::Done(body.into_bytes());
    Box::pin(std::future::ready(response))
}
