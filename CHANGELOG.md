# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.1](https://github.com/nickderobertis/onevcs/compare/v0.12.0...v0.12.1) - 2026-08-23

### Fixed

- make the judged tier replay one verdict per tree, base and judge configuration ([#70](https://github.com/nickderobertis/onevcs/pull/70))

## [0.12.0](https://github.com/nickderobertis/onevcs/compare/v0.11.1...v0.12.0) - 2026-08-23

### Fixed

- report a push that landed and a merge path that could not be read as what it is ([#71](https://github.com/nickderobertis/onevcs/pull/71))

## [0.11.1](https://github.com/nickderobertis/onevcs/compare/v0.11.0...v0.11.1) - 2026-08-23

### Fixed

- refuse to reap a session worktree holding commits its branch does not ([#68](https://github.com/nickderobertis/onevcs/pull/68))

## [0.11.0](https://github.com/nickderobertis/onevcs/compare/v0.10.0...v0.11.0) - 2026-08-21

### Added

- *(rules)* [**breaking**] make the repository's own merge path the only verifier ([#65](https://github.com/nickderobertis/onevcs/pull/65))

## [0.10.0](https://github.com/nickderobertis/onevcs/compare/v0.9.0...v0.10.0) - 2026-08-21

### Added

- *(publish)* evidence every publication failure, and watch an auto-merge to its end ([#63](https://github.com/nickderobertis/onevcs/pull/63))

## [0.9.0](https://github.com/nickderobertis/onevcs/compare/v0.8.1...v0.9.0) - 2026-08-20

### Added

- *(status)* [**breaking**] decide landing from history, and never resume a landed branch ([#61](https://github.com/nickderobertis/onevcs/pull/61))

## [0.8.1](https://github.com/nickderobertis/onevcs/compare/v0.8.0...v0.8.1) - 2026-08-20

### Added

- *(sweep)* reclaim the publication workspaces, and stop what they left running ([#59](https://github.com/nickderobertis/onevcs/pull/59))

## [0.8.0](https://github.com/nickderobertis/onevcs/compare/v0.7.0...v0.8.0) - 2026-08-20

### Added

- *(session)* continue a pinned branch that already exists ([#57](https://github.com/nickderobertis/onevcs/pull/57))

## [0.7.0](https://github.com/nickderobertis/onevcs/compare/v0.6.1...v0.7.0) - 2026-08-19

### Added

- *(publish)* carry a caller's body onto every branch-keyed landing ([#53](https://github.com/nickderobertis/onevcs/pull/53))

## [0.6.1](https://github.com/nickderobertis/onevcs/compare/v0.6.0...v0.6.1) - 2026-08-18

### Added

- *(publish)* put the composed subject to the repository's own commit-msg hook ([#51](https://github.com/nickderobertis/onevcs/pull/51))

## [0.6.0](https://github.com/nickderobertis/onevcs/compare/v0.5.0...v0.6.0) - 2026-08-17

### Fixed

- *(branch)* [**breaking**] refuse copies of a branch that no other copy carries ([#49](https://github.com/nickderobertis/onevcs/pull/49))

## [0.5.0](https://github.com/nickderobertis/onevcs/compare/v0.4.2...v0.5.0) - 2026-08-16

### Added

- report what became of a piece of work, and make a branch reachable ([#47](https://github.com/nickderobertis/onevcs/pull/47))

## [0.4.2](https://github.com/nickderobertis/onevcs/compare/v0.4.1...v0.4.2) - 2026-08-16

### Fixed

- *(workspace)* open at the real remote tip, and reuse a pinned branch's worktree ([#45](https://github.com/nickderobertis/onevcs/pull/45))

## [0.4.1](https://github.com/nickderobertis/onevcs/compare/v0.4.0...v0.4.1) - 2026-08-16

### Added

- *(publish)* replay a stacked branch, and lease the push it rewrote ([#43](https://github.com/nickderobertis/onevcs/pull/43))

## [0.4.0](https://github.com/nickderobertis/onevcs/compare/v0.3.1...v0.4.0) - 2026-08-16

### Added

- open a change request with the caller's own body ([#40](https://github.com/nickderobertis/onevcs/pull/40))

## [0.3.1](https://github.com/nickderobertis/onevcs/compare/v0.3.0...v0.3.1) - 2026-08-16

### Fixed

- find preserved work and refuse a pin you cannot honour ([#38](https://github.com/nickderobertis/onevcs/pull/38))

## [0.3.0](https://github.com/nickderobertis/onevcs/compare/v0.2.10...v0.3.0) - 2026-08-15

### Added

- publish a completed branch by name, and guide every refusal ([#36](https://github.com/nickderobertis/onevcs/pull/36))

## [0.2.10](https://github.com/nickderobertis/onevcs/compare/v0.2.9...v0.2.10) - 2026-08-15

### Fixed

- *(gate)* drain both gate pipes so a loud gate cannot deadlock ([#34](https://github.com/nickderobertis/onevcs/pull/34))

## [0.2.9](https://github.com/nickderobertis/onevcs/compare/v0.2.8...v0.2.9) - 2026-08-15

### Fixed

- *(github)* fetch a coloured check log; never store a failure as one ([#32](https://github.com/nickderobertis/onevcs/pull/32))

## [0.2.8](https://github.com/nickderobertis/onevcs/compare/v0.2.7...v0.2.8) - 2026-08-15

### Fixed

- *(publish)* refuse an empty-diff PR; raise the title limit to 120 ([#30](https://github.com/nickderobertis/onevcs/pull/30))

## [0.2.7](https://github.com/nickderobertis/onevcs/compare/v0.2.6...v0.2.7) - 2026-08-15

### Added

- filter a session's event stream ([#26](https://github.com/nickderobertis/onevcs/pull/26))

## [0.2.6](https://github.com/nickderobertis/onevcs/compare/v0.2.5...v0.2.6) - 2026-08-13

### Added

- expose workspace holder enumeration ([#24](https://github.com/nickderobertis/onevcs/pull/24))

## [0.2.5](https://github.com/nickderobertis/onevcs/compare/v0.2.4...v0.2.5) - 2026-08-12

### Fixed

- identify live session owner processes soundly ([#21](https://github.com/nickderobertis/onevcs/pull/21))

## [0.2.4](https://github.com/nickderobertis/onevcs/compare/v0.2.3...v0.2.4) - 2026-08-12

### Fixed

- simplify canonical Windows paths for Git ([#19](https://github.com/nickderobertis/onevcs/pull/19))

## [0.2.3](https://github.com/nickderobertis/onevcs/compare/v0.2.2...v0.2.3) - 2026-08-12

### Added

- report which live sessions hold a repository identity ([#16](https://github.com/nickderobertis/onevcs/pull/16))

## [0.2.2](https://github.com/nickderobertis/onevcs/compare/v0.2.1...v0.2.2) - 2026-08-12

### Fixed

- read check state through an API a fine-grained token can reach ([#15](https://github.com/nickderobertis/onevcs/pull/15))

## [0.2.1](https://github.com/nickderobertis/onevcs/compare/v0.2.0...v0.2.1) - 2026-08-11

### Added

- serve publish, close, and events through the provider seam ([#11](https://github.com/nickderobertis/onevcs/pull/11))

## [0.2.0](https://github.com/nickderobertis/onevcs/compare/v0.1.3...v0.2.0) - 2026-08-10

### Added

- [**breaking**] version the file-backed state and check its bytes in ([#9](https://github.com/nickderobertis/onevcs/pull/9))

## [0.1.3](https://github.com/nickderobertis/onevcs/compare/v0.1.2...v0.1.3) - 2026-08-09

### Added

- let a consumer choose the provenance trailer prefix ([#7](https://github.com/nickderobertis/onevcs/pull/7))

## [0.1.2](https://github.com/nickderobertis/onevcs/compare/v0.1.1...v0.1.2) - 2026-08-09

### Fixed

- *(release)* make the release archive resolve every file it names ([#5](https://github.com/nickderobertis/onevcs/pull/5))

## [0.1.1](https://github.com/nickderobertis/onevcs/compare/v0.1.0...v0.1.1) - 2026-08-08

### Added

- port e2e suite and implement the onevcs contract ([#1](https://github.com/nickderobertis/onevcs/pull/1))

## [0.1.0](https://github.com/nickderobertis/onevcs/releases/tag/v0.1.0) - 2026-08-08

### Added

- bootstrap onevcs as the approved contract, interface-only

### Documentation

- keep the subtree instructions to what is only true here

### Fixed

- hand the npm journeys a workspace root Node can resolve
- clear the llmlint tier's second pass on the new suite
- scope the check-field suppression to the field it sits on
- settle the second llmlint pass on prose and script output
- clear the llmlint tier's findings on the bootstrap
