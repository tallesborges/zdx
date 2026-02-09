mod cli;
mod modes;

use zdx_core::core::interrupt;

fn main() {
    if let Err(e) = cli::run() {
        if e.downcast_ref::<interrupt::InterruptedError>().is_some() {
            std::process::exit(130);
        }
        eprintln!("{e:#}"); // pretty anyhow chain
        std::process::exit(1);
    }
}
