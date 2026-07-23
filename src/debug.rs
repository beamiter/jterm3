use std::sync::OnceLock;

// Only called from the debug_log! macro, which compiles to a no-op in release builds.
#[allow(dead_code)]
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();

    *ENABLED.get_or_init(|| {
        std::env::var_os("JTERM3_DEBUG")
            .map(|value| value != "0")
            .unwrap_or(false)
    })
}

#[allow(dead_code)]
pub fn format_bytes(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 96;

    let mut out = String::new();
    let preview = if bytes.len() > MAX_BYTES {
        &bytes[..MAX_BYTES]
    } else {
        bytes
    };

    for &byte in preview {
        match byte {
            b'\x1b' => out.push_str("<ESC>"),
            b'\r' => out.push_str("<CR>"),
            b'\n' => out.push_str("<LF>"),
            b'\t' => out.push_str("<TAB>"),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("<0x{byte:02x}>")),
        }
    }

    if bytes.len() > MAX_BYTES {
        out.push_str(&format!("...(+{} bytes)", bytes.len() - MAX_BYTES));
    }

    out
}

#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            if $crate::debug::enabled() {
                eprintln!($($arg)*);
            }
        }
    };
}
