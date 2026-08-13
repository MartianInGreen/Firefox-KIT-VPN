#!/usr/bin/env node
/* Unit tests for extension/domainmatch.js */
"use strict";

const { kitMatches, kitNormalizeDomain, kitHostname } = require("../extension/domainmatch.js");

let failures = 0;
function eq(actual, expected, label) {
  if (actual === expected) {
    console.log("  ok  " + label);
  } else {
    failures++;
    console.log("  FAIL " + label + " (got " + JSON.stringify(actual) + ", want " + JSON.stringify(expected) + ")");
  }
}

console.log("normalize");
eq(kitNormalizeDomain("*.kit.edu"), "kit.edu", "wildcard strip");
eq(kitNormalizeDomain("kit.edu"), "kit.edu", "plain");
eq(kitNormalizeDomain("https://KIT.edu:443/x"), "kit.edu", "scheme+port+path");
eq(kitNormalizeDomain(" *.scc.kit.edu "), "scc.kit.edu", "whitespace");
eq(kitNormalizeDomain("not a domain"), null, "invalid rejected");
eq(kitNormalizeDomain(""), null, "empty rejected");

console.log("hostname extraction");
eq(kitHostname("https://www.kit.edu/a/b"), "www.kit.edu", "simple");
eq(kitHostname("http://KIT.EDU"), "kit.edu", "case folded");
eq(kitHostname("garbage"), "", "unparsable -> empty");

console.log("matching (example from the spec)");
eq(kitMatches("www.kit.edu", ["*.kit.edu"]), true, "www.kit.edu matches *.kit.edu");
eq(kitMatches("kit.edu", ["*.kit.edu"]), true, "apex kit.edu matches *.kit.edu");
eq(kitMatches("scc.kit.edu", ["*.kit.edu"]), true, "subdomain of kit.edu");
eq(kitMatches("deep.a.b.kit.edu", ["*.kit.edu"]), true, "deep subdomain");
eq(kitMatches("kit.edu.evil.com", ["*.kit.edu"]), false, "suffix attack rejected");
eq(kitMatches("notkit.edu", ["*.kit.edu"]), false, "prefix lookalike rejected");
eq(kitMatches("example.com", ["*.kit.edu"]), false, "unrelated domain");
eq(kitMatches("www.kit.edu", ["kit.edu"]), true, "plain pattern also matches subdomains");
eq(kitMatches("www.kit.edu", []), false, "empty domain list -> no match");
eq(kitMatches("", ["*.kit.edu"]), false, "empty hostname -> no match");

console.log("proxy decision equivalence (background logic)");
function decide(url, enabled, domains) {
  if (!enabled) return "direct";
  const h = kitHostname(url);
  if (h && kitMatches(h, domains)) return "socks";
  return "direct";
}
eq(decide("https://www.kit.edu/", true, ["*.kit.edu"]), "socks", "kit -> socks");
eq(decide("https://www.kit.edu/", false, ["*.kit.edu"]), "direct", "disabled -> direct");
eq(decide("https://example.com/", true, ["*.kit.edu"]), "direct", "non-kit -> direct");

if (failures) {
  console.log(`\n${failures} failure(s)`);
  process.exit(1);
}
console.log("\nall domain-match tests passed");
