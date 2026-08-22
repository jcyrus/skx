//! End-to-end tests that drive the actual `skx` binary as a subprocess —
//! the same way a user would — against a temporary workspace and a fake
//! `HOME`, so nothing here can touch the real filesystem outside the test's
//! own tempdir.

use std::path::Path;
use std::process::Output;

const SAMPLE_SKILL: &str = "---\nname: rust-systems-expert\ndescription: Deep systems architectural conventions\nversion: 1.0.0\ntriggers:\n  - \"*.rs\"\ntargets:\n  antigravity:\n    scope: workspace\n    auto_activate: true\n  claude_code:\n    enabled: true\n  cursor:\n    glob: \"**/*.rs\"\n  copilot:\n    enabled: true\nmcp_dependencies:\n  - name: rust-analyzer-mcp\n    command: rust-analyzer-mcp\n    args: [\"--stdio\"]\n---\n\n# Rust Systems Engineering Instructions\n\n- Prefer zero-cost abstractions.\n";

/// Runs the real binary against a throwaway home directory.
///
/// `HOME` alone isn't enough: `dirs::home_dir()` reads `USERPROFILE` on
/// Windows and ignores `HOME` entirely, so tests that set only `HOME` there
/// resolved the *developer's* actual home and then asserted against a temp
/// directory that skx had never written to. Setting both keeps the sandbox
/// real on every platform — and matters beyond test hygiene, since a test
/// escaping into the real home would write skills into it.
fn run(workspace: &Path, home: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_skx"))
        .args(args)
        .current_dir(workspace)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("failed to run skx binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

struct TestEnv {
    _root: tempfile::TempDir,
    workspace: std::path::PathBuf,
    home: std::path::PathBuf,
    skill_src: std::path::PathBuf,
}

fn setup() -> TestEnv {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let skill_src = root.path().join("skill_src");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&skill_src).unwrap();
    std::fs::write(skill_src.join("SKILL.md"), SAMPLE_SKILL).unwrap();
    TestEnv {
        _root: root,
        workspace,
        home,
        skill_src,
    }
}

#[test]
fn init_creates_skx_toml() {
    let env = setup();
    let output = run(&env.workspace, &env.home, &["init"]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(env.workspace.join("skx.toml").is_file());

    // Running init again shouldn't overwrite it.
    std::fs::write(env.workspace.join("skx.toml"), "detected_targets = []\n").unwrap();
    let output = run(&env.workspace, &env.home, &["init"]);
    assert!(stdout(&output).contains("already exists"));
}

#[test]
fn add_rejects_url_sources() {
    let env = setup();
    let output = run(
        &env.workspace,
        &env.home,
        &["add", "https://github.com/example/skill"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("supported yet"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn add_then_list_shows_the_installed_skill() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    let add_out = run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap()],
    );
    assert!(add_out.status.success(), "{}", stdout(&add_out));
    assert!(
        env.workspace
            .join(".skx/skills/rust-systems-expert/SKILL.md")
            .is_file()
    );

    let list_out = run(&env.workspace, &env.home, &["list"]);
    assert!(stdout(&list_out).contains("rust-systems-expert v1.0.0"));
}

#[test]
fn sync_then_audit_reports_in_sync() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap()],
    );
    let sync_out = run(&env.workspace, &env.home, &["sync"]);
    assert!(sync_out.status.success(), "{}", stdout(&sync_out));
    assert!(stdout(&sync_out).contains("Synced 1 skill(s)"));

    // Every declared target should have produced a real file/symlink.
    assert!(
        env.workspace
            .join(".agents/skills/rust-systems-expert/SKILL.md")
            .exists()
    );
    assert!(
        env.workspace
            .join(".claude/skills/rust-systems-expert/SKILL.md")
            .exists()
    );
    assert!(
        env.workspace
            .join(".cursor/rules/rust-systems-expert.mdc")
            .exists()
    );
    assert!(
        env.workspace
            .join(".github/copilot-instructions.md")
            .exists()
    );
    assert!(env.workspace.join(".vscode/mcp.json").exists());

    let audit_out = run(&env.workspace, &env.home, &["audit"]);
    assert!(
        stdout(&audit_out).contains("Everything in sync"),
        "{}",
        stdout(&audit_out)
    );
}

#[test]
fn audit_detects_a_hand_edit_to_a_shared_file() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap()],
    );
    run(&env.workspace, &env.home, &["sync"]);

    let copilot_path = env.workspace.join(".github/copilot-instructions.md");
    let original = std::fs::read_to_string(&copilot_path).unwrap();
    std::fs::write(&copilot_path, original.replace("zero-cost", "HAND EDITED")).unwrap();

    let audit_out = run(&env.workspace, &env.home, &["audit"]);
    let text = stdout(&audit_out);
    assert!(text.contains("UserModified"), "{text}");
    assert!(text.contains("issue(s) found"), "{text}");
}

