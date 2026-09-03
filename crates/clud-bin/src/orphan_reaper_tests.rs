use super::*;
use crate::process_identity::UNKNOWN_START_TIME;

/// The production predicate, with an empty OS spare-list — the #522 half
/// of the rule on its own.
fn marker_only(daemons: &HashSet<u32>, pid: u32) -> bool {
    may_kill(&SpareList::new(), daemons, pid)
}

/// A declared daemon is spared. This is what keeps zccache/soldr/fbuild
/// alive across a clud exit, and it is why the marker exists.
#[test]
fn declared_daemon_is_spared() {
    let daemons: HashSet<u32> = [4242].into_iter().collect();
    assert!(!marker_only(&daemons, 4242));
}

/// The regression #522 actually reports: a descendant whose environment
/// was rebuilt somewhere in the spawn chain carries no originator tag,
/// yet is an ordinary abandoned process. The old predicate spared it --
/// and, because sparing prunes, everything beneath it too.
#[test]
fn untagged_non_daemon_is_reapable() {
    let daemons: HashSet<u32> = [4242].into_iter().collect();
    assert!(
        marker_only(&daemons, 9001),
        "an env-stripped descendant that never declared itself a daemon \
             must stay reachable (regression of #522)"
    );
}

/// Absence of evidence is not evidence of daemon-hood. With no marker
/// anywhere, nothing is spared -- the previous behavior spared
/// everything untagged, which is the inversion this fixes.
#[test]
fn empty_daemon_set_spares_nothing() {
    let daemons: HashSet<u32> = HashSet::new();
    assert!(marker_only(&daemons, 1));
    assert!(marker_only(&daemons, u32::MAX));
}

// ---- #688: the OS-signal spare-list reaches *this* reaper too ----

/// Build the spare-list the way `report_and_reap` does, but from injected
/// facts. Tier 1 (#674): a reap decision must be a pure function of
/// `ProcessFacts`, assertable on every platform without spawning anything.
fn spares_from(facts: &reaper_facts::FactsSnapshot, candidates: &[(u32, &str)]) -> SpareList {
    reaper_facts::build_spare_list(
        facts,
        candidates
            .iter()
            .map(|(pid, image)| (*pid, (*image).to_string())),
    )
}

/// The exact shape #688 reports: sccache-class daemons never call
/// `running-process`, so they keep the inherited `CLUD:<pid>` tag and never
/// set the marker. Before this fix nothing in *this* reaper protected them,
/// and `clud slay` / the on-exit scan / the daemon sweep all killed them.
///
/// Asserted on the **reason**, not merely on survival: a reaper that spared
/// it by accident — or that never saw it at all — passes a survival-only
/// test and still regresses.
#[test]
fn a_listening_undeclared_orphan_is_spared_with_a_reason() {
    let mut facts = reaper_facts::FactsSnapshot::default();
    facts.listening.insert(5150);
    let spares = spares_from(&facts, &[(5150, "sccache.exe")]);

    assert_eq!(spares.get(&5150), Some(&SpareReason::ListeningEndpoint));
    assert!(!may_kill(&spares, &HashSet::new(), 5150));
}

/// The counterweight, and the reason the signal table is ranked rather than
/// permissive: a genuinely leaked orphan with no daemon signal at all is
/// still reaped. Over-sparing would silently turn the reaper off.
#[test]
fn a_leaked_orphan_with_no_daemon_signal_is_still_reaped() {
    let facts = reaper_facts::FactsSnapshot::default();
    let spares = spares_from(&facts, &[(9001, "node.exe")]);

    assert!(spares.is_empty(), "nothing here deserves protection");
    assert!(may_kill(&spares, &HashSet::new(), 9001));
}

/// Sparing prunes. A spared daemon's children are its in-flight work, not
/// leaked garbage, so the walk must stop at it rather than step over it.
#[test]
fn a_spared_daemon_prunes_its_subtree() {
    let mut facts = reaper_facts::FactsSnapshot::default();
    facts.session_leaders.insert(13);
    let spares = spares_from(&facts, &[(13, "sccache"), (14, "cc1plus")]);

    assert_eq!(spares.get(&13), Some(&SpareReason::SessionLeader));
    // The compiler child is not itself protected...
    assert!(may_kill(&spares, &HashSet::new(), 14));
    // ...but the walk never reaches it, because it prunes at 13. That is
    // `process_tree`'s contract, covered by its own `filter_tests`.
}

