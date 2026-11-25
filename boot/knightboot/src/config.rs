//! Minimal zero-dependency parser for `\EFI\KNIGHTOS\knightboot.toml`.

pub const DEFAULT_CONFIG_PATH: &str = "\\EFI\\KNIGHTOS\\knightboot.toml";

#[cfg(target_arch = "aarch64")]
pub const DEFAULT_KERNEL_PATH: &str = "\\EFI\\KNIGHTOS\\kernel-aarch64.elf";
#[cfg(not(target_arch = "aarch64"))]
pub const DEFAULT_KERNEL_PATH: &str = "\\EFI\\KNIGHTOS\\kernel-x86_64.elf";

#[cfg(target_arch = "aarch64")]
pub const DEFAULT_INITRD_PATH: &str = "\\EFI\\KNIGHTOS\\initrd-aarch64.kpkg";
#[cfg(not(target_arch = "aarch64"))]
pub const DEFAULT_INITRD_PATH: &str = "\\EFI\\KNIGHTOS\\initrd-x86_64.kpkg";

#[derive(Debug, Clone)]
pub struct BootConfig {
    pub slot: u32,
    pub kernel_path: [u8; 128],
    pub kernel_path_len: usize,
    pub initrd_path: [u8; 128],
    pub initrd_path_len: usize,
    pub cmdline: [u8; 256],
    pub cmdline_len: usize,
    pub kaslr: bool,
    pub fb_width: Option<u32>,
    pub fb_height: Option<u32>,
}

impl BootConfig {
    pub fn defaults() -> Self {
        let mut cfg = Self {
            slot: 0,
            kernel_path: [0; 128],
            kernel_path_len: 0,
            initrd_path: [0; 128],
            initrd_path_len: 0,
            cmdline: [0; 256],
            cmdline_len: 0,
            kaslr: true,
            fb_width: None,
            fb_height: None,
        };
        cfg.set_kernel_path(DEFAULT_KERNEL_PATH);
        cfg.set_initrd_path(DEFAULT_INITRD_PATH);
        cfg
    }

    pub fn kernel_path(&self) -> &str {
        core::str::from_utf8(&self.kernel_path[..self.kernel_path_len]).unwrap_or(DEFAULT_KERNEL_PATH)
    }

    pub fn initrd_path(&self) -> &str {
        core::str::from_utf8(&self.initrd_path[..self.initrd_path_len]).unwrap_or(DEFAULT_INITRD_PATH)
    }

    pub fn cmdline(&self) -> &str {
        core::str::from_utf8(&self.cmdline[..self.cmdline_len]).unwrap_or("")
    }

    fn set_kernel_path(&mut self, val: &str) {
        self.kernel_path_len = copy_unescaped(val, &mut self.kernel_path);
    }

    fn set_initrd_path(&mut self, val: &str) {
        self.initrd_path_len = copy_unescaped(val, &mut self.initrd_path);
    }

    fn set_cmdline(&mut self, val: &str) {
        self.cmdline_len = copy_unescaped(val, &mut self.cmdline);
    }

    /// Parses a UTF-8 TOML-like configuration file into [`BootConfig`].
    pub fn parse(src: &str) -> Self {
        let mut cfg = Self::defaults();
        for raw_line in src.lines() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let key = k.trim();
            let val = unquote(v.trim());
            match key {
                "slot" => {
                    if val.eq_ignore_ascii_case("b") || val == "1" {
                        cfg.slot = 1;
                    } else {
                        cfg.slot = 0;
                    }
                }
                "kernel" | "kernel_path" => {
                    if !val.is_empty() {
                        cfg.set_kernel_path(val);
                    }
                }
                "initrd" | "initrd_path" => {
                    if !val.is_empty() {
                        cfg.set_initrd_path(val);
                    }
                }
                "cmdline" => cfg.set_cmdline(val),
                "kaslr" => {
                    cfg.kaslr = val.eq_ignore_ascii_case("true") || val == "1";
                }
                "fb_width" => {
                    cfg.fb_width = val.parse::<u32>().ok().filter(|&x| x > 0);
                }
                "fb_height" => {
                    cfg.fb_height = val.parse::<u32>().ok().filter(|&x| x > 0);
                }
                _ => {}
            }
        }
        cfg
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (idx, ch) in line.char_indices() {
        if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch == '#' && !in_quotes {
            return &line[..idx];
        }
    }
    line
}

fn copy_unescaped(val: &str, dst: &mut [u8]) -> usize {
    let bytes = val.as_bytes();
    let mut i = 0usize;
    let mut out = 0usize;
    while i < bytes.len() && out < dst.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && (bytes[i + 1] == b'\\' || bytes[i + 1] == b'"') {
            dst[out] = bytes[i + 1];
            i += 2;
        } else {
            dst[out] = bytes[i];
            i += 1;
        }
        out += 1;
    }
    out
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_knightboot_toml() {
        let sample = r#"
            # KnightOS boot configuration
            slot = "B"
            kernel = "\\EFI\\KNIGHTOS\\kernel-test.elf"
            initrd = "\\EFI\\KNIGHTOS\\initrd-test.kpkg"
            cmdline = "console=serial debug=1 # keep hash inside quotes"
            kaslr = false
            fb_width = 1920
            fb_height = 1080
        "#;
        let cfg = BootConfig::parse(sample);
        assert_eq!(cfg.slot, 1);
        assert_eq!(cfg.kernel_path(), r"\EFI\KNIGHTOS\kernel-test.elf");
        assert_eq!(cfg.initrd_path(), r"\EFI\KNIGHTOS\initrd-test.kpkg");
        assert_eq!(cfg.cmdline(), "console=serial debug=1 # keep hash inside quotes");
        assert!(!cfg.kaslr);
        assert_eq!(cfg.fb_width, Some(1920));
        assert_eq!(cfg.fb_height, Some(1080));
    }
}
