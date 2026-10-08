//! Starting programs without a window of their own.

use std::process::Command;

/// Windows gives a program started from an app with no console a console window of its own, for as long
/// as the program runs: one flash per git command, and dozens at once when a project opens. This asks for
/// none. Nothing changes on other systems.
pub fn windowless(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
