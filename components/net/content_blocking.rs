/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Content blocking engine powered by adblock-rust (Brave's filter list engine).
//! Blocks ads, trackers, and other unwanted network requests using EasyList-compatible
//! filter syntax (Adblock Plus, uBlock Origin, Brave).

use std::cell::RefCell;

use adblock::Engine;
use adblock::lists::FilterSet;
use log::info;

thread_local! {
    /// Content blocking engine, lazily initialized per-thread.
    /// adblock::Engine is not Send+Sync (uses Rc/RefCell internally),
    /// so we use thread-local storage. Servo's network requests run
    /// on a dedicated IO thread, so this is both safe and efficient.
    static ENGINE: RefCell<Engine> = RefCell::new(create_engine());
}

fn create_engine() -> Engine {
    info!("Initializing content blocking engine");
    let mut filter_set = FilterSet::new(false);

    // Built-in minimal filter rules for common trackers.
    // Full filter lists (EasyList, EasyPrivacy) will be downloaded on first run.
    let builtin_filters = [
        // Google Analytics
        "||google-analytics.com^",
        "||googletagmanager.com^",
        // Facebook tracking
        "||connect.facebook.net^$third-party",
        "||pixel.facebook.com^",
        // Common ad networks
        "||doubleclick.net^",
        "||googlesyndication.com^",
        "||googleadservices.com^",
        "||adnxs.com^",
        "||ads.yahoo.com^",
        "||ads.twitter.com^",
        // Tracking pixels and beacons
        "||bat.bing.com^",
        "||analytics.tiktok.com^",
        "||t.co/i/adsct$third-party",
        // Common fingerprinting/tracking
        "||scorecardresearch.com^",
        "||quantserve.com^",
        "||hotjar.com^$third-party",
        "||amplitude.com^$third-party",
        "||mixpanel.com^$third-party",
        "||segment.io^$third-party",
        "||segment.com^$third-party",
        // Crypto miners
        "||coinhive.com^",
        "||coin-hive.com^",
        "||jsecoin.com^",
        "||cryptoloot.pro^",
    ];

    filter_set.add_filters(builtin_filters, Default::default());
    Engine::from_filter_set(filter_set, true)
}

/// Check whether a network request should be blocked.
///
/// # Arguments
/// * `url` - The URL being requested
/// * `source_url` - The URL of the page making the request (first-party context)
/// * `request_type` - The type of resource being requested (e.g., "script", "image", "stylesheet")
///
/// # Returns
/// `true` if the request should be blocked, `false` if it should be allowed.
pub fn should_block(url: &str, source_url: &str, request_type: &str) -> bool {
    let request = match adblock::request::Request::new(url, source_url, request_type) {
        Ok(req) => req,
        Err(_) => return false, // Can't parse URL — don't block
    };
    ENGINE.with(|engine| engine.borrow().check_network_request(&request).matched)
}
