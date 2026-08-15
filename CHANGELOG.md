# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
