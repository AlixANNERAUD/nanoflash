# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0](https://github.com/AlixANNERAUD/nanoflash/releases/tag/nanoflash-cli-v0.1.0) - 2026-11-23

### Added

- add layout check and layout write to the command line

### Changed

- parse arguments with getargs-derive

### Fixed

- exit 1 with JSON on argument errors, survive a closed pipe, reject image directories

### Miscellaneous

- set up the workspace, tooling and continuous integration
- give the nanoflash dependency of the command a version