/// A declared daemon that is only a *descendant* of a candidate was never
/// in the candidate set, so no facts were collected about it. The host-wide
/// marker set is what keeps it alive.
#[test]
fn a_host_declared_daemon_is_spared_even_when_it_is_not_a_candidate() {
    let spares = SpareList::new();
    let daemons: HashSet<u32> = [777].into_iter().collect();
    assert!(!may_kill(&spares, &daemons, 777));
}

/// An unavailable signal must not spare. Absence of evidence is not
/// evidence of daemon-hood — the exact inversion #522 fixed, re-asserted
/// here because the fix added a second, platform-varying source of `None`.
#[test]
fn a_signal_this_platform_cannot_evaluate_never_spares() {
    let mut facts = reaper_facts::FactsSnapshot::default();
    facts.listening.insert(31);
    facts
        .unavailable
        .insert(reaper_facts::Signal::ListeningEndpoint);
    let spares = spares_from(&facts, &[(31, "sccache")]);

    assert!(spares.is_empty());
    assert!(may_kill(&spares, &HashSet::new(), 31));
}

/// The producer this reaper actually calls must never claim job membership
/// it cannot observe: there is no Job Object on the `clud slay` / on-exit /
/// daemon-sweep path, and answering "inside the job" would read as a
/// positive finding of containment.
#[test]
fn the_cross_platform_producer_reports_no_job_object() {
    let facts =
        reaper_facts::collect_host_facts(&[std::process::id()], &HashSet::new(), Vec::new());
    assert!(facts
        .unavailable
        .contains(&reaper_facts::Signal::JobMembership));
}

/// The query must not blow up when nothing has declared itself; a scan
/// on a machine with no daemons is the common case.
#[test]
fn declared_daemon_pids_is_callable() {
    let _ = declared_daemon_pids();
}

// ---- #673 Phase 6: the kill path re-verifies identity ----

/// The bug this closes: between the scan that selected a target and the
/// kill that acts, the PID died and was handed to something else. Killing
/// it would take out the replacement *and its whole subtree*.
#[test]
fn a_recycled_pid_is_never_killed() {
    let recorded = ProcessIdentity::new(4321, 1_700_000_000);
    let replacement = ProcessIdentity::new(4321, 1_700_000_042);
    assert!(!kill_target_is_current(recorded, Some(replacement)));
}

/// The ordinary case: same PID, same creation time, kill proceeds.
#[test]
fn an_unchanged_target_is_still_killed() {
    let recorded = ProcessIdentity::new(4321, 1_700_000_000);
    assert!(kill_target_is_current(recorded, Some(recorded)));
}

/// A target that simply exited before the kill is skipped rather than
/// having its number acted on.
#[test]
fn a_target_that_vanished_is_skipped() {
    assert!(!kill_target_is_current(
        ProcessIdentity::new(4321, 1_700_000_000),
        None
    ));
}

/// Without a recorded creation time there is nothing to compare, and
/// refusing would disable reaping wholesale rather than for one PID. This
/// is the documented `ProcessIdentity::matches` fallback, asserted here so
/// the choice is visible rather than incidental.
#[test]
fn an_unrecorded_creation_time_falls_back_to_pid_only() {
    let recorded = ProcessIdentity::new(4321, UNKNOWN_START_TIME);
    assert!(kill_target_is_current(
        recorded,
        Some(ProcessIdentity::new(4321, 1_700_000_000))
    ));
    assert!(!kill_target_is_current(
        recorded,
        Some(ProcessIdentity::new(9999, 1_700_000_000))
    ));
}

/// Descendants carry the creation time observed by the selecting scan, so
/// the kill path has something to re-verify against.
#[test]
fn descendant_identity_pairs_pid_with_observed_creation_time() {
    let tagged = process_scan::TaggedProcess {
        pid: 4321,
        start_time: 1_700_000_000,
        name: "node.exe".into(),
        command: "node vite.js".into(),
        originator: "CLUD:1".into(),
        parent_pid: 1,
        parent_alive: false,
    };
    let descendant = Descendant::from(tagged);
    assert_eq!(descendant.identity().pid, 4321);
    assert_eq!(descendant.identity().start_time, 1_700_000_000);
}

