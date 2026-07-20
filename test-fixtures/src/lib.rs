//! Safe, bounded Linux fixtures for TaskCage integration tests.
//!
//! The Ghost Process, Memory Hog and Safe Fork Storm binaries are added during
//! the first two MVP milestones. Every fixture must enforce its own safety cap
//! so an accidental uncaged run cannot exhaust the development host.
