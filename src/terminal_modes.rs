// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::io::{self, Write};

const DISABLE_HOST_MOUSE_REPORTING_SEQUENCE: &[u8] =
    b"\x1b[?1006l\x1b[?1016l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

pub(crate) fn clear_host_mouse_reporting<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(DISABLE_HOST_MOUSE_REPORTING_SEQUENCE)?;
    writer.flush()
}

#[cfg(not(windows))]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    writer: &mut W,
    report_all_keys: bool,
) -> io::Result<()> {
    let mut flags = crate::input::ime_compatible_keyboard_enhancement_flags();
    if report_all_keys {
        flags |= crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    }
    write!(writer, "\x1b[={}u", flags.bits())?;
    writer.flush()
}

#[cfg(windows)]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    _writer: &mut W,
    _report_all_keys: bool,
) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        flushes: usize,
        fail_write: bool,
        fail_flush: bool,
    }

    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.fail_flush {
                return Err(io::ErrorKind::ConnectionReset.into());
            }
            Ok(())
        }
    }

    #[test]
    fn m810_reset_writes_exact_seven_modes_and_flushes() {
        let mut writer = Writer::default();
        clear_host_mouse_reporting(&mut writer).unwrap();
        assert_eq!(
            writer.bytes,
            b"\x1b[?1006l\x1b[?1016l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1000l"
        );
        assert_eq!(writer.flushes, 1);
    }

    #[test]
    fn m810_reset_returns_write_error_without_flushing() {
        let mut writer = Writer {
            fail_write: true,
            ..Writer::default()
        };
        assert_eq!(
            clear_host_mouse_reporting(&mut writer).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(writer.bytes.is_empty());
        assert_eq!(writer.flushes, 0);
    }

    #[test]
    fn m810_reset_returns_flush_error() {
        let mut writer = Writer {
            fail_flush: true,
            ..Writer::default()
        };
        assert_eq!(
            clear_host_mouse_reporting(&mut writer).unwrap_err().kind(),
            io::ErrorKind::ConnectionReset
        );
        assert_eq!(writer.bytes, DISABLE_HOST_MOUSE_REPORTING_SEQUENCE);
        assert_eq!(writer.flushes, 1);
    }

    #[test]
    fn host_keyboard_report_all_only_changes_the_current_zynk_stack_entry() {
        let mut writer = Writer::default();

        set_host_kitty_keyboard_report_all(&mut writer, true).unwrap();
        set_host_kitty_keyboard_report_all(&mut writer, false).unwrap();

        assert_eq!(writer.bytes, b"\x1b[=15u\x1b[=7u");
        assert_eq!(writer.flushes, 2);
    }
}