#[test]
fn classify_vite_preview_with_port() {
    let cmd = "node C:\\Users\\x\\node_modules\\.bin\\..\\vite\\bin\\vite.js preview --port 8081";
    assert_eq!(
        classify("node.exe", cmd),
        Shape::VitePreview {
            port: Some("8081".to_string())
        }
    );
}

#[test]
fn classify_vite_preview_without_port() {
    let cmd = "node vite.js preview";
    assert_eq!(classify("node.exe", cmd), Shape::VitePreview { port: None });
}

#[test]
fn classify_vite_dev() {
    let cmd = "node /usr/local/lib/node_modules/.bin/../vite/bin/vite.js --port=5173";
    assert_eq!(
        classify("node", cmd),
        Shape::ViteDev {
            port: Some("5173".to_string())
        }
    );
}

#[test]
fn classify_codex() {
    let cmd = "node /home/user/.npm/lib/codex.js --bypass";
    assert_eq!(classify("node", cmd), Shape::OpenAiCodex);
}

#[test]
fn classify_claude_code_cli() {
    let cmd =
            "C:\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe --dangerously-skip-permissions";
    assert_eq!(classify("claude.exe", cmd), Shape::ClaudeCode);
}

#[test]
fn classify_pyright_langserver_python_shim() {
    let cmd = "python.exe Scripts\\pyright-langserver.exe --stdio";
    assert_eq!(classify("python.exe", cmd), Shape::PyrightLangserver);
}

#[test]
fn classify_pyright_langserver_node_side() {
    let cmd = "node pyright/dist/langserver.index.js -- --stdio";
    assert_eq!(classify("node", cmd), Shape::PyrightLangserver);
}

#[test]
fn classify_pm2() {
    let cmd = "node pm2-beta/lib/Daemon.js";
    assert_eq!(classify("node", cmd), Shape::Pm2);
}

#[test]
fn classify_npm_run_preview() {
    let cmd = "C:\\Scripts\\npm.exe run preview -- --port 9090";
    assert_eq!(classify("npm.exe", cmd), Shape::NpmRunPreview);
}

#[test]
fn classify_npm_run_dev() {
    let cmd = "C:\\Scripts\\npm.exe run dev";
    assert_eq!(classify("npm.exe", cmd), Shape::NpmRunDev);
}

#[test]
fn classify_nodejs_wheel_shim() {
    let cmd = "C:\\tools\\python13\\Lib\\site-packages\\nodejs_wheel\\node.exe some-script.js";
    assert_eq!(classify("node.exe", cmd), Shape::NodejsWheelShim);
}

#[test]
fn classify_generic_falls_back_to_name_and_basename() {
    let cmd = "/usr/bin/some-thing /home/u/work/script.sh --arg1";
    match classify("some-thing", cmd) {
        Shape::Generic { label } => assert!(
            label.contains("some-thing") && label.contains("script.sh"),
            "got {label:?}"
        ),
        other => panic!("expected Generic, got {other:?}"),
    }
}

/// Issue #360: Codex's command-safety AST parser runs as
/// `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy
/// Bypass -EncodedCommand <multi-KB-base64>`. The classifier must
/// recognize it as PowerShell-EncodedCommand so the printed label
/// stays short instead of falling into `Shape::Generic` and
/// surfacing the base64 payload as the row label.
#[test]
fn classify_powershell_encoded_command_codex_shape() {
    let cmd = "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
                   -EncodedCommand JABFAHIAcgBvAHIAQQBjAHQAaQBvAG4AUAByAGUAZgBlAHIAZQBuAGMAZQA=";
    assert_eq!(
        classify("powershell.exe", cmd),
        Shape::PowerShell {
            mode: PowerShellMode::EncodedCommand
        }
    );
}

#[test]
fn classify_powershell_command_flag() {
    let cmd = "powershell.exe -NoProfile -Command Get-Process";
    assert_eq!(
        classify("powershell.exe", cmd),
        Shape::PowerShell {
            mode: PowerShellMode::Command
        }
    );
}

#[test]
fn classify_powershell_file_flag() {
    let cmd = "powershell.exe -NoProfile -File C:\\scripts\\foo.ps1";
    assert_eq!(
        classify("powershell.exe", cmd),
        Shape::PowerShell {
            mode: PowerShellMode::File
        }
    );
}

