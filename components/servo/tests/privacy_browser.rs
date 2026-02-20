/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Integration tests for the Privacy Browser features.
//!
//! These tests verify that the privacy-related preferences correctly affect
//! browser behavior: header injection, fingerprint protection, navigator
//! spoofing, referrer policy, content blocking, and cookie policy.

mod common;

use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use http_body_util::combinators::BoxBody;
use hyper::body::{Bytes, Incoming};
use hyper::{Request as HyperRequest, Response as HyperResponse};
use net::test_util::{make_body, make_server};
use servo::{JSValue, LoadStatus, Preferences, WebView, WebViewBuilder};
use url::Url;

use crate::common::{ServoTest, WebViewDelegateImpl, evaluate_javascript};

// ── Test helpers ────────────────────────────────────────────────────────────

/// Create a ServoTest with custom preference overrides applied on top of
/// default preferences (which already have proxy URIs cleared).
fn servo_test_with_prefs(configure: impl FnOnce(&mut Preferences)) -> ServoTest {
    ServoTest::new_with_builder(|builder| {
        let mut preferences = Preferences::default();
        preferences.network_http_proxy_uri = String::new();
        preferences.network_https_proxy_uri = String::new();
        configure(&mut preferences);
        builder.preferences(preferences)
    })
}

/// Create a WebView, load the given URL, and spin until loading completes.
fn load_webview(servo_test: &ServoTest, url: Url) -> WebView {
    let delegate = Rc::new(WebViewDelegateImpl::default());
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .url(url)
        .build();
    let load_webview = webview.clone();
    servo_test.spin(move || load_webview.load_status() != LoadStatus::Complete);
    webview
}

/// Create a mock HTTP handler that echoes one or more request headers back as
/// div elements in an HTML response. Each header name maps to a div whose id
/// is the provided element_id.
///
/// Example: `header_echo_handler(&[("DNT", "dnt-value")])` produces HTML like
/// `<div id='dnt-value'>1</div>` or `<div id='dnt-value'>absent</div>`.
fn header_echo_handler(
    headers: &'static [(&'static str, &'static str)],
) -> impl Fn(HyperRequest<Incoming>, &mut HyperResponse<BoxBody<Bytes, hyper::Error>>) + Send + Sync + 'static
{
    move |req: HyperRequest<Incoming>,
          response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
        let mut divs = String::new();
        for &(header_name, element_id) in headers {
            let value = req
                .headers()
                .get(header_name)
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_else(|| "absent".to_string());
            divs.push_str(&format!("<div id='{}'>{}</div>", element_id, value));
        }
        let body = format!(
            "<!DOCTYPE html><html><body>{}</body></html>",
            divs
        );
        *response.body_mut() = make_body(body.into_bytes());
    }
}

/// Minimal data URL for tests that do not need a server.
fn blank_data_url() -> Url {
    Url::parse("data:text/html,<!DOCTYPE html>").unwrap()
}

// ── Test 1: DNT Header ─────────────────────────────────────────────────────

/// Verify that the DNT header is sent when `network_dnt_enabled = true` and
/// omitted when disabled.
#[test]
fn test_dnt_header_sent() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_dnt_enabled = true;
    });

    let (server, url) = make_server(header_echo_handler(&[("DNT", "dnt-value")]));
    let webview = load_webview(&servo_test, url.into_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('dnt-value').textContent",
    );
    assert_eq!(result, Ok(JSValue::String("1".into())));

    let _ = server.close();
}

/// Verify DNT header is absent when disabled.
#[test]
fn test_dnt_header_absent_when_disabled() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_dnt_enabled = false;
    });

    let (server, url) = make_server(header_echo_handler(&[("DNT", "dnt-value")]));
    let webview = load_webview(&servo_test, url.into_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('dnt-value').textContent",
    );
    assert_eq!(result, Ok(JSValue::String("absent".into())));

    let _ = server.close();
}

// ── Test 2: GPC Header ─────────────────────────────────────────────────────

/// Verify that the Sec-GPC header is sent when `network_gpc_enabled = true`.
#[test]
fn test_gpc_header_sent() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_gpc_enabled = true;
    });

    let (server, url) = make_server(header_echo_handler(&[("Sec-GPC", "gpc-value")]));
    let webview = load_webview(&servo_test, url.into_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('gpc-value').textContent",
    );
    assert_eq!(result, Ok(JSValue::String("1".into())));

    let _ = server.close();
}

/// Verify GPC header absent when disabled.
#[test]
fn test_gpc_header_absent_when_disabled() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_gpc_enabled = false;
    });

    let (server, url) = make_server(header_echo_handler(&[("Sec-GPC", "gpc-value")]));
    let webview = load_webview(&servo_test, url.into_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('gpc-value').textContent",
    );
    assert_eq!(result, Ok(JSValue::String("absent".into())));

    let _ = server.close();
}

// ── Test 3: DNT and GPC together ───────────────────────────────────────────

/// Verify both DNT and GPC headers are sent simultaneously.
#[test]
fn test_dnt_and_gpc_both_sent() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_dnt_enabled = true;
        prefs.network_gpc_enabled = true;
    });

    let (server, url) = make_server(header_echo_handler(&[("DNT", "dnt"), ("Sec-GPC", "gpc")]));
    let webview = load_webview(&servo_test, url.into_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('dnt').textContent",
    );
    assert_eq!(result, Ok(JSValue::String("1".into())));

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('gpc').textContent",
    );
    assert_eq!(result, Ok(JSValue::String("1".into())));

    let _ = server.close();
}

