use std::io::Write;

#[expect(dead_code)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

#[doc(hidden)]
pub fn log_internal(level: Level, message: std::fmt::Arguments<'_>) {
    let prefix = match level {
        Level::Debug => "\x1b[36mdebug\x1b[0m",
        Level::Info => "\x1b[34minfo \x1b[0m",
        Level::Warn => "\x1b[33mwarn \x1b[0m",
        Level::Error => "\x1b[1;31merror\x1b[0m",
    };

    let mut output = std::io::stderr().lock();
    let now = chrono::Local::now();
    let time = now.format("%F %X");

    for line in message.to_string().lines() {
        writeln!(output, "[{prefix} {time}] {line}").unwrap();
    }
}

#[macro_export]
macro_rules! log {
    ($level: ident, $($args: tt)*) => {
        $crate::log::log_internal($crate::log::Level::$level, format_args!($($args)*))
    };
}

#[macro_export]
macro_rules! debug {
    ($($args: tt)*) => { log!(Debug, $($args)*) };
}

#[macro_export]
macro_rules! info {
    ($($args: tt)*) => { log!(Info, $($args)*) };
}

#[macro_export]
macro_rules! warn {
    ($($args: tt)*) => { log!(Warn, $($args)*) };
}

#[macro_export]
macro_rules! error {
    ($($args: tt)*) => { log!(Error, $($args)*) };
}