#[test]
fn remove_unlinks_every_artifact_and_cleans_the_shared_region() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap()],
    );
    run(&env.workspace, &env.home, &["sync"]);

    let remove_out = run(
        &env.workspace,
        &env.home,
        &["remove", "rust-systems-expert"],
    );
    assert!(remove_out.status.success(), "{}", stdout(&remove_out));
    assert!(stdout(&remove_out).contains("Removed rust-systems-expert"));

    assert!(
        !env.workspace
            .join(".agents/skills/rust-systems-expert/SKILL.md")
            .exists()
    );
    assert!(
        !env.workspace
            .join(".claude/skills/rust-systems-expert/SKILL.md")
            .exists()
    );
    assert!(
        !env.workspace
            .join(".cursor/rules/rust-systems-expert.mdc")
            .exists()
    );
    assert!(
        !env.workspace
            .join(".skx/skills/rust-systems-expert")
            .exists()
    );

    // The shared Copilot file survives, but skx's marked region is gone.
    let copilot_contents =
        std::fs::read_to_string(env.workspace.join(".github/copilot-instructions.md")).unwrap();
    assert!(!copilot_contents.contains("skx:start"));

    // Audit should now be clean — nothing orphaned in state.
    let audit_out = run(&env.workspace, &env.home, &["audit"]);
    assert!(
        stdout(&audit_out).contains("Everything in sync"),
        "{}",
        stdout(&audit_out)
    );

    // And it's really gone from the manifest.
    let list_out = run(&env.workspace, &env.home, &["list"]);
    assert!(stdout(&list_out).contains("No skills installed"));
}

#[test]
fn add_with_global_flag_installs_outside_the_workspace() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    let output = run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap(), "--global"],
    );
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(
        env.home
            .join(".config/skx/skills/rust-systems-expert/SKILL.md")
            .is_file()
    );
    assert!(
        !env.workspace
            .join(".skx/skills/rust-systems-expert/SKILL.md")
            .exists()
    );
}

#[test]
fn export_writes_real_files_never_symlinks() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap(), "--global"],
    );

    let output = run(&env.workspace, &env.home, &["export"]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(stdout(&output).contains("Exported 1 skill(s)"));

    let export_dir = env.workspace.join("skx-export");
    for relative in [
        ".agents/skills/rust-systems-expert/SKILL.md",
        ".claude/skills/rust-systems-expert/SKILL.md",
        ".cursor/rules/rust-systems-expert.mdc",
        ".github/copilot-instructions.md",
        ".vscode/mcp.json",
    ] {
        let path = export_dir.join(relative);
        assert!(
            path.is_file(),
            "expected {relative} to exist and be a real file"
        );
        assert!(
            path.symlink_metadata().unwrap().file_type().is_file(),
            "{relative} must be a real file, not a symlink — a static export can't \
             depend on the cache it was compiled from still existing"
        );
    }

    // The exported skill file is fully self-contained canonical content,
    // not a reference back to the (global-scope) cache it came from.
    let claude_contents =
        std::fs::read_to_string(export_dir.join(".claude/skills/rust-systems-expert/SKILL.md"))
            .unwrap();
    assert!(claude_contents.contains("Prefer zero-cost abstractions"));

    // Export must not touch the live sync state.
    assert!(!env.workspace.join(".skx/state.toml").exists());
}

#[test]
fn export_respects_custom_out_dir() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap()],
    );

    let output = run(&env.workspace, &env.home, &["export", "--out", "dist"]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(
        env.workspace
            .join("dist/.claude/skills/rust-systems-expert/SKILL.md")
            .is_file()
    );
    assert!(!env.workspace.join("skx-export").exists());
}

#[test]
fn discover_reports_nothing_in_a_fresh_workspace() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    let output = run(&env.workspace, &env.home, &["discover"]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(stdout(&output).contains("No unmanaged skills found"));
}

#[test]
fn discover_finds_a_hand_installed_skill_without_registering_it() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);

    // Simulate a skill installed by hand (or by Claude Code directly)
    // before skx ever touched this machine — a real file, not a symlink,
    // sitting where Claude Code itself would look for it.
    let hand_installed = env.workspace.join(".claude/skills/hand-installed/SKILL.md");
    std::fs::create_dir_all(hand_installed.parent().unwrap()).unwrap();
    std::fs::write(
        &hand_installed,
        SAMPLE_SKILL.replace("rust-systems-expert", "hand-installed"),
    )
    .unwrap();

    let output = run(&env.workspace, &env.home, &["discover"]);
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("hand-installed"), "{text}");
    assert!(text.contains("1 unmanaged skill(s) found"), "{text}");
    // SAMPLE_SKILL already declares claude_code, so nothing to auto-add.
    assert!(!text.contains("would declare"), "{text}");

    // Report-only: nothing should actually be registered.
    let list_out = run(&env.workspace, &env.home, &["list"]);
    assert!(stdout(&list_out).contains("No skills installed"));
}

