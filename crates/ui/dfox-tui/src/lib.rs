//! The dfox terminal app, as a library so benchmarks and tests can reach its
//! parts. `main.rs` is the program.

pub mod action;
pub mod app;
pub mod catalog;
pub mod cli;
pub mod clipboard;
pub mod command;
pub mod component;
pub mod components;
pub mod config;
pub mod connections;
pub mod headless;
pub mod history;
pub mod icons;
pub mod keymap;
pub mod motion;
pub mod sql;
pub mod terminal;
#[cfg(test)]
mod testing;
pub mod theme;
pub mod update;
pub mod widgets;
