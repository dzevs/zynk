# libghostty-vt local patches

This file tracks intentional local changes applied on top of the vendored
`libghostty-vt` source. Remove a patch only when the vendored source commit
contains the upstream behavior and the listed verification still passes.

## 0001 default lib-vt panes to grapheme clustering

status: active

patch: `vendor/patches/libghostty-vt/0001-default-grapheme-cluster-mode.patch`

upstream issue: libghostty-vt default terminal mode configuration

upstream discussion: not opened; libghostty-vt currently exposes current mode mutation but no C API for configuring terminal default modes

upstream pr: not opened

introduced upstream: not yet

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: zynk renders terminal cells directly and requires DEC private mode
2027 to store flags, ZWJ emoji, and other multi-codepoint grapheme clusters in
one cell. This patch makes clustering active for new terminals and keeps it as
the reset default so RIS (`ESC c`) does not disable it.

remove when: libghostty-vt exposes a C API for setting default mode 2027, or
upstream makes grapheme clustering the lib-vt default, and the reset-survival
regression passes without this patch.

verification:

```sh
cargo nextest run --locked --bin zynk grapheme_cluster_mode_is_default_and_survives_full_reset
cargo nextest run --locked --bin zynk grapheme_cluster_mode_renders_flag_emoji_in_single_wide_cell
cargo nextest run --locked --bin zynk grapheme_cluster_mode_renders_zwj_family_in_single_wide_cell
```

## 0002 expose modifyOtherKeys mode through terminal data

status: active

patch: `vendor/patches/libghostty-vt/0002-expose-modify-other-keys-mode.patch`

zynk issue: none; fixes the performance regression exposed by the upstream
modifyOtherKeys release-handling work

upstream discussion: not opened

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: zynk needs an allocation-free scalar query for xterm
modifyOtherKeys mode 2. The formatter API can recover this fact only by
formatting the active screen and scrollback. M7 uses the scalar in the retained
handoff snapshot; M8 will also use it for host report-all negotiation.

remove when: the vendored source exposes an equivalent scalar query for
modifyOtherKeys mode 2 and zynk can use it without this patch.

verification:

```sh
cargo nextest run --locked --bin zynk modify_other_keys_query_tracks_mode_two
cargo nextest run --locked --bin zynk handoff_runtime_state_captures_terminal_input_state
python3 -m unittest scripts.test_vendor_libghostty_vt scripts.test_ui_hot_path_architecture
```
