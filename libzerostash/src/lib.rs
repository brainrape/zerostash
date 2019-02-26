#![deny(clippy::all)]
#![feature(test)]

#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate failure;

pub mod backends;
pub mod chunks;
pub mod compress;
pub mod crypto;
pub mod files;
pub mod meta;
pub mod objects;
pub mod stash;

pub mod rollsum;
pub mod splitter;

// Use block size of 4MiB for now
pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;