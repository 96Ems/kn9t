# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added
- Initial open source release
- Core agent loop with event sourcing
- Plugin system with NdJSON protocol
- TUI with Lua-based layout
- Default tools plugin (bash, read, write, edit)

### Architecture
- Schema-first API design
- OS threads only (no async)
- Single vocabulary crate (`kn9t-core`)
- HTTP-only TUI (no direct core imports)

## [0.1.0] - 2024-XX-XX

Initial release.
