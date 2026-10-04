//! Embedded Web UI (iteration 10).
//!
//! A SINGLE self-contained HTML file — inline CSS, inline JS, zero external
//! resources (no CDN, no fonts, no build step, no npm). It is served by the
//! remote server at `/` when `--ui` is enabled and works in any modern
//! browser, including fully offline/sandboxed contexts.
//!
//! Security properties (THREAT_MODEL §E, §F):
//! * the HTML contains NO data and NO secrets — it is served without auth;
//!   every byte of repository data flows through the role-gated `/v1/*`
//!   endpoints with the user's own bearer token;
//! * the token lives in `sessionStorage` only (never localStorage/cookies);
//! * all dynamic content is rendered via `textContent`/`createTextNode`
//!   (the `el()` helper) — `innerHTML` is never used, so hostile commit
//!   messages/filenames cannot script the UI (XSS-safe by construction);
//! * the UI is a READ-ONLY explorer by design: mutations stay in the
//!   CLI/agent flow (no write endpoints exposed to the browser, which also
//!   makes CSRF structurally impossible — bearer header, not cookies).

/// The whole UI. Served verbatim as `text/html; charset=utf-8`.
pub const INDEX_HTML: &str = include_str!("index.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_is_self_contained_and_xss_disciplined() {
        let h = INDEX_HTML;
        // no external resource references of any kind
        for pat in [
            "http://",
            "https://",
            "//cdn",
            "<script src",
            "<link ",
            "@import",
            "url(",
            "innerHTML",
            "outerHTML",
            "document.write",
            "eval(",
        ] {
            assert!(
                !h.contains(pat),
                "UI must not contain {pat:?} (self-contained + XSS discipline)"
            );
        }
        // core sanity: it IS an html document with the app script
        assert!(h.starts_with("<!DOCTYPE html>"));
        assert!(h.contains("<script>"));
        assert!(h.contains("sessionStorage"));
        assert!(h.contains("/v1/info"));
        assert!(h.contains("X-NewGit-Protocol"));
    }
}
