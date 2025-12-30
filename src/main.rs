mod app;
mod config;
mod core;
mod models;
mod providers;
mod tools;
mod ui;

use crate::core::interrupt;

fn main() {
    if let Err(e) = app::run() {
        if e.downcast_ref::<interrupt::InterruptedError>().is_some() {
            std::process::exit(130);
        }
        eprintln!("{:#}", e); // pretty anyhow chain
        std::process::exit(1);
    }
}
