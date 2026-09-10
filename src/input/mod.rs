// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
mod encode;
mod model;
mod parse;

#[allow(unused_imports)]
pub use encode::{
    encode_cursor_key, encode_key, encode_mouse_button, encode_mouse_scroll, encode_terminal_key,
};
pub use model::ime_compatible_keyboard_enhancement_flags;
pub use model::{
    host_modify_other_keys_mode, KeyIdentity, KeyboardProtocol, MouseProtocolEncoding,
    MouseProtocolMode, TerminalKey, TextCommit, WindowsKeyRecord,
};
pub use parse::parse_terminal_key_sequence;
