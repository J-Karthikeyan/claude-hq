//! Claude HQ's local server, ported from dashboard.py (and arena.py, music.py,
//! worksignals.py, ext_*.py). Same routes, payloads, state files and privacy
//! boundary; see README.md for what differs.

pub mod actions;
pub mod analytics;
pub mod app;
pub mod arena;
pub mod cost;
pub mod creature;
pub mod difflib;
pub mod evo;
pub mod ext;
pub mod fatigue;
pub mod handlers;
pub mod music;
pub mod notch;
pub mod payload;
pub mod py;
pub mod pyjson;
pub mod scan;
pub mod server;
pub mod state;
pub mod vial;
pub mod worksignals;
