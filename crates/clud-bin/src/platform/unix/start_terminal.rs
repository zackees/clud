//! Host adapter for the Windows-only explicit console launcher.

pub fn bash_function() -> &'static str {
    ""
}

pub fn run() -> i32 {
    eprintln!("clud: opening Command Prompt is available only on Windows");
    2
}
