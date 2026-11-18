#[cfg(windows)]
mod registry;
#[cfg(windows)]
mod terminal;
#[cfg(windows)]
mod ui;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some(arg), None) if arg == "--version" => {
            println!("ee {}", env!("CARGO_PKG_VERSION"));
        }
        (Some(arg), None) if arg == "--help" || arg == "-h" => {
            println!("ee — Windows environment variable editor\n\nee             Open the TUI\nee --version   Print version\n\nChanges apply to new processes launched from a refreshed environment.");
        }
        (None, None) => {
            #[cfg(windows)]
            if let Err(error) = ui::run() {
                eprintln!("ee: {error}");
                return std::process::ExitCode::FAILURE;
            }
            #[cfg(not(windows))]
            {
                eprintln!("ee requires Windows");
                return std::process::ExitCode::FAILURE;
            }
        }
        _ => {
            eprintln!("Usage: ee [--help | --version]");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}
