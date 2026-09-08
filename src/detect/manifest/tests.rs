use super::*;

fn remote_manifest(version: &str, state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"
version = "{version}"
min_engine_version = 1
updated_at = "2026-06-10T12:00:00Z"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn local_manifest(state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn rules_manifest(rules: &str) -> String {
    format!(
        r#"
id = "codex"

{rules}
"#
    )
}

fn with_manifest_dirs<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let old_config = std::env::var_os("XDG_CONFIG_HOME");
    let old_state = std::env::var_os("XDG_STATE_HOME");
    let base = std::env::temp_dir().join(format!(
        "zynk-manifest-loader-{name}-{}",
        std::process::id()
    ));
    let config_dir = base.join("config");
    let state_dir = base.join("state");
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    std::env::set_var("XDG_STATE_HOME", &state_dir);
    reload_manifests();
    let result = f();
    match old_config {
        Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    match old_state {
        Some(value) => std::env::set_var("XDG_STATE_HOME", value),
        None => std::env::remove_var("XDG_STATE_HOME"),
    }
    reload_manifests();
    let _ = std::fs::remove_dir_all(&base);
    result
}

fn write_remote_codex(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

fn write_remote_codex_without_reload(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_local_codex(content: &str) {
    let path = override_path(Agent::Codex).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

#[test]
fn known_agent_no_match_defaults_to_idle_fallback() {
    let explain = explain(Agent::Codex, "ordinary prompt text");

    assert_eq!(explain.state, AgentState::Idle);
    assert!(!explain.visible_idle);
    assert_eq!(
        explain.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
}

#[test]
fn rule_semantics_apply_gates_priority_and_line_regex() {
    with_manifest_dirs("rule-semantics", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "low_contains"
state = "idle"
priority = 1
contains = ["match"]

[[rules]]
id = "high_nested_gates"
state = "working"
priority = 10
contains = ["match"]
all = [
  { any = [{ regex = ["w[io]n"] }, { contains = ["fallback"] }] },
]
not = [
  { contains = ["blocked"] },
]

[[rules]]
id = "line_regex"
state = "blocked"
priority = 20
line_regex = ["^exact line$"]
"#,
        ));

        let high = explain(Agent::Codex, "match win");
        assert_eq!(high.state, AgentState::Working);
        assert_eq!(
            high.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("high_nested_gates")
        );

        let not_gate = explain(Agent::Codex, "match win blocked");
        assert_eq!(not_gate.state, AgentState::Idle);
        assert_eq!(
            not_gate.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("low_contains")
        );

        let line = explain(Agent::Codex, "before\nexact line\nafter");
        assert_eq!(line.state, AgentState::Blocked);
        assert_eq!(
            line.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("line_regex")
        );
    });
}

#[test]
fn remote_manifest_loads_between_local_override_and_bundled() {
    with_manifest_dirs("remote-source", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn fallback_explain_preserves_active_manifest_version() {
    with_manifest_dirs("fallback-version", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "ordinary prompt text");

        assert_eq!(explain.state, AgentState::Idle);
        assert_eq!(
            explain.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
    });
}

#[test]
fn older_cached_remote_manifest_does_not_shadow_newer_bundled_manifest() {
    with_manifest_dirs("older-remote-bundled-fallback", || {
        write_remote_codex(&remote_manifest("2026.06.10.0", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(matches!(explain.source, Some(ManifestSource::Bundled)));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("2026.06.10.0")
        );
        assert!(explain
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("older than bundled")));
    });
}

#[test]
fn local_override_shadows_cached_remote_manifest() {
    with_manifest_dirs("local-shadows-remote", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex(&local_manifest("idle", "local-ready"));

        let explain = explain(Agent::Codex, "local-ready");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(matches!(explain.source, Some(ManifestSource::Override(_))));
        assert!(explain.local_override_shadowing_remote);
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn invalid_local_override_falls_back_to_cached_remote_manifest() {
    with_manifest_dirs("invalid-local-remote-fallback", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex("id = ");

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert!(explain.warning.is_some());
    });
}

#[test]
fn detection_uses_cached_manifest_until_explicit_reload() {
    with_manifest_dirs("cache-boundary", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "cached-ready"));

        let cached = explain(Agent::Codex, "cached-ready");
        assert_eq!(cached.state, AgentState::Blocked);
        assert!(matches!(cached.source, Some(ManifestSource::Remote { .. })));
        assert_eq!(
            cached.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );

        write_remote_codex_without_reload(&remote_manifest("9999.01.01.2", "working", "new-ready"));

        let unchanged = explain(Agent::Codex, "new-ready");
        assert_eq!(unchanged.state, AgentState::Idle);
        assert_eq!(
            unchanged.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(
            unchanged.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );

        reload_manifests();

        let reloaded = explain(Agent::Codex, "new-ready");
        assert_eq!(reloaded.state, AgentState::Working);
        assert_eq!(
            reloaded.cached_remote_version.as_deref(),
            Some("9999.01.01.2")
        );
        assert_eq!(
            reloaded.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );
    });
}

#[test]
fn all_bundled_manifests_parse_and_validate() {
    // Iterate the constant, not a hand-listed array: an agent added to
    // SCREEN_MANIFEST_AGENTS without a bundled manifest must fail here rather
    // than land in the cache as a silent `(agent, None)` entry.
    for agent in Agent::SCREEN_MANIFEST_AGENTS {
        assert!(
            bundled_manifest(agent).is_some(),
            "missing bundled manifest for {}",
            agent_label(agent)
        );
    }
}

#[test]
fn devin_manifest_detects_idle_working_and_blocked_states() {
    let idle = explain(
        Agent::Devin,
        "─────────────────────────────────────────────────────\n❭ Ask Devin to build features, fix bugs, or work on\n  your code\n─────────────────────────────────────────────────────\nSWE-1.6               Context: 16k / 200k tokens (7%)",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);

    let live_footer_idle = explain(
        Agent::Devin,
        "Done.\n\n────────────────────────────────────────────────── (bypass permissions on) ─\n❭\n────────────────────────────────────────────────────────────────────────────\nClaude Opus 4.6 Thinking                                    Context: 38k / 200k tokens (18%)",
    );
    assert_eq!(live_footer_idle.state, AgentState::Idle);
    assert_eq!(
        live_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("live_prompt_footer")
    );
    assert!(live_footer_idle.visible_idle);

    let welcome_footer_idle = explain(
        Agent::Devin,
        "⠀⠀⠀⠀⠀⣴⣾⣶⡄⠀⠀⠀⠀\n⠀⣴⣾⣶⡾⠛⠿⠟⠃⣴⣾⣶⡄  Devin CLI\n⠀⠛⠿⠟⠃⣴⣾⣶⡾⠛⠿⠟⠃  v2026.5.26-8\n⠀⣤⣶⣦⡄⠻⢿⠿⢷⣤⣶⣦⡄\n⠀⠻⢿⠿⢷⣤⣶⣦⡄⠻⢿⠿⠃  Hybrid\n⠀⠀⠀⠀⠀⠻⢿⠿⠃⠀⠀⠀⠀\n\n───────────────────────────\n❭ Ask Devin to build\n  features, fix bugs, or\n  work on your code\n───────────────────────────\nClaude Opus Looking for\n4.6 Thinkingplan mode? /\n            plan",
    );
    assert_eq!(welcome_footer_idle.state, AgentState::Idle);
    assert_eq!(
        welcome_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("welcome_prompt_footer")
    );
    assert!(welcome_footer_idle.visible_idle);

    let working = explain(
        Agent::Devin,
        "◔ Reading shell 91b655\n  │ Timeout: 35s\n\n⠀⡆ Running tools · 27s (esc to interrupt)\n─────────────────────────────────────────────────────\n❭ Guide Devin while it works",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let trust_prompt = explain(
        Agent::Devin,
        "Do you trust the authors of this directory?\nFor security, devin should not be run in directories\nwith untrusted content.\n❭ 1 Yes, trust /private/tmp/devin-hook-probe\n· 2 No, exit",
    );
    assert_eq!(trust_prompt.state, AgentState::Blocked);
    assert!(trust_prompt.visible_blocker);

    let permission_prompt = explain(
        Agent::Devin,
        "⏺ Running command\n  └ $ sleep 30\n\n❭ 1 Yes  (Approve once)\n· 2 Yes, allow `sleep` commands\n· 3 Yes, always allow `sleep` commands\n· 4 No\n↑↓ select · ↵ confirm · esc cancel",
    );
    assert_eq!(permission_prompt.state, AgentState::Blocked);
    assert!(permission_prompt.visible_blocker);
}

#[test]
fn copilot_manifest_detects_ask_user_accept_prompt_and_ignores_bare_cancel_hint() {
    // ask_user accept prompt: an esc-cancel hint AND an enter-action hint must
    // both be present for the blocker. The new `enter accept` / `esc cancel`
    // phrasings (upstream #725) now resolve to Blocked.
    let accept_prompt = explain(
        Agent::GithubCopilot,
        "Allow Copilot to run this command?\n  $ rm tmp\n> Yes\n  No\nenter accept · esc cancel",
    );
    assert_eq!(accept_prompt.state, AgentState::Blocked);
    assert!(accept_prompt.visible_blocker);
    assert_eq!(
        accept_prompt
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("selection_blocker")
    );

    let select_prompt = explain(
        Agent::GithubCopilot,
        "Pick a file to edit\n> src/main.rs\n  src/lib.rs\nenter to select · esc to cancel",
    );
    assert_eq!(select_prompt.state, AgentState::Blocked);
    assert!(select_prompt.visible_blocker);

    // A bare cancel hint with no enter-action line is the working spinner, not a
    // selection blocker — the `all` gate must require the enter-family hint too.
    let working_cancel = explain(
        Agent::GithubCopilot,
        "Thinking about your request...\nesc to cancel",
    );
    assert_eq!(working_cancel.state, AgentState::Working);
    assert!(!working_cancel.visible_blocker);
}

#[test]
fn copilot_esc_interrupt_footer_is_working() {
    // Copilot CLI v1.0.69-2 replaced the interrupt footer with `esc interrupt`
    // (upstream #1119). It is not an `esc cancel` phrasing, so before the new
    // alternative a working pane matched nothing and fell through to idle.
    let working = explain(
        Agent::GithubCopilot,
        "\u{25cf} Working on your request\n\n  esc interrupt \u{b7} ctrl+c quit",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);
    assert!(!working.visible_blocker);
    assert_eq!(
        working.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("working_cancel_hint")
    );

    // No interrupt hint at all is still the idle fallback, so the alternative
    // above is what carries the working verdict.
    let done = explain(Agent::GithubCopilot, "Done. Updated 2 files.\n\n> ");
    assert_eq!(done.state, AgentState::Idle);
    assert!(!done.visible_working);
    assert!(done.matched_rule.is_none());
}

#[test]
fn maki_manifest_detects_idle_working_and_blocked_states() {
    // Maki paints a persistent one-line status bar on the bottom row: the bare
    // mode label is idle, a leading braille spinner cell is working. It sets no
    // OSC title and no OSC 9;4 progress, so these screen rules are the only
    // evidence the manifest has.
    let idle = explain(
        Agent::Maki,
        "\u{23fa} Updated src/main.rs\n\n [BUILD] gpt-5.5 \u{b7} ready \u{b7} ctrl-c quit",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);
    assert_eq!(
        idle.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("status_bar_idle")
    );

    let working = explain(
        Agent::Maki,
        "\u{23fa} Reading src/main.rs\n\n \u{2839} [BUILD] gpt-5.5 \u{b7} 42s \u{b7} ctrl-c stop",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);
    assert_eq!(
        working.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("status_bar_spinner_working")
    );

    // The permission panel replaces the input box above a status bar that still
    // reads idle, so the blocker has to outrank `status_bar_idle`.
    let permission_prompt = explain(
        Agent::Maki,
        "Permission Required\n\n  Run: rm -rf build\n\n  y Allow   n Deny   esc Cancel\n\n [BASH] gpt-5.5 \u{b7} waiting",
    );
    assert_eq!(permission_prompt.state, AgentState::Blocked);
    assert!(permission_prompt.visible_blocker);
    assert_eq!(
        permission_prompt
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("permission_prompt")
    );

    let plan_complete = explain(
        Agent::Maki,
        "Plan complete\n\n  1. Add the parser\n  2. Wire the CLI\n\n  space toggle parallel \u{b7} enter confirm\n\n [PLAN] gpt-5.5 \u{b7} review",
    );
    assert_eq!(plan_complete.state, AgentState::Blocked);
    assert!(plan_complete.visible_blocker);
    assert_eq!(
        plan_complete
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("plan_complete_form")
    );

    // On a narrow pane the right side of the status bar overwrites the mode
    // label, so idle falls back to the prompt chevron.
    let narrow_idle = explain(
        Agent::Maki,
        "\u{23fa} Done.\n\n\u{276f} add a test\n ctx 12k \u{b7} gpt-5.5",
    );
    assert_eq!(narrow_idle.state, AgentState::Idle);
    assert!(narrow_idle.visible_idle);
    assert_eq!(
        narrow_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("prompt_box_idle")
    );

    // The same narrow pane while streaming: the chevron is still there, so the
    // two `not` gates -- the queue placeholder and a spinner row -- are what
    // keep a working pane from reading as idle.
    let narrow_streaming = explain(
        Agent::Maki,
        "\u{276f} queue another prompt\n \u{280b} working",
    );
    assert!(!narrow_streaming.visible_idle);
    assert!(narrow_streaming.matched_rule.is_none());
}

#[test]
fn antigravity_background_task_chip_is_working_without_the_tasks_hint() {
    // The `/tasks` slash-command hint left the background-task status row
    // (upstream #755), so a rule anchored on that literal stopped matching and
    // a pane waiting on background work read as idle. The chip itself is the
    // anchor now.
    let waiting = explain(
        Agent::Antigravity,
        "Wrote src/lib.rs\n\n\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\u{2502} > Ask Antigravity \u{2502}\n\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\ngemini-3-pro \u{b7} 2 tasks \u{b7} ctrl-c quit",
    );
    assert_eq!(waiting.state, AgentState::Working);
    assert!(waiting.visible_working);
    assert_eq!(
        waiting.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("background_tasks_working")
    );

    // Same footer with no chip: nothing to wait on, so nothing matches.
    let no_chip = explain(
        Agent::Antigravity,
        "Wrote src/lib.rs\n\n\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\u{2502} > Ask Antigravity \u{2502}\n\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\ngemini-3-pro \u{b7} ctrl-c quit",
    );
    assert!(!no_chip.visible_working);
    assert!(no_chip.matched_rule.is_none());

    // The rule reads `bottom_non_empty_lines(5)`, so a chip left behind in
    // scrollback above five newer lines is stale evidence and must not match.
    let stale_chip = explain(
        Agent::Antigravity,
        "gemini-3-pro \u{b7} 2 tasks \u{b7} ctrl-c quit\nRead src/a.rs\nRead src/b.rs\n\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\u{2502} > Ask Antigravity \u{2502}\n\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
    );
    assert!(!stale_chip.visible_working);
    assert!(stale_chip.matched_rule.is_none());
}

#[test]
fn cursor_run_everything_status_is_not_an_approval_control() {
    // The approval rule's catch-all matched any line opening with `run `, so
    // Cursor's `Run Everything` status row read as a blocked approval and an
    // idle pane looked stuck (upstream #1763). The alternative now requires a
    // literal `run ... (y)` control.
    let run_everything = explain(
        Agent::Cursor,
        "Done. Updated 3 files.\n\n\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\u{2502} > Plan, search, build \u{2502}\n\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\n  Run Everything    shift+tab",
    );
    assert_eq!(run_everything.state, AgentState::Idle);
    assert!(!run_everything.visible_blocker);
    assert!(run_everything.matched_rule.is_none());

    // The control the rule is actually for still blocks, with or without the
    // selection arrow the tightened pattern makes optional. Neither screen
    // carries another `any` alternative, so this rule is what answers.
    for screen in [
        "Cursor wants to run a command\n\n  $ npm test\n\n\u{2192} Run (y)\n  Reject (n)",
        "Cursor wants to run a command\n\n  $ npm test\n\n  Run (y)\n  Reject (n)",
    ] {
        let approval = explain(Agent::Cursor, screen);
        assert_eq!(approval.state, AgentState::Blocked);
        assert!(approval.visible_blocker);
        assert_eq!(
            approval.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("approval_prompt")
        );
    }
}

#[test]
fn kiro_prompt_placeholder_is_idle_and_yields_to_the_working_banner() {
    // Kiro carried only negative idle evidence, so an idle pane fell through to
    // the fallback (upstream discussion #982). The composer placeholder is
    // positive evidence now.
    let idle = explain(
        Agent::Kiro,
        "Updated src/main.rs\n\n\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\u{2502} > Ask a question or describe a task \u{2502}\n\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\nctrl-j newline \u{b7} /copy to clipboard",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);
    assert_eq!(
        idle.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("prompt_idle")
    );

    // The composer keeps that placeholder while the agent works, and the idle
    // rule outranks the working banner 200 to 100 -- so the two `not` gates are
    // the only thing keeping a busy pane from reading as idle.
    let working = explain(
        Agent::Kiro,
        "\u{25d4} Kiro is working... (esc to cancel)\n\n\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\u{2502} > Ask a question or describe a task \u{2502}\n\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\nctrl-j newline \u{b7} /copy to clipboard",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);
    assert!(!working.visible_idle);
    assert_eq!(
        working.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("kiro_working_marker")
    );
}

#[test]
fn manifest_validation_rejects_unknown_fields_empty_rules_invalid_regions_and_regexes() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "typo"
state = "working"
contain = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "empty"
state = "working"
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_region"
state = "working"
region = "after_last_promt_marker"
contains = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_regex"
state = "working"
regex = ["["]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_nested_regex"
state = "working"
any = [{ line_regex = ["["] }]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_keeps_skip_rules_neutral() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_state"
state = "idle"
skip_state_update = true
contains = ["menu"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_visible"
state = "unknown"
skip_state_update = true
visible_blocker = true
contains = ["menu"]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_rejects_excessive_rule_count() {
    let mut manifest = String::from(
        r#"
id = "codex"
"#,
    );
    for index in 0..129 {
        manifest.push_str(&format!(
            r#"
[[rules]]
id = "rule_{index}"
state = "idle"
contains = ["ready"]
"#
        ));
    }

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_gate_depth() {
    let manifest = r#"
id = "codex"

[[rules]]
id = "deep"
state = "idle"
contains = ["ready"]
all = [
  { contains = ["1"], all = [
    { contains = ["2"], all = [
      { contains = ["3"], all = [
        { contains = ["4"], all = [
          { contains = ["5"], all = [
            { contains = ["6"], all = [
              { contains = ["7"], all = [
                { contains = ["8"], all = [
                  { contains = ["9"] },
                ] },
              ] },
            ] },
          ] },
        ] },
      ] },
    ] },
  ] },
]
"#;

    assert!(parse_manifest(manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_matchers() {
    let matchers = (0..33)
        .map(|index| format!(r#""m{index}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        r#"
id = "codex"

[[rules]]
id = "many"
state = "idle"
contains = [{matchers}]
"#
    );

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn bottom_non_empty_lines_uses_bottom_occurrence_for_repeated_text() {
    let content = "marker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "bottom_non_empty_lines(2)"
        ),
        "marker\nnew\n"
    );
}

#[test]
fn top_non_empty_lines_uses_top_occurrence_for_repeated_text() {
    let content = "\nmarker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "top_non_empty_lines(2)"
        ),
        "\nmarker\nold\n"
    );
}

#[test]
fn top_non_empty_lines_requires_a_canonical_positive_bounded_count() {
    let name = "top_non_empty_lines";
    assert!(validate_region_name(&format!("{name}(1)")).is_ok());
    assert!(validate_region_name(&format!("{name}({})", u16::MAX)).is_ok());
    for count in ["0", "01", "+1", "65536", "999999999999999999999999"] {
        assert!(
            validate_region_name(&format!("{name}({count})")).is_err(),
            "{name} accepted invalid count {count}"
        );
    }
}

#[test]
fn top_non_empty_lines_requires_engine_three_when_declared() {
    let manifest = r#"
id = "grok"
version = "1"
min_engine_version = 2

[[rules]]
id = "background"
state = "working"
region = " top_non_empty_lines(1) "
contains = ["active"]
"#;

    assert!(parse_manifest(manifest).is_err());
}

// ---------------------------------------------------------------------------
// OSC rule tests — exercise the new osc_title / osc_progress regions against
// the bundled Claude and Codex manifests.
// ---------------------------------------------------------------------------

fn osc_explain(
    agent: Agent,
    screen: &str,
    osc_title: &str,
    osc_progress: &str,
) -> DetectionExplain {
    explain_with_input(
        agent,
        DetectionInput {
            screen,
            osc_title,
            osc_progress,
        },
    )
}

// --- Claude OSC rules ---

#[test]
fn claude_osc_title_braille_prefix_is_working() {
    // "⠂" is U+2802, in the braille block U+2800-U+28FF
    let result = osc_explain(Agent::Claude, "", "⠂ project", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn claude_osc_title_half_circle_frames_are_working() {
    // Claude Code >= 2.1.228 spins with half-circle frames (U+25D0..U+25D3) instead of Braille; without this
    // rule a working Claude reads as idle.
    for frame in ['\u{25D0}', '\u{25D3}', '\u{25D1}', '\u{25D2}'] {
        let title = format!("{frame} Initial conversation with Claude");
        let result = osc_explain(Agent::Claude, "", &title, "");
        assert_eq!(result.state, AgentState::Working, "frame {frame}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("osc_title_working"),
            "frame {frame}"
        );
        assert!(result.visible_working, "frame {frame}");
    }
}

const CLAUDE_BUSY_TITLES: [&str; 5] = [
    "\u{25D0} Initial conversation with Claude",
    "\u{25D3} Initial conversation with Claude",
    "\u{25D1} Reading files",
    "\u{25D2} Reading files",
    "\u{280B} Thinking",
];

// Current dialogs: their hint footer is the last (or second-to-last, under a box border) non-empty line.
const CLAUDE_BASH_APPROVAL: &str = "do you want to proceed?\n\
    bash command: rm -rf /tmp/test\n\
    ❯ 1. Yes\n   2. No\n\n\
    Esc to cancel · Tab to amend · ctrl+e to explain\n";
const CLAUDE_GENERIC_PERMISSION: &str =
    "──────────\nDo you want to proceed?\n  1. Yes\n  2. No\n\nEsc to cancel · Tab to amend\n";
const CLAUDE_SELECTION_FORM: &str =
    "──────────\n  1. Yes\n  2. No\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
const CLAUDE_DYNAMIC_PROMPT: &str =
    "Run a dynamic workflow?\n❯ 1. Yes\n  2. No\nEnter to select · Esc to cancel\n";
const CLAUDE_BOXED_APPROVAL: &str = "╭──────────╮\n│ Bash command: ls │\n│ Do you want to proceed? │\n\
    │ ❯ 1. Yes │\n│   2. No │\n│ Esc to cancel · Tab to amend · ctrl+e to explain │\n╰──────────╯\n";
const CLAUDE_PROMPT_BOX: &str = "──────────\n❯ Ask Claude\n──────────\n";

fn claude_current_dialogs() -> [(&'static str, &'static str); 5] {
    [
        ("bash approval", CLAUDE_BASH_APPROVAL),
        ("generic permission", CLAUDE_GENERIC_PERMISSION),
        ("selection form", CLAUDE_SELECTION_FORM),
        ("dynamic prompt", CLAUDE_DYNAMIC_PROMPT),
        ("boxed approval", CLAUDE_BOXED_APPROVAL),
    ]
}

#[test]
fn claude_current_dialog_outranks_a_retained_busy_title() {
    // Gate-3 ARB-4FDA-OSC-PRECEDENCE-001 / ARB-EE13-OSC-FRESHNESS-001: Claude keeps its busy spinner title
    // while an approval, permission or selection dialog waits for the user (upstream issue #3467). A dialog
    // that is CURRENT — its hint footer is at the bottom of the buffer — must win over that title.
    for (label, screen) in claude_current_dialogs() {
        for title in CLAUDE_BUSY_TITLES {
            let result = osc_explain(Agent::Claude, screen, title, "");
            assert_eq!(result.state, AgentState::Blocked, "{label} / {title}");
            assert_eq!(
                result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
                Some("current_approval_dialog"),
                "{label} / {title}"
            );
            assert!(result.visible_blocker, "{label} / {title}");
            assert!(!result.visible_working, "{label} / {title}");
        }
        // Control: with a static title the same dialog is Blocked too.
        let result = osc_explain(Agent::Claude, screen, "\u{2733} Claude", "");
        assert_eq!(result.state, AgentState::Blocked, "{label} static");
        assert!(result.visible_blocker, "{label} static");
    }
}

#[test]
fn claude_answered_dialog_followed_by_work_is_working() {
    // The inverse failure: once the dialog was answered and work continued below it, the same text must no
    // longer block an active spinner (freshness, not just priority).
    for (label, screen) in claude_current_dialogs() {
        // No later divider or prompt box on purpose: only freshness (the footer is no longer at the bottom)
        // may release the blocker; a suffix selector such as after_last_horizontal_rule still sees the footer.
        let answered = format!("{screen}Selected: yes\nReading src/main.rs\n");
        for title in ["\u{25D0} Reading files", "\u{280B} Reading files"] {
            let result = osc_explain(Agent::Claude, &answered, title, "");
            assert_eq!(result.state, AgentState::Working, "{label} / {title}");
            assert_eq!(
                result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
                Some("osc_title_working"),
                "{label} / {title}"
            );
            assert!(result.visible_working, "{label} / {title}");
            assert!(!result.visible_blocker, "{label} / {title}");
        }
    }
}

#[test]
fn claude_historical_dialog_above_a_later_divider_is_working() {
    for (label, screen) in claude_current_dialogs() {
        let historical = format!("{screen}\n──────────\nReading src/main.rs\n{CLAUDE_PROMPT_BOX}");
        let result = osc_explain(Agent::Claude, &historical, "\u{25D0} Reading files", "");
        assert_eq!(result.state, AgentState::Working, "{label}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("osc_title_working"),
            "{label}"
        );
    }
}

#[test]
fn claude_busy_title_outranks_a_matched_idle_prompt_box() {
    // Gate-3 ARB-EE13-PROMPT-TEST-001: a REAL two-border prompt box must match live_prompt_box (proved from
    // evaluated_rules) and still lose to the busy title; with a static title the same box is Idle.
    let busy = osc_explain(
        Agent::Claude,
        CLAUDE_PROMPT_BOX,
        "\u{25D0} Reading files",
        "",
    );
    assert!(
        busy.evaluated_rules
            .iter()
            .any(|rule| rule.id == "live_prompt_box" && rule.matched),
        "the fixture must exercise live_prompt_box: {:?}",
        busy.evaluated_rules
            .iter()
            .map(|rule| (rule.id.as_str(), rule.matched))
            .collect::<Vec<_>>()
    );
    assert_eq!(busy.state, AgentState::Working);
    assert_eq!(
        busy.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("osc_title_working")
    );
    assert!(busy.visible_working && !busy.visible_idle);
    let idle = osc_explain(Agent::Claude, CLAUDE_PROMPT_BOX, "\u{2733} Claude", "");
    assert_eq!(idle.state, AgentState::Idle);
    assert_eq!(
        idle.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("live_prompt_box")
    );
    assert!(idle.visible_idle);
}

#[test]
fn claude_stale_dynamic_workflow_text_does_not_override_a_busy_title() {
    // Codex Gate-2 on 7b5639f: `dynamic_workflow_prompt` matches the whole recent screen, so a historical
    // "Run a dynamic workflow?" left in scrollback must stay below an active spinner, while a dynamic
    // workflow prompt without a busy title is still Blocked through that broad fallback.
    let stale = "Earlier prompt: Run a dynamic workflow?\nEsc to cancel\n\nSelected: yes\n\n\
        ──────────\nReading src/main.rs\n──────────\n❯ Ask Claude\n──────────\n";
    let result = osc_explain(Agent::Claude, stale, "\u{25D0} Reading files", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("osc_title_working")
    );
    // A bare "Esc to cancel" footer without a second hint is not a current dialog for the bounded rule;
    // the broad fallback still classifies the prompt as Blocked when no busy title is present.
    let current = "Run a dynamic workflow?\n  1. Yes\n  2. No\nEsc to cancel\n";
    let result = osc_explain(Agent::Claude, current, "", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("dynamic_workflow_prompt")
    );
    assert!(result.visible_blocker);
}

#[test]
fn claude_osc_title_adjacent_code_points_are_not_busy_frames() {
    // The frame class is exactly U+25D0..U+25D3 followed by a space.
    for title in ["\u{25CF} Claude", "\u{25D4} Claude", "\u{25D0}Claude"] {
        let result = osc_explain(Agent::Claude, "", title, "");
        assert_ne!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("osc_title_working"),
            "title {title}"
        );
        assert!(!result.visible_working, "title {title}");
    }
}

#[test]
fn claude_osc_title_static_prefix_is_idle() {
    // "✳" is U+2733, static prefix when Claude is not working
    let result = osc_explain(Agent::Claude, "", "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn claude_osc_progress_4_3_alone_does_not_force_working() {
    // Claude leaves progress stuck at 4;3 while waiting for permission, so
    // 4;3 must not be a working signal on its own. With no other evidence it
    // falls back to idle; blocked screen rules can win when present.
    let result = osc_explain(Agent::Claude, "", "", "4;3;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_working);
}

#[test]
fn claude_blocker_screen_outranks_stale_osc_progress() {
    // Regression: progress 4;3 persists during permission prompts. The
    // blocked form on screen must win because no rule treats 4;3 as working.
    let blocker_screen =
        "──────────\n  1. Yes\n  2. No\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Task title", "4;3;");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_osc_progress_4_0_is_idle() {
    let result = osc_explain(Agent::Claude, "", "", "4;0;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_progress_idle")
    );
}

#[test]
fn claude_blocker_screen_outranks_osc_idle_title() {
    // When the OSC title shows ✳ (idle) but the screen has a bash permission
    // prompt, the blocked rule at priority 850 beats osc_title_idle at 250.
    let blocker_screen = "do you want to proceed?\n\
        bash command: rm -rf /tmp/test\n\
        ❯ 1. Yes\n   2. No\n\n\
        Esc to cancel · Tab to amend · ctrl+e to explain\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_empty_osc_empty_screen_is_idle_fallback() {
    // No OSC data, no matching screen rule → fallback idle (unchanged V3 behavior)
    let result = osc_explain(Agent::Claude, "", "", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_idle);
}

// --- Codex OSC rules ---

#[test]
fn codex_osc_title_braille_spinner_is_working() {
    // "⠋" is U+280B, in the braille block
    let result = osc_explain(Agent::Codex, "", "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_title_spinner_away_from_the_title_start_is_working() {
    // Fork-only. Upstream `4800ff54` unanchored the spinner class because Codex
    // moved the braille cell off the front of the title, but shipped no test for
    // it. Under the old `^[\x{2800}-\x{28FF}] ` regex neither the working rule
    // nor `osc_title_idle`'s `not` gate matched these titles, so a working pane
    // reported Idle.
    for title in ["llm-proxy \u{2838}", "codex \u{2839} llm-proxy"] {
        let result = osc_explain(Agent::Codex, "", title, "");
        assert_eq!(result.state, AgentState::Working, "title {title:?}");
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("osc_title_working"),
            "title {title:?}"
        );
        assert!(result.visible_working, "title {title:?}");
    }

    // The class is an explicit frame list, not the whole braille block, and it
    // still needs a space or an edge on both sides: a braille cell welded into a
    // word is not a spinner.
    let not_a_spinner = osc_explain(Agent::Codex, "", "llm\u{280b}proxy", "");
    assert_eq!(not_a_spinner.state, AgentState::Idle);
    assert_eq!(
        not_a_spinner.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
}

#[test]
fn codex_osc_title_action_required_is_blocked() {
    let result = osc_explain(Agent::Codex, "", "[ . ] Action Required | llm-proxy", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_blocked")
    );
    assert!(result.visible_blocker);
}

#[test]
fn codex_osc_title_plain_is_idle() {
    let result = osc_explain(Agent::Codex, "", "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_background_terminal_screen_does_not_override_osc_idle() {
    // Background terminal tasks can be long-lived helpers such as dev servers.
    // They should not make Codex look busy once the foreground turn is idle.
    let screen = "background terminal running · /ps to view · /stop to close\n";
    let result = osc_explain(Agent::Codex, screen, "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_screen_working_fallback_handles_static_osc_title() {
    let screen = "• I’ll run it and wait for completion.\n\n\
        ◦ Working (1m 16s • esc to interrupt) · 1 background…\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_working_remains_preferred_over_screen_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "⠸ project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_screen_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › 1. Yes, proceed\n\
        Press enter to confirm or esc to cancel\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("live_strong_blocker")
    );
    assert!(result.visible_blocker);
    assert!(!result.visible_working);
}

#[test]
fn codex_weak_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        do you want to continue? [y/n]\n\
        › Use /skills to list available skills\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("weak_blocker")
    );
    assert!(!result.visible_working);
}

#[test]
fn codex_transcript_viewer_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › transcript\n\
        ↑/↓ to scroll · pgup/pgdn to move · home/end to jump · q to quit · esc to edit prev\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Unknown);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("transcript_viewer")
    );
    assert!(result.skip_state_update);
    assert!(!result.visible_working);
}

#[test]
fn codex_screen_working_fallback_ignores_stale_and_prompt_text() {
    let screens = [
        "◦ Working (1m 16s • esc to interrupt)\n\
         ■ Conversation interrupted\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
        "› Explain the text ◦ Working (1m 16s • esc to interrupt)\n\
         gpt-5.6-sol default · /work\n",
        "  ◦ Working (1m 16s • esc to interrupt)\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
    ];

    for screen in screens {
        let result = osc_explain(Agent::Codex, screen, "project", "");
        assert_eq!(result.state, AgentState::Idle);
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("osc_title_idle")
        );
        assert!(result.visible_idle);
        assert!(!result.visible_working);
    }
}

#[test]
fn codex_screen_working_fallback_ignores_interrupted_short_terminal() {
    let screen = "◦ Working (1m 16s • esc to interrupt)\n\
        ■ Conversation interrupted\n\
        ›\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
    assert!(!result.visible_working);
}

#[test]
fn codex_osc_working_beats_weak_blocker_screen() {
    // A stale [y/n] on screen triggers weak_blocker at priority 600, but an
    // active braille spinner in the OSC title is priority 1050 — OSC wins.
    let screen = "do you want to continue? [y/n]\n";
    let result = osc_explain(Agent::Codex, screen, "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
}

// --- Amp OSC + status-footer rules ---

#[test]
fn amp_manifest_detects_osc_states_and_the_status_footer() {
    // Fork-only. Upstream `b03f033d` added all four rules below without a test.
    // Amp's title carries the turn state, and its status footer is the only
    // evidence when the title is static.
    let blocked = osc_explain(
        Agent::Amp,
        "",
        "Plugin confirmation needed - amp - zynk",
        "",
    );
    assert_eq!(blocked.state, AgentState::Blocked);
    assert_eq!(
        blocked.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_plugin_confirmation_blocked")
    );
    assert!(blocked.visible_blocker);
    assert!(!blocked.visible_idle);

    // A braille spinner outranks the idle title, and the idle rule's `not` gate
    // keeps it quiet even though the title still carries " - amp - ".
    let working_title = osc_explain(Agent::Amp, "", "\u{2802} - amp - zynk", "");
    assert_eq!(working_title.state, AgentState::Working);
    assert_eq!(
        working_title.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(working_title.visible_working);

    // Static title, working footer: `status_footer_working` (200) is the only
    // working evidence and it must beat `osc_title_idle` (50).
    for verb in ["thinking", "streaming", "running tools", "waiting"] {
        let screen = format!("  \u{2570} \u{2838} {verb} \u{2500}\u{2500}\u{2500}\n");
        let result = osc_explain(Agent::Amp, &screen, " - amp - zynk", "");
        assert_eq!(result.state, AgentState::Working, "verb {verb:?}");
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("status_footer_working"),
            "verb {verb:?}"
        );
        assert!(result.visible_working, "verb {verb:?}");
    }

    // Static title, no footer: idle.
    let idle = osc_explain(Agent::Amp, "ready\n", " - amp - zynk", "");
    assert_eq!(idle.state, AgentState::Idle);
    assert_eq!(
        idle.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(idle.visible_idle);

    // The footer is bounded to the bottom five non-empty lines, so a footer
    // stranded above newer output is stale scrollback, not a live turn.
    let stale = osc_explain(
        Agent::Amp,
        "  \u{2570} \u{2838} thinking \u{2500}\u{2500}\u{2500}\none\ntwo\nthree\nfour\nfive\n",
        " - amp - zynk",
        "",
    );
    assert_eq!(stale.state, AgentState::Idle);
    assert!(!stale.visible_working);
}
