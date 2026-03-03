//! Pelagos - A modern container runtime library for Linux.
//!
//! This library provides tools for creating and managing lightweight containers
//! using Linux namespaces.

#[cfg(not(target_os = "linux"))]
compile_error!(
    "pelagos only supports Linux (requires kernel namespaces, cgroups v2, and seccomp-BPF)"
);

pub mod build;
pub mod cgroup;
pub mod cgroup_rootless;
pub mod compose;
pub mod container;
pub mod dns;
pub mod idmap;
pub mod image;
pub mod landlock;
pub mod lisp;
pub mod network;
pub mod notif;
pub mod oci;
pub mod paths;
pub mod pty;
pub mod seccomp;
pub mod sexpr;