#[test]
fn classify_powershell_bare_other() {
    // Plain `powershell.exe` with no recognized mode flag still
    // gets classified as PowerShell — not Generic — so the label
    // stays short.
    let cmd = "powershell.exe -NoExit";
    assert_eq!(
        classify("powershell.exe", cmd),
        Shape::PowerShell {
            mode: PowerShellMode::Other
        }
    );
}

#[test]
fn classify_pwsh_core_recognized() {
    // PowerShell 7+ ships as `pwsh.exe`; same classifier path.
    let cmd = "pwsh.exe -NoProfile -EncodedCommand JABF";
    assert_eq!(
        classify("pwsh.exe", cmd),
        Shape::PowerShell {
            mode: PowerShellMode::EncodedCommand
        }
    );
}

#[test]
fn classify_powershell_case_insensitive_flag() {
    // PowerShell accepts mixed case on switches; the classifier
    // must too. Codex emits `-EncodedCommand`, ad-hoc scripts often
    // use `-encodedcommand` or `-EnC`.
    let cmd = "powershell.exe -noprofile -encodedcommand AAAA";
    assert_eq!(
        classify("powershell.exe", cmd),
        Shape::PowerShell {
            mode: PowerShellMode::EncodedCommand
        }
    );
}

#[test]
fn powershell_label_is_short_and_distinguishes_modes() {
    // Each mode produces a distinct, single-line label. The
    // critical guarantee for #360 is that no mode can produce a
    // multi-kilobyte label — the format is hard-coded.
    assert_eq!(
        Shape::PowerShell {
            mode: PowerShellMode::EncodedCommand
        }
        .label(),
        "powershell (EncodedCommand)"
    );
    assert_eq!(
        Shape::PowerShell {
            mode: PowerShellMode::Command
        }
        .label(),
        "powershell (-Command)"
    );
    assert_eq!(
        Shape::PowerShell {
            mode: PowerShellMode::File
        }
        .label(),
        "powershell (-File)"
    );
    assert_eq!(
        Shape::PowerShell {
            mode: PowerShellMode::Other
        }
        .label(),
        "powershell"
    );
    // Sanity: every mode's label stays well under the print-site
    // cap, so PowerShell rows never get ellipsized.
    for mode in [
        PowerShellMode::EncodedCommand,
        PowerShellMode::Command,
        PowerShellMode::File,
        PowerShellMode::Other,
    ] {
        let label = Shape::PowerShell { mode }.label();
        assert!(
            label.len() < PRINTED_LABEL_MAX,
            "label {label:?} ({} bytes) must stay under PRINTED_LABEL_MAX = {PRINTED_LABEL_MAX}",
            label.len()
        );
    }
}

/// Issue #360 defense-in-depth: even if a future classifier path
/// produces a Generic label longer than the basename cap (e.g.
/// from a renamed argv format), the truncation helper guarantees
/// the printed row stays one line.
#[test]
fn truncate_with_ellipsis_caps_long_input_and_adds_ellipsis() {
    let blob = "a".repeat(500);
    let out = truncate_with_ellipsis(&blob, 40);
    // Truncated bytes + the 3-byte ellipsis (UTF-8) = 43 bytes max.
    assert!(out.len() <= 40 + "…".len());
    assert!(out.ends_with('…'));
}

#[test]
fn truncate_with_ellipsis_short_input_unchanged() {
    // No truncation, no ellipsis appended.
    let out = truncate_with_ellipsis("hello", 40);
    assert_eq!(out, "hello");
}

#[test]
fn truncate_with_ellipsis_at_exact_boundary_unchanged() {
    // Boundary case: input length == max. No truncation expected.
    let s = "a".repeat(40);
    let out = truncate_with_ellipsis(&s, 40);
    assert_eq!(out, s);
}

#[test]
fn truncate_with_ellipsis_respects_utf8_boundaries() {
    // Truncating mid-multibyte-char would panic on the slice
    // operation. Walk-back-to-boundary must save us — and the
    // resulting string must still be valid UTF-8.
    let s = "中文测试字符串";
    let out = truncate_with_ellipsis(s, 5);
    // Round-trip through UTF-8 must succeed; the only way this
    // fails is if we sliced through a multibyte sequence.
    assert!(out.is_char_boundary(out.len()));
    assert!(out.ends_with('…'));
}

