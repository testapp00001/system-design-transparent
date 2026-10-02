//! Cross-site request forgery protection and security headers.
//!
//! SameSite=Lax cookies already stop browsers from attaching our session
//! cookie to cross-site POSTs. But voting is anonymous (no cookie at all), so
//! a malicious page could still make its visitors' browsers vote. We therefore
//! reject unsafe requests whose `Sec-Fetch-Site` / `Origin` headers show they
//! came from another site. Every modern browser sends at least one of them.

use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub async fn cross_origin_guard(req: Request, next: Next) -> Response {
    let unsafe_method = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if unsafe_method && is_cross_origin(req.headers()) {
        return (StatusCode::FORBIDDEN, "Cross-origin request blocked").into_response();
    }
    next.run(req).await
}

fn is_cross_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        // "same-origin" is us; "none" is a user typing a URL / bookmark.
        return !matches!(site, "same-origin" | "none");
    }
    // Older browsers: compare the Origin host with the Host header.
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    match (origin, host) {
        (Some("null"), _) => true,
        (Some(origin), Some(host)) => {
            let origin_host = origin.split_once("://").map(|(_, rest)| rest).unwrap_or(origin);
            !origin_host.eq_ignore_ascii_case(host)
        }
        // No Origin header: not a browser cross-site request (curl, tests).
        _ => false,
    }
}

/// PostgreSQL rejects NUL bytes in text values, so a `%00` anywhere in the URL
/// would otherwise surface as a 500 from deep inside a query. No legitimate
/// URL on this site contains one, so reject it up front.
pub async fn reject_nul_in_url(req: Request, next: Next) -> Response {
    let has_nul = |s: &str| s.contains("%00");
    if has_nul(req.uri().path()) || req.uri().query().is_some_and(has_nul) {
        return (StatusCode::BAD_REQUEST, "Bad request").into_response();
    }
    next.run(req).await
}

/// Adds conservative security headers to every response.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
             object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, HeaderValue::from_static(v));
        }
        m
    }

    #[test]
    fn fetch_metadata_wins() {
        assert!(!is_cross_origin(&h(&[("sec-fetch-site", "same-origin")])));
        assert!(is_cross_origin(&h(&[("sec-fetch-site", "cross-site")])));
        assert!(is_cross_origin(&h(&[("sec-fetch-site", "same-site")])));
    }

    #[test]
    fn falls_back_to_origin_vs_host() {
        assert!(!is_cross_origin(&h(&[("origin", "https://example.com"), ("host", "example.com")])));
        assert!(is_cross_origin(&h(&[("origin", "https://evil.com"), ("host", "example.com")])));
        assert!(is_cross_origin(&h(&[("origin", "null"), ("host", "example.com")])));
        assert!(!is_cross_origin(&h(&[("host", "example.com")])));
    }
}
