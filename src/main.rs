use std::io::Write;

use failure::format_err;

use octobot::config;
use octobot::server;
use octobot::errors::*;

fn main() {
    if let Err(ref e) = run() {
        let stderr = &mut ::std::io::stderr();
        let errmsg = "Error writing to stderr";

        writeln!(stderr, "error: {}", e).expect(errmsg);

        for cause in e.iter_causes() {
            writeln!(stderr, "{}: {}", cause.name().unwrap_or("Error"), cause).expect(errmsg);
        }

        // The backtrace is not always generated. Try to run this example
        // with `RUST_BACKTRACE=1`.
        writeln!(stderr, "backtrace: {:?}", e.backtrace()).expect(errmsg);

        ::std::process::exit(1);
    }
}

fn run() -> Result<()> {
    if std::env::args().len() < 2 {
        return Err(format_err!("Usage: octobot <config-file>"));
    }

    setup_logging();

    if let Ok(mut path) = std::env::current_exe() {
        path.pop();
        path.push("version");

        if let Ok(version) = std::fs::read_to_string(path) {
            log::info!("Starting octobot version [{}]", version.trim());
        }
    }

    let config_file = std::env::args().nth(1).unwrap();

    let config = config::new(config_file.into()).map_err(|e| format_err!("Error parsing config: {}", e))?;

    server::main::start(config);

    Ok(())
}

fn setup_logging() {
    let formatter = |buf: &mut env_logger::fmt::Formatter, record: &log::Record| {
        let t = time::now();
        write!(
            buf,
            "[{},{:03}][{}:{}] - {} - {}\n",
            time::strftime("%Y-%m-%d %H:%M:%S", &t).unwrap(),
            t.tm_nsec / 1000_000,
            thread_id::get(),
            std::thread::current().name().unwrap_or(""),
            record.level(),
            record.args()
        )
    };

    let mut builder = env_logger::Builder::from_default_env();
    builder.format(formatter).filter(None, log::LevelFilter::Info);

    let is_info;
    if let Ok(ref env_log) = std::env::var("RUST_LOG") {
        is_info = env_log.is_empty() || env_log.to_lowercase() == "info";
    } else {
        is_info = true;
    }

    if is_info {
        builder.filter(Some("rustls"), log::LevelFilter::Warn);
    }

    builder.init();
}