/// Issue #360 root case: a Generic basename longer than
/// `GENERIC_BASENAME_MAX` (e.g. a base64 token with no path
/// separators) must be truncated by the classifier so the printed
/// label stays short even before the print-site cap kicks in.
#[test]
fn classify_generic_truncates_long_basename() {
    let blob = "A".repeat(500);
    let cmd = format!("/usr/bin/some-tool {blob}");
    match classify("some-tool", &cmd) {
        Shape::Generic { label } => {
            assert!(
                label.len() < 100,
                "label should be truncated, got {} bytes: {label:?}",
                label.len()
            );
            assert!(
                label.ends_with('…'),
                "expected trailing ellipsis, got {label:?}"
            );
        }
        other => panic!("expected Generic, got {other:?}"),
    }
}

#[test]
fn detect_powershell_mode_recognizes_short_aliases() {
    // PowerShell accepts `-enc` and `-c` abbreviations; the
    // detector should too.
    assert_eq!(
        detect_powershell_mode("powershell.exe -enc AAAA"),
        PowerShellMode::EncodedCommand
    );
    assert_eq!(
        detect_powershell_mode("powershell.exe -c Get-Process"),
        PowerShellMode::Command
    );
    assert_eq!(
        detect_powershell_mode("powershell.exe -f foo.ps1"),
        PowerShellMode::File
    );
}

#[test]
fn detect_powershell_mode_other_when_no_recognized_flag() {
    // Plain `powershell.exe` and unrecognized flags both fall
    // through to Other rather than being misclassified as a
    // specific mode.
    assert_eq!(
        detect_powershell_mode("powershell.exe"),
        PowerShellMode::Other
    );
    assert_eq!(
        detect_powershell_mode("powershell.exe -NoProfile -NoExit"),
        PowerShellMode::Other
    );
}

#[test]
fn extract_port_space_form() {
    assert_eq!(
        extract_port("vite preview --port 8081 --host"),
        Some("8081".to_string())
    );
}

#[test]
fn extract_port_equals_form() {
    assert_eq!(extract_port("vite --port=5173"), Some("5173".to_string()));
}

#[test]
fn extract_port_none() {
    assert_eq!(extract_port("vite preview"), None);
}

#[test]
fn extract_port_non_numeric_ignored() {
    assert_eq!(extract_port("vite --port=abc"), None);
}

#[test]
fn shape_label_includes_port_when_present() {
    assert_eq!(
        Shape::VitePreview {
            port: Some("8082".into())
        }
        .label(),
        "vite preview port=8082"
    );
    assert_eq!(Shape::VitePreview { port: None }.label(), "vite preview");
}

#[test]
fn scan_finds_nothing_when_no_descendants_match_self_pid() {
    // Pick a PID that is overwhelmingly unlikely to be a real CLUD originator
    // on the test host. Even if a real clud session were running, its
    // originator is its own PID, which is not u32::MAX. The scan returns
    // a default outcome (0/0) without touching anything.
    let outcome = scan_and_report(
        u32::MAX,
        &ReapOpts {
            keep: true,
            quiet: true,
            explain: false,
        },
    );
    assert_eq!(outcome.found, 0);
    assert_eq!(outcome.reaped, 0);
}

#[test]
fn reap_orphans_in_keep_mode_does_not_kill() {
    // `keep: true` means: list candidates but never invoke kill_tree. The
    // test host may or may not have CLUD-tagged descendants with a dead
    // originator, so we only assert that `reaped == 0` (never kill) and
    // that `found >= reaped`. This guards against regressions where the
    // shared report_and_reap path stops honoring `keep`.
    let outcome = reap_orphans(&ReapOpts {
        keep: true,
        quiet: true,
        explain: false,
    });
    assert_eq!(outcome.reaped, 0);
    assert!(outcome.found >= outcome.reaped);
}

