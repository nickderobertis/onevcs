# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.4.23...onevcs-testing-v0.5.0) - 2026-08-30

### Added

- publish a change request as a draft carrying the reason it is not ready ([#119](https://github.com/nickderobertis/onevcs/pull/119))

## [0.4.9](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.4.8...onevcs-testing-v0.4.9) - 2026-08-25

### Fixed

- bind a merge-path verdict to the commit the publication just pushed ([#84](https://github.com/nickderobertis/onevcs/pull/84))

## [0.4.7](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.4.6...onevcs-testing-v0.4.7) - 2026-08-24

### Added

- *(events)* correlate release phases and retried sessions ([#80](https://github.com/nickderobertis/onevcs/pull/80))

## [0.4.0](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.3.1...onevcs-testing-v0.4.0) - 2026-08-21

### Added

- *(rules)* [**breaking**] make the repository's own merge path the only verifier ([#65](https://github.com/nickderobertis/onevcs/pull/65))

## [0.3.1](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.3.0...onevcs-testing-v0.3.1) - 2026-08-21

### Added

- *(publish)* evidence every publication failure, and watch an auto-merge to its end ([#63](https://github.com/nickderobertis/onevcs/pull/63))

## [0.3.0](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.2.8...onevcs-testing-v0.3.0) - 2026-08-20

### Added

- *(status)* [**breaking**] decide landing from history, and never resume a landed branch ([#61](https://github.com/nickderobertis/onevcs/pull/61))

## [0.2.7](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.2.6...onevcs-testing-v0.2.7) - 2026-08-20

### Added

- *(session)* continue a pinned branch that already exists ([#57](https://github.com/nickderobertis/onevcs/pull/57))

## [0.2.1](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.2.0...onevcs-testing-v0.2.1) - 2026-08-16

### Added

- *(publish)* replay a stacked branch, and lease the push it rewrote ([#43](https://github.com/nickderobertis/onevcs/pull/43))

## [0.2.0](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.1.12...onevcs-testing-v0.2.0) - 2026-08-16

### Added

- open a change request with the caller's own body ([#40](https://github.com/nickderobertis/onevcs/pull/40))

## [0.1.12](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.1.11...onevcs-testing-v0.1.12) - 2026-08-16

### Fixed

- find preserved work and refuse a pin you cannot honour ([#38](https://github.com/nickderobertis/onevcs/pull/38))

## [0.1.8](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.1.7...onevcs-testing-v0.1.8) - 2026-08-15

### Fixed

- *(publish)* refuse an empty-diff PR; raise the title limit to 120 ([#30](https://github.com/nickderobertis/onevcs/pull/30))

## [0.1.3](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.1.2...onevcs-testing-v0.1.3) - 2026-08-12

### Added

- report which live sessions hold a repository identity ([#16](https://github.com/nickderobertis/onevcs/pull/16))

## [0.1.2](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.1.1...onevcs-testing-v0.1.2) - 2026-08-12

### Fixed

- read check state through an API a fine-grained token can reach ([#15](https://github.com/nickderobertis/onevcs/pull/15))

## [0.1.1](https://github.com/nickderobertis/onevcs/compare/onevcs-testing-v0.1.0...onevcs-testing-v0.1.1) - 2026-08-11

### Added

- serve publish, close, and events through the provider seam ([#11](https://github.com/nickderobertis/onevcs/pull/11))

## [0.1.0](https://github.com/nickderobertis/onevcs/releases/tag/onevcs-testing-v0.1.0) - 2026-08-10

### Added

- [**breaking**] version the file-backed state and check its bytes in ([#9](https://github.com/nickderobertis/onevcs/pull/9))
