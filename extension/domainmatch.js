/*
 * domainmatch.js — pure domain-matching helpers.
 *
 * Loaded as a plain (non-module) script by the extension background page and
 * the popup, and also usable from Node for unit tests.
 *
 * Matching semantics (same as the README example):
 *   "kit.edu"   -> matches exactly "kit.edu" and every subdomain (*.kit.edu)
 *   "*.kit.edu" -> identical to "kit.edu"
 */
(function () {
  "use strict";

  /**
   * Normalize a user-supplied domain pattern to its bare domain form.
   * Returns null when the pattern is not usable.
   *   "*.kit.edu"      -> "kit.edu"
   *   "https://KIT.edu:443/x" -> "kit.edu"
   */
  function kitNormalizeDomain(pattern) {
    if (typeof pattern !== "string") return null;
    let p = pattern.trim().toLowerCase();
    if (!p) return null;
    p = p.replace(/^[a-z][a-z0-9+.-]*:\/\//, ""); // strip scheme
    if (p.startsWith("*.")) p = p.slice(2);        // strip wildcard marker
    p = p.split("/")[0];                           // strip path
    const at = p.indexOf("@");
    if (at >= 0) p = p.slice(at + 1);              // strip userinfo
    const colon = p.lastIndexOf(":");
    if (colon >= 0) p = p.slice(0, colon);         // strip port
    if (p.startsWith("[")) return null;            // IPv6 literals unsupported
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/.test(p)) return null;
    return p;
  }

  /** Lowercased hostname of a URL, or "" when parsing fails. */
  function kitHostname(url) {
    try {
      return new URL(url).hostname.toLowerCase();
    } catch (e) {
      return "";
    }
  }

  /**
   * True when `hostname` matches any of the configured domain patterns.
   * A pattern "example.org" matches "example.org" and "www.example.org".
   */
  function kitMatches(hostname, domains) {
    if (!hostname) return false;
    const h = hostname.toLowerCase();
    for (const pat of domains || []) {
      const base = kitNormalizeDomain(pat);
      if (!base) continue;
      if (h === base || h.endsWith("." + base)) return true;
    }
    return false;
  }

  const api = {
    kitNormalizeDomain,
    kitHostname,
    kitMatches,
  };

  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
  if (typeof globalThis !== "undefined") {
    globalThis.kitNormalizeDomain = api.kitNormalizeDomain;
    globalThis.kitHostname = api.kitHostname;
    globalThis.kitMatches = api.kitMatches;
  }
})();
