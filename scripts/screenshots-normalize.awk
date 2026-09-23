# Rewrite every per-run value in a captured scene to a fixed placeholder, so two
# captures of one build are byte-identical and screencomp can gate on the hash.
#
# `onevcs` has no clock override and no deterministic-id switch, and this repository
# does not add one for a screenshot's convenience (screenshots/AGENTS.md says why),
# so the normalization lives here — exactly as `llmlint`'s capture rewrites its three
# per-run paths. What varies per run is: the scratch host's own absolute path, the
# session tokens and artifact ids `onevcs` mints, the commit hashes (the provenance
# trailer carries the session token, so even a fixed-date merge hashes differently
# every run), the RFC 3339 timestamps on every event envelope, process ids, and
# elapsed times.
#
# Every family is mapped **in order of first appearance across the whole capture**
# rather than collapsed onto one value, so two different sessions still read as two
# different sessions and a shot keeps saying what it said. That is why every scene
# file is passed to one invocation: the map has to be shared across them.
#
# Usage: awk -f screenshots-normalize.awk -v root=<scratch host> <file>...
#        each <file> is rewritten to <file>.norm

function fail(message) {
  print "screenshots-normalize: " message > "/dev/stderr"
  exit 1
}

BEGIN {
  # POSIX interval expressions are what every rule below is built on, and an awk
  # that read `{40}` as four literal characters would quietly normalize nothing and
  # leave CI to report the drift as an output change. Refuse here instead.
  if ("0123456789abcdef0123456789abcdef01234567" !~ /^[0-9a-f]{40}$/)
    fail("this awk has no POSIX interval expressions ({40}); install gawk, or mawk >= 1.3.4")
  if (root == "") fail("-v root=<scratch host path> is required")
}

{
  line = $0
  # The scratch host's path first, and literally: every other rule reads what is
  # left, and a fixed home is what makes a shot legible at all.
  line = literal(line, root, "/home/dev")
  line = map_family(line, "(^|[^0-9A-Za-z])s-[0-9a-f]{12}", "session")
  line = map_family(line, "(^|[^0-9A-Za-z])a-[0-9a-f]{12}", "artifact")
  line = map_family(line, "[0-9a-f]{40}", "commit")
  line = map_family(line, "[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})", "stamp")
  line = map_family(line, "(pid |pid=)[0-9]+", "pid")
  gsub(/"elapsed":[0-9]+(\.[0-9]+)?/, "\"elapsed\":0.001", line)
  print line > (FILENAME ".norm")
}

# One literal (non-regex) replacement pass.
function literal(s, from, to,   out, at) {
  if (from == "") return s
  out = ""
  while ((at = index(s, from)) > 0) {
    out = out substr(s, 1, at - 1) to
    s = substr(s, at + length(from))
  }
  return out s
}

# Map every match of `pattern` onto a stable per-family placeholder, minting a new
# one the first time a value is seen anywhere in the capture.
function map_family(s, pattern, family,   out, found, prefix, value, key) {
  out = ""
  while (match(s, pattern)) {
    found = substr(s, RSTART, RLENGTH)
    # The leading boundary character these patterns need in order not to match
    # inside a longer word is part of the match, not of the value: keep it, and map
    # only what follows it.
    prefix = ""
    if (family == "session" || family == "artifact") {
      if (found !~ /^[sa]-/) { prefix = substr(found, 1, 1); found = substr(found, 2) }
    } else if (family == "pid") {
      prefix = (found ~ /^pid=/) ? "pid=" : "pid "
      found = substr(found, length(prefix) + 1)
    }
    key = family SUBSEP found
    if (!(key in seen)) seen[key] = ++minted[family]
    value = placeholder(family, seen[key])
    out = out substr(s, 1, RSTART - 1) prefix value
    s = substr(s, RSTART + RLENGTH)
  }
  return out s
}

# What each family's Nth distinct value becomes. Shaped like the real thing — a
# token still looks like a token and a hash like a hash — so the README shows the
# report `onevcs` actually prints rather than a redacted one.
function placeholder(family, n) {
  if (family == "session") return "s-" hex(n, 12, family)
  if (family == "artifact") return "a-" hex(n, 12, family)
  if (family == "commit") return hex(n, 40, family)
  if (family == "stamp") return sprintf("2026-03-02T09:15:%02d.000Z", (n - 1) % 60)
  if (family == "pid") return sprintf("%d", 40000 + n)
  fail("unknown placeholder family: " family)
}

# A width-long lowercase-hex string derived from `family` and `n` alone: the same
# inputs give the same digits on every host, and they look like the value they stand
# in for rather than like a run of zeroes.
function hex(n, width, family,   s, state, i) {
  s = ""
  # `family` seeds the sequence as well as `n`: without it the 12-digit token and
  # the 40-digit hash minted for the same ordinal would share a prefix, and a shot
  # would read as though a session token were the head of a commit.
  state = n * 2654435761 + length(family) * 7919 + index("scap", substr(family, 1, 1)) * 104729 + 97
  for (i = 0; i < width; i++) {
    state = (state * 1103515245 + 12345) % 2147483648
    s = s sprintf("%x", int(state / 65536) % 16)
  }
  return s
}