/// A tagged process is only ours if we could plausibly have started it.
///
/// Linux recycles PIDs through `kernel.pid_max` and wraps. A `__worker`
/// leaked by an interrupted pytest run carried
/// `RUNNING_PROCESS_ORIGINATOR=CLUD:3513633` for a day and a half; a fresh
/// clud that the kernel later handed PID 3513633 matched it on raw PID
/// equality and reported someone else's leak as its own descendant.
#[test]
fn a_recycled_originator_pid_does_not_adopt_an_older_leak() {
    let leak = process_scan::TaggedProcess {
        pid: 586_489,
        start_time: 1_700_000_000,
        name: "clud".into(),
        command: "clud __worker --session-id sess-1".into(),
        originator: "CLUD:3513633".into(),
        parent_pid: 3_513_633,
        // We started *after* it, so we are not its originator.
        parent_alive: false,
    };
    assert!(select_own_descendants(vec![leak], 3_513_633).is_empty());
}

/// The same filter still selects a genuine descendant: same originator PID,
/// and a start time that is consistent with us having spawned it.
#[test]
fn a_genuine_descendant_of_this_pid_is_still_selected() {
    let child = process_scan::TaggedProcess {
        pid: 4321,
        start_time: 1_700_000_000,
        name: "node".into(),
        command: "node vite.js".into(),
        originator: "CLUD:3513633".into(),
        parent_pid: 3_513_633,
        parent_alive: true,
    };
    let selected = select_own_descendants(vec![child], 3_513_633);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].pid, 4321);
}

/// A descendant tagged with a *different* clud's PID is never ours, however
/// plausible its own lineage is.
#[test]
fn a_concurrent_cluds_descendant_is_left_alone() {
    let other = process_scan::TaggedProcess {
        pid: 4321,
        start_time: 1_700_000_000,
        name: "node".into(),
        command: "node vite.js".into(),
        originator: "CLUD:999".into(),
        parent_pid: 999,
        parent_alive: true,
    };
    assert!(select_own_descendants(vec![other], 3_513_633).is_empty());
}

// ---------------------------------------------------------------------------
// #1146: the spare rows must name what they spared.
// ---------------------------------------------------------------------------

fn descendant(pid: u32, name: &str, command: &str) -> Descendant {
    Descendant {
        pid,
        start_time: UNKNOWN_START_TIME,
        name: name.to_string(),
        command: command.to_string(),
    }
}

/// The report this issue was filed about: 19 spare lines, each a bare PID,
/// all of them the same one `__worker` shape. Grouping has to collapse them
/// the way the candidate rows already collapse — one row, one count, one
/// label — because "19 identical facts printed 19 times" is what made the
/// original output unreadable.
#[test]
fn spare_rows_group_by_shape_and_reason_and_name_the_executable() {
    let ds: Vec<Descendant> = (0..19)
        .map(|i| descendant(586_530 + i, "clud", "clud __worker --session-id s"))
        .collect();
    let classified: Vec<(Shape, &Descendant)> = ds
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    let spares: SpareList = ds
        .iter()
        .map(|d| (d.pid, SpareReason::SessionLeader))
        .collect();

    let rows = spare_rows(&classified, &spares);

    assert_eq!(
        rows.len(),
        1,
        "19 identical spares must collapse to one row: {rows:#?}"
    );
    assert_eq!(rows[0].count, 19);
    assert_eq!(rows[0].reason, SpareReason::SessionLeader);
    assert!(
        rows[0].label.contains("clud"),
        "the row must name the executable, not just the PID: {:?}",
        rows[0].label
    );
    assert_eq!(rows[0].pids.len(), 19);
}

/// Two shapes spared for the same reason are two different facts, and the
/// original report could not tell them apart — it showed one `ctrl-c
/// __worker` among 18 `clud __worker`s as 19 indistinguishable lines.
#[test]
fn spare_rows_separate_distinct_shapes() {
    let ds = [
        descendant(1, "clud", "clud __worker --session-id a"),
        descendant(2, "clud", "clud __worker --session-id b"),
        descendant(3, "ctrl-c", "ctrl-c __worker"),
    ];
    let classified: Vec<(Shape, &Descendant)> = ds
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    let spares: SpareList = ds
        .iter()
        .map(|d| (d.pid, SpareReason::SessionLeader))
        .collect();

    let rows = spare_rows(&classified, &spares);
    assert_eq!(
        rows.len(),
        2,
        "distinct shapes must not be merged: {rows:#?}"
    );
    assert!(rows.iter().any(|r| r.count == 2));
    assert!(rows.iter().any(|r| r.count == 1));
}