#[test]
fn discover_flags_a_naming_conflict_across_two_locations() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);

    let local = env.workspace.join(".claude/skills/dup-skill/SKILL.md");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(
        &local,
        SAMPLE_SKILL.replace("rust-systems-expert", "dup-skill"),
    )
    .unwrap();

    let global = env.home.join(".claude/skills/dup-skill/SKILL.md");
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    std::fs::write(
        &global,
        SAMPLE_SKILL
            .replace("rust-systems-expert", "dup-skill")
            .replace("1.0.0", "2.0.0"),
    )
    .unwrap();

    let output = run(&env.workspace, &env.home, &["discover"]);
    let text = stdout(&output);
    assert!(text.contains("conflict"), "{text}");
    assert!(text.contains("1 naming conflict"), "{text}");
}

#[test]
fn discover_hints_at_the_target_it_would_auto_declare() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);

    // A realistic hand-installed skill: no `targets:` block, since that's
    // an skx-only concept it never had before skx existed.
    let bare = env.workspace.join(".claude/skills/bare-skill/SKILL.md");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    std::fs::write(
        &bare,
        "---\nname: bare-skill\ndescription: no targets declared\nversion: 1.0.0\n---\nbody\n",
    )
    .unwrap();

    let output = run(&env.workspace, &env.home, &["discover"]);
    let text = stdout(&output);
    assert!(
        text.contains("would declare claude_code on import"),
        "{text}"
    );
}

/// A skill is a directory, so `scripts/`, `references/` and `assets/` have
/// to travel with it — the body refers to them by relative path.
#[test]
fn bundled_directories_are_cached_and_linked() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);

    let source_dir = env.skill_src.clone();
    std::fs::create_dir_all(source_dir.join("references")).unwrap();
    std::fs::write(source_dir.join("references/deep.md"), "reference body").unwrap();
    std::fs::create_dir_all(source_dir.join("scripts")).unwrap();
    std::fs::write(source_dir.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
    // Not a spec directory: a skill authored inside a checkout shouldn't
    // drag its VCS metadata into the cache.
    std::fs::create_dir_all(source_dir.join(".git")).unwrap();
    std::fs::write(source_dir.join(".git/HEAD"), "ref: refs/heads/main").unwrap();

    let output = run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap()],
    );
    assert!(output.status.success(), "{}", stdout(&output));

    let cache = env.workspace.join(".skx/skills/rust-systems-expert");
    assert!(cache.join("references/deep.md").is_file());
    assert!(cache.join("scripts/run.sh").is_file());
    assert!(
        !cache.join(".git").exists(),
        "must not vacuum up the checkout"
    );

    run(&env.workspace, &env.home, &["sync"]);

    // The linked destination resolves the bundled files, which is the whole
    // point: before this, only SKILL.md was linked and every relative
    // reference in the body dangled.
    let linked = env.workspace.join(".claude/skills/rust-systems-expert");
    assert_eq!(
        std::fs::read_to_string(linked.join("references/deep.md")).unwrap(),
        "reference body"
    );
    assert!(linked.join("SKILL.md").is_file());
}

/// The migration hazard: a workspace synced by an older skx has a *real*
/// directory at the destination holding the user's own files beside skx's
/// SKILL.md symlink. Replacing it wholesale would delete them.
#[test]
fn syncing_over_a_legacy_layout_rescues_user_files_instead_of_deleting_them() {
    let env = setup();
    run(&env.workspace, &env.home, &["init"]);
    run(
        &env.workspace,
        &env.home,
        &["add", env.skill_src.to_str().unwrap()],
    );

    // Reconstruct the old layout by hand: a real directory containing a
    // SKILL.md symlink into the cache, plus content skx never wrote.
    let legacy = env.workspace.join(".claude/skills/rust-systems-expert");
    std::fs::create_dir_all(legacy.join("references")).unwrap();
    std::fs::write(legacy.join("references/notes.md"), "hand-written notes").unwrap();
    std::fs::write(legacy.join(".agent_version"), "7").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        env.workspace
            .join(".skx/skills/rust-systems-expert/SKILL.md"),
        legacy.join("SKILL.md"),
    )
    .unwrap();

    let output = run(&env.workspace, &env.home, &["sync"]);
    assert!(output.status.success(), "{}", stdout(&output));

    // Rescued into the cache...
    let cache = env.workspace.join(".skx/skills/rust-systems-expert");
    assert_eq!(
        std::fs::read_to_string(cache.join("references/notes.md")).unwrap(),
        "hand-written notes",
        "user files must survive the migration"
    );
    assert_eq!(
        std::fs::read_to_string(cache.join(".agent_version")).unwrap(),
        "7"
    );

    // ...and therefore still reachable at the destination afterwards.
    assert_eq!(
        std::fs::read_to_string(legacy.join("references/notes.md")).unwrap(),
        "hand-written notes"
    );

    // And skx says so rather than moving files silently.
    assert!(
        stdout(&output).contains("adopted"),
        "the migration must be reported: {}",
        stdout(&output)
    );
}
