//! Signal handler — sets up SIGTERM/SIGINT for graceful shutdown.
//!
//! Equivalent to the Windows `RegisterServiceCtrlHandlerExW` pattern.

use anyhow::Result;
use log::info;

/// Register signal handlers for graceful shutdown
pub fn setup_signal_handlers() -> Result<()> {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

    let handler = SigAction::new(
        SigHandler::Handler(handle_signal),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );

    unsafe {
        sigaction(Signal::SIGTERM, &handler)?;
        sigaction(Signal::SIGINT, &handler)?;
        sigaction(Signal::SIGQUIT, &handler)?;
    }

    // Ignore SIGPIPE (can happen with broken ALSA connections)
    unsafe {
        sigaction(
            Signal::SIGPIPE,
            &SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
        )?;
    }

    info!("Signal handlers registered (SIGTERM, SIGINT, SIGQUIT)");
    Ok(())
}

extern "C" fn handle_signal(sig: i32) {
    let name = match sig {
        nix::libc::SIGTERM => "SIGTERM",
        nix::libc::SIGINT => "SIGINT",
        nix::libc::SIGQUIT => "SIGQUIT",
        _ => "UNKNOWN",
    };
    log::info!("Received {}, initiating shutdown...", name);
    crate::SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
}