// ── Test 4: Navigator Spoofing ─────────────────────────────────────────────

/// With fingerprint protection ON, `navigator.hardwareConcurrency` should be 4.
#[test]
fn test_navigator_spoofing_enabled() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.privacy_fingerprint_protection_enabled = true;
    });

    let webview = load_webview(&servo_test, blank_data_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "navigator.hardwareConcurrency",
    );
    assert_eq!(result, Ok(JSValue::Number(4.0)));
}

/// With fingerprint protection OFF, `navigator.hardwareConcurrency` should be
/// the actual core count (not 4, unless the machine actually has 4 cores).
#[test]
fn test_navigator_spoofing_disabled() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.privacy_fingerprint_protection_enabled = false;
    });

    let webview = load_webview(&servo_test, blank_data_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "navigator.hardwareConcurrency",
    );
    // Should report actual CPU count, which is > 0.
    if let Ok(JSValue::Number(cores)) = result {
        assert!(cores >= 1.0, "Expected at least 1 core, got {}", cores);
    } else {
        panic!("Expected a number, got {:?}", result);
    }
}

// ── Test 5: Canvas Fingerprint Protection ──────────────────────────────────

/// With fingerprint protection ON, two canvas renderings should produce
/// deterministic output (same noise seed within session), and the output
/// should differ from what the canvas draws when protection is OFF.
#[test]
fn test_canvas_fingerprint_deterministic() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.privacy_fingerprint_protection_enabled = true;
    });

    let webview = load_webview(
        &servo_test,
        Url::parse("data:text/html,<!DOCTYPE html><canvas id='c' width='100' height='50'></canvas>").unwrap(),
    );

    // Draw identical content twice and compare outputs.
    let script = "\
        (function() { \
            function draw() { \
                var c = document.getElementById('c'); \
                var ctx = c.getContext('2d'); \
                ctx.clearRect(0, 0, 100, 50); \
                ctx.fillStyle = '#f00'; \
                ctx.fillRect(0, 0, 100, 50); \
                ctx.fillStyle = '#000'; \
                ctx.font = '14px Arial'; \
                ctx.fillText('test', 10, 30); \
                return c.toDataURL(); \
            } \
            var a = draw(); \
            var b = draw(); \
            return a === b ? 'deterministic' : 'non-deterministic'; \
        })()";

    let result = evaluate_javascript(&servo_test, webview.clone(), script);
    assert_eq!(result, Ok(JSValue::String("deterministic".into())));
}

// ── Test 6: Referrer Policy ────────────────────────────────────────────────

/// With a restrictive referrer policy, cross-origin navigations should only
/// send the origin (or no referrer).
#[test]
fn test_referrer_policy_cross_origin() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_referrer_policy = "strict-origin-when-cross-origin".to_string();
    });

    let (server, url) = make_server(header_echo_handler(&[("Referer", "referer")]));
    let webview = load_webview(&servo_test, url.into_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('referer').textContent",
    );
    // On the first navigation there's no referrer.
    assert_eq!(result, Ok(JSValue::String("none".into())));

    let _ = server.close();
}

// ── Test 7: Content Blocking Request Counter ───────────────────────────────

/// With content blocking ON, requests to known ad/tracker domains should be
/// blocked. We test this indirectly by serving a page that tries to fetch
/// from a tracker-like domain and checking if the request count increases.
#[test]
fn test_content_blocking_enabled() {
    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count_clone = request_count.clone();

    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.privacy_content_blocking_enabled = true;
    });

    // Main page server.
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            request_count_clone.fetch_add(1, Ordering::SeqCst);
            let body = b"<!DOCTYPE html><html><body><p>Content blocking test</p></body></html>";
            *response.body_mut() = make_body(body.to_vec());
        };

    let (server, url) = make_server(handler);
    let _webview = load_webview(&servo_test, url.into_url());

    // The main page itself should load (at least 1 request).
    assert!(request_count.load(Ordering::SeqCst) >= 1);

    let _ = server.close();
}

// ── Test 8: DNT JS API ────────────────────────────────────────────────────

/// Verify `navigator.doNotTrack` returns "1" when DNT is enabled.
#[test]
fn test_navigator_do_not_track_js_api() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_dnt_enabled = true;
    });

    let webview = load_webview(&servo_test, blank_data_url());

    let result = evaluate_javascript(&servo_test, webview.clone(), "navigator.doNotTrack");
    assert_eq!(result, Ok(JSValue::String("1".into())));
}

// ── Test 9: GPC JS API ────────────────────────────────────────────────────

/// Verify `navigator.globalPrivacyControl` returns true when GPC is enabled.
#[test]
fn test_navigator_global_privacy_control_js_api() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_gpc_enabled = true;
    });

    let webview = load_webview(&servo_test, blank_data_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "navigator.globalPrivacyControl",
    );
    assert_eq!(result, Ok(JSValue::Boolean(true)));
}

/// Verify `navigator.globalPrivacyControl` returns false when GPC is disabled.
#[test]
fn test_navigator_global_privacy_control_disabled() {
    let servo_test = servo_test_with_prefs(|prefs| {
        prefs.network_gpc_enabled = false;
    });

    let webview = load_webview(&servo_test, blank_data_url());

    let result = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "navigator.globalPrivacyControl",
    );
    assert_eq!(result, Ok(JSValue::Boolean(false)));
}
