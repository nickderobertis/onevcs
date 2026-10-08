# Exposure audit coverage manifest

Written by `onevcs-exposure-audit run` at 2026-10-08T14:29:39.873366594Z. It states what the audit read, not what it matched: that is kept only in the host-local vault.

## Scope

- Listing query: owner `nickderobertis`, visibility public, pushed on or after 2026-01-01, forks included.
- Listing returned 165 public repositories (165 by the host's own total) over 2 page(s) of at most 100.
- Pushed on or after the cutoff: 35; pushed before it, excluded: 130.
- Expected count supplied: 35; difference from it: +0.
- Visibility re-read: 35 confirmed public; dropped as not public: 0; as unknown: 0; as unreadable: 0.
- Allowlist: none.
- Board issue repositories added beyond the listing: 0.
- Mode: owner listing. Registered identities confirmed public: 17, of which in the listing: 17.
- Repositories audited: 35.

## Status vocabulary

- `scanned`: every read the surface needs succeeded.
- `not-found`: the surface does not exist for the row (an empty repository, issues turned off, a board's git history, a repository that backs no board).
- `permission-denied`: the credential was refused or absent.
- `rate-limited`: the host refused for quota.
- `other-error`: any other failed read.

## Repositories

| Repository | Current files | Git history | Refs read | Issues | Change requests | Board items | Edit history |
|---|---|---|---|---|---|---|---|
| nickderobertis/oneharness | scanned | scanned | heads 115, tags 173, pull 1331, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/ai-orchestrator | scanned | scanned | heads 215, tags 0, pull 15, other 0 | scanned | scanned | permission-denied | scanned |
| nickderobertis/onepipeline | scanned | scanned | heads 46, tags 160, pull 448, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/onetaskgraph | scanned | scanned | heads 5, tags 524, pull 203, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/crozier | scanned | scanned | heads 10, tags 109, pull 333, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/onepipeline-ui | scanned | scanned | heads 15, tags 56, pull 155, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/allowlister-terminal-approval-plugin | scanned | scanned | heads 1, tags 0, pull 6, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/github-graphql-node-count | scanned | scanned | heads 1, tags 2, pull 6, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/allowlister-remote | scanned | scanned | heads 8, tags 25, pull 111, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/nick-derobertis-site | scanned | scanned | heads 30, tags 9, pull 102, other 1 | scanned | scanned | not-found | scanned |
| nickderobertis/llmlint | scanned | scanned | heads 8, tags 81, pull 195, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/printobserver | scanned | scanned | heads 10, tags 3, pull 70, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/screencomp | scanned | scanned | heads 13, tags 33, pull 98, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/allowlister | scanned | scanned | heads 9, tags 39, pull 130, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/onevcs | scanned | scanned | heads 9, tags 188, pull 207, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/notignored | scanned | scanned | heads 20, tags 19, pull 62, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/oneharness-ui | scanned | scanned | heads 27, tags 28, pull 94, other 27 | scanned | scanned | not-found | scanned |
| nickderobertis/onetaskgraph-live-scratch | not-found | not-found | heads 0, tags 0, pull 0, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/skilltest | scanned | scanned | heads 15, tags 23, pull 42, other 22 | scanned | scanned | not-found | scanned |
| nickderobertis/oneagentgraph | scanned | scanned | heads 15, tags 59, pull 139, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/dero-skills | scanned | scanned | heads 5, tags 64, pull 87, other 64 | scanned | scanned | not-found | scanned |
| nickderobertis/onejudge | scanned | scanned | heads 3, tags 43, pull 113, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/github-secrets | scanned | scanned | heads 13, tags 9, pull 64, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/onemessagebus | scanned | scanned | heads 7, tags 17, pull 27, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/onebudgetspec | scanned | scanned | heads 2, tags 4, pull 15, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/screencomp-demo | scanned | scanned | heads 3, tags 0, pull 13, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/nick-derobertis-site-visual-docs | scanned | scanned | heads 2, tags 0, pull 0, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/spanish-language-tutor | scanned | scanned | heads 1, tags 0, pull 0, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/screencomp-pages-e2e | scanned | scanned | heads 2, tags 0, pull 0, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/asdf-prusaslicer | scanned | scanned | heads 1, tags 0, pull 6, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/asdf-allowlister | scanned | scanned | heads 1, tags 3, pull 10, other 2 | scanned | scanned | not-found | scanned |
| nickderobertis/cloud-agent-dev-env | scanned | scanned | heads 2, tags 0, pull 17, other 0 | scanned | scanned | not-found | scanned |
| nickderobertis/asdf-oneharness | scanned | scanned | heads 1, tags 1, pull 1, other 1 | scanned | scanned | not-found | scanned |
| nickderobertis/asdf-llmlint | scanned | scanned | heads 1, tags 1, pull 2, other 1 | scanned | scanned | not-found | scanned |
| nickderobertis/agents | scanned | scanned | heads 240, tags 351, pull 0, other 0 | not-found | scanned | not-found | scanned |

## Boards

| Board | Current files | Git history | Refs read | Issues | Change requests | Board items | Edit history |
|---|---|---|---|---|---|---|---|
| nickderobertis project 2 | not-found | not-found | none (a board has no git history) | permission-denied | permission-denied | permission-denied | permission-denied |
| nickderobertis project 3 | not-found | not-found | none (a board has no git history) | permission-denied | permission-denied | permission-denied | permission-denied |

