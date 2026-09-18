//! The Omnia command line.
//!
//! # What this is, and what it is not
//!
//! The premise of the system is that you never need to know a command. `omni`
//! is a command, so the honest version is narrower: you never need to know the
//! four hundred commands Linux otherwise demands — `tar`, `systemctl`,
//! `iptables`, `lvm`, `dd` — or their flags. You say what you want.
//!
//! `omni` is the escape hatch, the inspection tool, and the thing scripts call.
//! The front door is the shell handler and the desktop overlay, which arrive
//! with the autonomous daemon. Everything they will do goes through here.
//!
//! # Exit codes are interface
//!
//! The shell integration branches on them, so they come from
//! `omnia_core::error::Error::exit_code` and are fixed. In particular a caller
//! must be able to tell "the model backend is down" (5) from "nothing survived
//! validation" (8), because the first is worth waiting on and the second is
//! not.

#![forbid(unsafe_op_in_unsafe_fn)]

mod args;
mod ask;
mod context;
mod devices;
mod doctor;
mod list;
mod render;

use args::{Command, Invocation};
use context::Context;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let invocation = match args::parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("omni: {message}");
            return 2;
        }
    };

    // Before anything else, so a parse-level request for help works on a
    // machine whose configuration is broken.
    match invocation.command {
        Command::Help => {
            print!("{}", args::HELP);
            return 0;
        }
        Command::Version => {
            println!("omni {VERSION}");
            return 0;
        }
        _ => {}
    }

    if invocation.verbose {
        std::env::set_var("OMNIA_GENERAL_LOG_LEVEL", "debug");
    }

    match dispatch(invocation) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("omni: {error}");
            error.exit_code()
        }
    }
}

fn dispatch(invocation: Invocation) -> omnia_core::Result<i32> {
    let mut context = Context::load()?;
    let json = invocation.json;

    match invocation.command {
        Command::Inbox => list::inbox(&context, json),
        Command::Capabilities => list::capabilities(&context, json),
        Command::Show { ref name } => list::show(name, &context, json),
        Command::Parts => list::parts(&context, json),
        Command::Devices => devices::survey(&context, json),
        Command::Fix { ref name, dry_run } => devices::fix(name, &context, json, dry_run),
        Command::Doctor { offline } => doctor::run(&context, offline, json),
        Command::Ask(ref ask) => ask::run(ask, &mut context, json),
        // Both are handled in `run`, before any configuration is loaded.
        Command::Help | Command::Version => unreachable!("handled before dispatch"),
    }
}