/// Same shape, different OS verdict: the reason is *why* the reaper left it
/// alive, so merging two reasons into one row would report a decision that
/// was never taken.
#[test]
fn spare_rows_separate_distinct_reasons() {
    let ds = [
        descendant(1, "clud", "clud __worker --session-id a"),
        descendant(2, "clud", "clud __worker --session-id b"),
    ];
    let classified: Vec<(Shape, &Descendant)> = ds
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    let spares: SpareList = [
        (1, SpareReason::SessionLeader),
        (2, SpareReason::ListeningEndpoint),
    ]
    .into_iter()
    .collect();

    let rows = spare_rows(&classified, &spares);
    assert_eq!(
        rows.len(),
        2,
        "one shape, two verdicts, two rows: {rows:#?}"
    );
}

/// Only spared candidates appear. A row for a process the reaper killed
/// would be a false claim about an action not taken.
#[test]
fn spare_rows_omit_unspared_candidates() {
    let ds = [
        descendant(1, "clud", "clud __worker --session-id a"),
        descendant(2, "clud", "clud __worker --session-id b"),
    ];
    let classified: Vec<(Shape, &Descendant)> = ds
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    let spares: SpareList = [(1, SpareReason::SessionLeader)].into_iter().collect();

    let rows = spare_rows(&classified, &spares);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pids, vec![1]);
}

/// #360's cap has to hold on this path too. A `Generic` label built from a
/// multi-kilobyte cmdline reaches the new spare row and the new tail line,
/// and an uncapped one would spray the terminal exactly as #360 describes.
#[test]
fn spare_row_labels_are_capped() {
    let huge = "x".repeat(5_000);
    let ds = [descendant(1, "weird", &format!("weird {huge}"))];
    let classified: Vec<(Shape, &Descendant)> = ds
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    let spares: SpareList = [(1, SpareReason::SessionLeader)].into_iter().collect();

    let rows = spare_rows(&classified, &spares);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].label.chars().count() <= PRINTED_LABEL_MAX,
        "label must be capped for print (#360), got {} chars",
        rows[0].label.chars().count()
    );
}

/// Deterministic order, for the same reason `spared` is sorted by PID: a
/// report and any assertion over it must not depend on map iteration order.
#[test]
fn spare_rows_are_deterministically_ordered() {
    let ds: Vec<Descendant> = [(3, "zzz"), (1, "aaa"), (2, "mmm")]
        .iter()
        .map(|(pid, name)| descendant(*pid, name, &format!("{name} --run")))
        .collect();
    let classified: Vec<(Shape, &Descendant)> = ds
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    let spares: SpareList = ds
        .iter()
        .map(|d| (d.pid, SpareReason::SessionLeader))
        .collect();

    let first = spare_rows(&classified, &spares);
    for _ in 0..8 {
        let again = spare_rows(&classified, &spares);
        assert_eq!(
            first.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
            again.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
        );
    }
    let labels: Vec<String> = first.iter().map(|r| r.label.clone()).collect();
    let mut sorted = labels.clone();
    sorted.sort();
    assert_eq!(labels, sorted, "rows must be label-ordered");
}

/// The line an operator actually reads. Asserted verbatim because the whole
/// issue is that the previous line — `sparing pid=586489 (session_leader) —
/// OS signal says this is a daemon` — was syntactically fine and told the
/// reader nothing about what had been spared.
#[test]
fn spare_row_renders_the_name_the_count_and_the_reason() {
    let ds: Vec<Descendant> = [586_489u32, 586_491]
        .iter()
        .map(|pid| descendant(*pid, "clud", "clud __worker --session-id s"))
        .collect();
    let classified: Vec<(Shape, &Descendant)> = ds
        .iter()
        .map(|d| (classify(&d.name, &d.command), d))
        .collect();
    let spares: SpareList = ds
        .iter()
        .map(|d| (d.pid, SpareReason::SessionLeader))
        .collect();

    let rows = spare_rows(&classified, &spares);
    let line = rows[0].render();

    assert!(line.contains("2x"), "{line}");
    assert!(
        line.contains("clud"),
        "the executable must be named: {line}"
    );
    assert!(line.contains("(session_leader)"), "{line}");
    assert!(line.contains("586489, 586491"), "{line}");
    assert!(
        !line.contains("OS signal says this is a daemon"),
        "the per-PID prose line is what this replaced: {line}"
    );
}
