# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.10](https://github.com/Amanieu/thread_local-rs/compare/v1.1.9...v1.1.10) - 2026-02-18

### Other

- Switch to `thread::scope` for example
- Fix integer underflow in `RawIter::size_hint`
- Fix warning
- Fix MSRV build
- Shrink unsafe block size and extra empty lines
- Formatting
- Update safety comments
- Formatting
- Apply suggestions from code review
- Newline
- Don't ignore Cargo.lock
- Review feedback
- Merge branch 'master' into cleanup
- Remove unnecessary needs_drop check
- Fail CI if Clippy raises any warnings
- Revert changes to allocation.
- Gratuitous safety comments
- Start writing safety comments
- Use MaybeUninit::asssume_init_{mut, ref}
- Use the lastest Rust stable for non-MSRV CI
- Don't change the criterion version
- Fix benches' Cargo.toml
- Run benches in the new crate
- Remove extra all()
- Separate benches into it own crate to keep it isolated from MSRV
- Slim down CI jobs
- Fix formatting
- Set up CI job for testing agaisnt the MSRV
- MSRV-based cleanup
- do not register a thread-local destructor in ThreadLocal::get
- Update cached.rs
- Update lib.rs

## [1.1.9](https://github.com/Amanieu/thread_local-rs/compare/v1.1.8...v1.1.9) - 2025-06-12

### Other

- Add release-plz for automated releases
- Remove once_cell dependency
- Bump MSRV to 1.61
