use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_job-harness"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env(
            "XDG_CONFIG_HOME",
            root.parent().unwrap().join("host-settings"),
        )
        .output()
        .unwrap()
}
fn output(out: Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("Invalid stdout: {}", String::from_utf8_lossy(&out.stdout)))
}

fn call(root: &Path, tool: &str, arguments: Value) -> Value {
    let input = root.parent().unwrap().join("tool-input.json");
    std::fs::write(&input, arguments.to_string()).unwrap();
    output(run(
        root,
        &["call", tool, "--input", input.to_str().unwrap()],
    ))
}

#[test]
fn imported_pdf_can_be_explicitly_selected_and_frozen_without_weakening_submit_guards() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let config_path = root.join("config/career-ledger.json");
    let initial_config = std::fs::read(&config_path).unwrap();
    let supplied = temp.path().join("Fictional résumé.PDF");
    let pdf = include_bytes!("../../../examples/cv/example.pdf");
    std::fs::write(&supplied, pdf).unwrap();
    let imported = output(run(
        &root,
        &["source", "import", "--input", supplied.to_str().unwrap()],
    ));
    assert_eq!(imported["ok"], true, "{imported}");
    assert_eq!(imported["data"]["primary_cv_changed"], false);
    assert_eq!(imported["data"]["facts_extracted"], false);
    assert_eq!(std::fs::read(&config_path).unwrap(), initial_config);
    let retained = imported["data"]["path"].as_str().unwrap();
    let provenance_path = root.join(imported["data"]["provenance_path"].as_str().unwrap());
    let provenance = std::fs::read(&provenance_path).unwrap();
    let mut config: Value = serde_json::from_slice(&initial_config).unwrap();
    config["primary_cv"] = json!(retained);
    config["permissions"]["auto_apply_portal"] = json!(true);
    std::fs::write(&config_path, config.to_string()).unwrap();
    let bootstrap = call(&root, "bootstrap", json!({"operation":"apply"}));
    assert_eq!(bootstrap["ok"], true, "{bootstrap}");
    assert_eq!(bootstrap["data"]["settings"]["primary_cv"], retained);
    assert!(
        bootstrap["data"]["configuration_issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|issue| issue["code"] != "missing_primary_cv")
    );
    assert_eq!(
        job_harness_core::workspace::validate(&root).unwrap()["primary_cv_present"],
        true
    );
    let run = call(
        &root,
        "start_run",
        json!({"operation":"apply","scope":{"sources":["employer"],"max_submissions":1,"dry_run":false},"user_request":"Synthetic internal reservation verification only; no external submission","idempotency_key":"import-run"}),
    );
    assert_eq!(run["ok"], true, "{run}");
    let run_id = run["data"]["run_id"].clone();
    let created = call(
        &root,
        "create_application",
        json!({"run_id":run_id,"record":"applications/example-engineer-123.md","company":"Example","role":"Engineer","vacancy_url":"https://jobs.example.invalid/example/123","source":"employer","posting_id":"123","fit_reason":"Fictional vacancy for a local fixture","evidence":["https://jobs.example.invalid/example/123"],"idempotency_key":"import-vacancy"}),
    );
    assert_eq!(created["ok"], true, "{created}");
    std::fs::write(root.join("applications/answers.json"), "{\"fixture\":true}").unwrap();
    let arguments = json!({"run_id":run_id,"record":"applications/example-engineer-123.md","company":"Example","vacancy_url":"https://jobs.example.invalid/example/123","source":"employer","posting_id":"123","cv_path":retained,"answers_path":"applications/answers.json","vacancy_text_unavailable":"Synthetic CLI fixture without a written ad","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"channel":"portal","idempotency_key":"import-packet"});
    let packet = call(&root, "prepare_packet", arguments.clone());
    assert_eq!(packet["ok"], true, "{packet}");
    let cv = &packet["data"]["packet"]["cv"];
    assert_eq!(cv["original_path"], retained);
    assert_eq!(cv["sha256"], imported["data"]["sha256"]);
    assert_eq!(
        std::fs::read(root.join(cv["path"].as_str().unwrap())).unwrap(),
        pdf
    );
    assert!(cv["path"].as_str().unwrap().ends_with(".pdf"));
    let replay = call(&root, "prepare_packet", arguments.clone());
    assert_eq!(replay["ok"], true, "{replay}");
    assert_eq!(replay["data"]["packet_id"], packet["data"]["packet_id"]);
    let state_before = job_harness_core::state::load(&root).unwrap();
    std::fs::write(root.join("sources/unimported.pdf"), pdf).unwrap();
    for denied in [
        "sources/unimported.pdf".to_owned(),
        format!(
            "sources/imports/{}/../../unimported.pdf",
            imported["data"]["sha256"].as_str().unwrap()
        ),
    ] {
        let mut denied_arguments = arguments.clone();
        denied_arguments["cv_path"] = json!(denied);
        denied_arguments["idempotency_key"] = json!(format!("deny:{denied}"));
        assert_eq!(
            call(&root, "prepare_packet", denied_arguments)["error"]["code"],
            "path_denied"
        );
    }
    #[cfg(unix)]
    {
        std::fs::remove_file(root.join(retained)).unwrap();
        std::os::unix::fs::symlink(&supplied, root.join(retained)).unwrap();
        assert_eq!(
            call(&root, "bootstrap", json!({"operation":"apply"}))["error"]["code"],
            "path_denied"
        );
        let mut denied_arguments = arguments.clone();
        denied_arguments["idempotency_key"] = json!("deny-outside-symlink");
        assert_eq!(
            call(&root, "prepare_packet", denied_arguments)["error"]["code"],
            "path_denied"
        );
        assert_eq!(std::fs::read(&supplied).unwrap(), pdf);
        std::fs::remove_file(root.join(retained)).unwrap();
        std::fs::write(root.join(retained), pdf).unwrap();
    }
    std::fs::write(root.join(retained), b"%PDF-1.7\nchanged original\n").unwrap();
    assert_eq!(
        call(&root, "bootstrap", json!({"operation":"apply"}))["error"]["code"],
        "artifact_changed"
    );
    assert_eq!(
        call(&root, "prepare_packet", arguments.clone())["error"]["code"],
        "artifact_changed"
    );
    assert_eq!(job_harness_core::state::load(&root).unwrap(), state_before);
    std::fs::write(root.join(retained), pdf).unwrap();
    let mut combined_arguments = arguments;
    combined_arguments["idempotency_key"] = json!("import-reserve");
    let reserved = call(&root, "prepare_application", combined_arguments.clone());
    assert_eq!(reserved["ok"], true, "{reserved}");
    let begin_arguments =
        json!({"attempt_id":reserved["data"]["attempt_id"],"idempotency_key":"import-begin"});
    assert_eq!(
        call(&root, "begin_submission", begin_arguments.clone())["data"]["ready_for_single_submit"],
        true
    );
    assert_eq!(
        call(&root, "begin_submission", begin_arguments)["data"]["ready_for_single_submit"],
        false
    );
    let unknown = call(
        &root,
        "finish_submission",
        json!({"attempt_id":reserved["data"]["attempt_id"],"outcome":"outcome_unknown","summary":"Synthetic lost response; no external submission occurred","evidence":[],"idempotency_key":"import-unknown"}),
    );
    assert_eq!(unknown["ok"], true, "{unknown}");
    combined_arguments["idempotency_key"] = json!("import-never-retry");
    assert_eq!(
        call(&root, "prepare_application", combined_arguments)["error"]["code"],
        "duplicate_blocked"
    );
    assert_eq!(std::fs::read(root.join(retained)).unwrap(), pdf);
    assert_eq!(std::fs::read(provenance_path).unwrap(), provenance);
}

#[test]
fn native_cli_keeps_compact_contract_and_workspace_outside_code() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("career");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let bootstrap = output(run(&root, &["call", "bootstrap"]));
    assert_eq!(bootstrap["schema_version"], 2);
    assert_eq!(bootstrap["ok"], true);
    let catalog = output(run(&root, &["schemas"]));
    assert_eq!(
        catalog.as_object().unwrap().len(),
        job_harness_core::contracts::specs().len()
    );
    let config = output(run(&root, &["connect", "mcp"]));
    assert_eq!(config["ok"], true);
    assert_eq!(
        config["data"]["args"],
        json!([
            "--root",
            root.canonicalize().unwrap(),
            "mcp",
            "--tool-profile",
            "focused"
        ])
    );
    assert!(
        Path::new(config["data"]["command"].as_str().unwrap())
            .file_stem()
            .is_some_and(|stem| stem == "job-harness")
    );
    let failed = run(&root, &["call", "unknown_tool"]);
    assert!(!failed.status.success());
    assert_eq!(output(failed)["error"]["code"], "unknown_tool");
}

#[test]
fn daily_check_is_read_only_composable_and_matches_the_engine() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("career");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let state_before = job_harness_core::state::load(&root).unwrap();
    let config_path = root.join("config/career-ledger.json");
    let config_before = std::fs::read(&config_path).unwrap();
    let args = json!({"offset":0,"limit":2});
    let expected = job_harness_core::Engine::open(&root)
        .unwrap()
        .call("daily_check", &args);
    assert_eq!(expected["ok"], true, "{expected}");
    let input_path = temp.path().join("daily-check.json");
    std::fs::write(&input_path, args.to_string()).unwrap();
    let checked = output(run(
        &root,
        &[
            "daily",
            "check",
            "--input",
            input_path.to_str().unwrap(),
            "--limit",
            "2",
        ],
    ));
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(checked["schema_version"], 2);
    assert_eq!(checked["data"], expected["data"]);
    assert_eq!(job_harness_core::state::load(&root).unwrap(), state_before);
    assert_eq!(std::fs::read(&config_path).unwrap(), config_before);
    let conflict = run(
        &root,
        &[
            "daily",
            "check",
            "--input",
            input_path.to_str().unwrap(),
            "--limit",
            "3",
        ],
    );
    assert!(!conflict.status.success());
    assert_eq!(output(conflict)["error"]["code"], "invalid_arguments");
    assert_eq!(job_harness_core::state::load(&root).unwrap(), state_before);
}

#[test]
fn daily_update_requires_explicit_valid_input_and_preserves_state_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("career");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let before = job_harness_core::state::load(&root).unwrap();
    let empty = run(&root, &["daily", "update"]);
    assert!(!empty.status.success());
    assert_eq!(output(empty)["error"]["code"], "invalid_arguments");
    let file = temp.path().join("bad-daily.json");
    std::fs::write(&file, "[]").unwrap();
    let malformed = run(
        &root,
        &["daily", "update", "--input", file.to_str().unwrap()],
    );
    assert!(!malformed.status.success());
    assert_eq!(output(malformed)["error"]["code"], "invalid_arguments");
    assert_eq!(job_harness_core::state::load(&root).unwrap(), before);
}

#[test]
fn daily_check_accepts_inline_host_observations_without_persisting_them() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("career");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let before = job_harness_core::state::load(&root).unwrap();
    let receipts = root.join("applications/operations/receipts");
    let receipt_count = receipts.read_dir().unwrap().count();
    let snapshot: Value = serde_json::from_str(include_str!(
        "../../../examples/daily-review/observations.json"
    ))
    .unwrap();
    let file = temp.path().join("inline-check.json");
    let mut args = json!({"mail_observations":snapshot});
    std::fs::write(&file, args.to_string()).unwrap();
    let checked = output(run(
        &root,
        &["daily", "check", "--input", file.to_str().unwrap()],
    ));
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(
        checked["data"]["observations"]["source_state"],
        "supplied_inline"
    );
    assert_eq!(checked["data"]["observations"]["live_checked"], false);
    assert_eq!(
        checked["data"]["communications"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        checked["data"]["archive_plan"]["actions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        checked["data"]["attention"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["message_id"] == "recruiter-1")
    );
    assert_eq!(job_harness_core::state::load(&root).unwrap(), before);
    assert_eq!(receipts.read_dir().unwrap().count(), receipt_count);
    args["mail_observations"]["items"][0]["text"] = json!("Changed supplied text");
    std::fs::write(&file, args.to_string()).unwrap();
    let corrupted = run(
        &root,
        &["daily", "check", "--input", file.to_str().unwrap()],
    );
    assert!(!corrupted.status.success());
    assert_eq!(output(corrupted)["error"]["code"], "artifact_changed");
    assert_eq!(job_harness_core::state::load(&root).unwrap(), before);
    assert_eq!(receipts.read_dir().unwrap().count(), receipt_count);
}

#[test]
fn host_default_roundtrip_and_invalid_input_are_json() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let installed = temp.path().join(if cfg!(windows) {
        "installed.exe"
    } else {
        "installed"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_job-harness"), &installed).unwrap();
    let configured = Command::new(&installed)
        .current_dir(temp.path())
        .args(["configure", "--workspace", root.to_str().unwrap()])
        .env("XDG_CONFIG_HOME", temp.path().join("host-settings"))
        .output()
        .unwrap();
    assert_eq!(output(configured)["ok"], true);
    let default = Command::new(&installed)
        .current_dir(temp.path())
        .args(["call", "bootstrap"])
        .env("XDG_CONFIG_HOME", temp.path().join("host-settings"))
        .env_remove("JOB_HARNESS_WORKSPACE")
        .output()
        .unwrap();
    assert_eq!(output(default)["ok"], true);
    let bad = temp.path().join("bad.json");
    std::fs::write(&bad, "[]").unwrap();
    let result = run(
        &root,
        &["call", "bootstrap", "--input", bad.to_str().unwrap()],
    );
    assert!(!result.status.success());
    assert_eq!(output(result)["error"]["code"], "invalid_arguments");
}

#[test]
fn source_default_ignores_live_environment_and_saved_settings() {
    let temp = tempfile::tempdir().unwrap();
    let personal = temp.path().join("fictional-personal");
    assert_eq!(output(run(&personal, &["init"]))["ok"], true);
    let before = job_harness_core::state::load(&personal).unwrap();
    let settings = temp.path().join("host-settings/job-harness/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let settings_bytes = json!({"schema_version":1,"workspace":personal}).to_string();
    std::fs::write(&settings, &settings_bytes).unwrap();
    let selected = job_harness::host::resolve(None).unwrap();
    assert_ne!(selected, personal);
    let result = Command::new(env!("CARGO_BIN_EXE_job-harness"))
        .args(["call", "bootstrap"])
        .current_dir(temp.path())
        .env("JOB_HARNESS_WORKSPACE", &personal)
        .env("XDG_CONFIG_HOME", temp.path().join("host-settings"))
        .output()
        .unwrap();
    let value = output(result);
    if value["ok"] == true {
        assert_eq!(
            value["data"]["ledger"],
            json!(selected.canonicalize().unwrap())
        );
    } else {
        // A missing or unmigrated development fixture must fail, never fall back to the live one.
        assert!(
            ["invalid_arguments", "missing_resource", "invalid_state"]
                .contains(&value["error"]["code"].as_str().unwrap())
        );
    }
    assert_eq!(job_harness_core::state::load(&personal).unwrap(), before);
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), settings_bytes);
    let installed = temp.path().join(if cfg!(windows) {
        "copied.exe"
    } else {
        "copied"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_job-harness"), &installed).unwrap();
    let in_checkout = Command::new(installed)
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap(),
        )
        .args(["call", "bootstrap"])
        .env("JOB_HARNESS_WORKSPACE", &personal)
        .env("XDG_CONFIG_HOME", temp.path().join("host-settings"))
        .output()
        .unwrap();
    let value = output(in_checkout);
    if value["ok"] == true {
        assert_ne!(value["data"]["ledger"], json!(personal));
    } else {
        assert!(
            ["invalid_arguments", "missing_resource", "invalid_state"]
                .contains(&value["error"]["code"].as_str().unwrap())
        );
    }
    assert_eq!(job_harness_core::state::load(&personal).unwrap(), before);
    assert_eq!(
        job_harness::host::resolve(Some(personal.clone())).unwrap(),
        personal.canonicalize().unwrap()
    );
}

#[test]
fn notes_cli_preserves_original_and_pipeline_check_has_no_events() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let original = temp.path().join("supplied.md");
    let text = "# Supplied notes\nA candidate supplied an example; dates are unknown.\n";
    std::fs::write(&original, text).unwrap();
    let imported = output(run(
        &root,
        &[
            "notes",
            "import",
            "--input",
            original.to_str().unwrap(),
            "--key",
            "notes-cli-fixture",
        ],
    ));
    assert_eq!(imported["ok"], true);
    let id = imported["data"]["note_id"].as_str().unwrap();
    let review = output(run(&root, &["notes", "review", id, "--include-original"]));
    assert_eq!(review["data"]["original"]["text"], text);
    assert_eq!(output(run(&root, &["pipeline"]))["ok"], true);
    let before = job_harness_core::state::load(&root).unwrap();
    assert_eq!(output(run(&root, &["pipeline", "--check"]))["ok"], true);
    assert_eq!(job_harness_core::state::load(&root).unwrap(), before);
    let models = Command::new(env!("CARGO_BIN_EXE_job-harness"))
        .args(["context", "models"])
        .env_remove("JOB_HARNESS_WORKSPACE")
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join("unconfigured-host"))
        .output()
        .unwrap();
    assert_eq!(output(models)["ok"], true);
}

#[test]
fn presentation_cli_preserves_supplied_sources_and_keeps_templates_fact_free() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let source = temp.path().join("supplied.txt");
    std::fs::write(&source, "A supplied example.\nUnselected detail.\n").unwrap();
    let imported = output(run(
        &root,
        &["source", "import", "--input", source.to_str().unwrap()],
    ));
    assert_eq!(imported["ok"], true);
    let template = output(run(
        &temp.path().join("not-initialized"),
        &["document", "template", "--kind", "interview_story"],
    ));
    assert_eq!(template["status"], "draft");
    assert_eq!(template["sections"][0]["text"], "");
    assert!(!temp.path().join("not-initialized").exists());
    let selection = json!({"schema_version":1,"title":"Supplied example","status":"draft","sections":[{"heading":"Story","text":"A supplied example.","citations":[{"path":imported["data"]["path"],"sha256":imported["data"]["sha256"],"start_line":1,"end_line":1}]}]});
    let input = temp.path().join("selection.json");
    std::fs::write(&input, selection.to_string()).unwrap();
    let rendered = output(run(
        &root,
        &[
            "document",
            "render",
            "--input",
            input.to_str().unwrap(),
            "--output",
            "output/story.md",
        ],
    ));
    assert_eq!(rendered["ok"], true);
    assert_eq!(rendered["data"]["citations_checked"], 1);
    let bytes = std::fs::read_to_string(root.join("output/story.md")).unwrap();
    assert!(bytes.contains("A supplied example."));
    assert!(!bytes.contains("Unselected detail."));
    assert_eq!(
        output(run(&root, &["workspace", "outcomes"]))["data"]["counts"]["total"],
        0
    );
    let human = run(&root, &["status"]);
    assert!(human.status.success());
    assert!(
        String::from_utf8(human.stdout)
            .unwrap()
            .contains("Job Harness · Workspace")
    );
    assert_eq!(
        output(run(&root, &["status", "--json"]))["data"]["health"]["records"],
        0
    );
}

#[cfg(windows)]
#[test]
fn windows_host_settings_use_appdata_and_preserve_drive_paths_with_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("career workspace with spaces");
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    let appdata = temp.path().join("Host Settings");
    let installed = temp.path().join("installed.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_job-harness"), &installed).unwrap();
    let configured = Command::new(&installed)
        .current_dir(temp.path())
        .args(["configure", "--workspace"])
        .arg(&root)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env("APPDATA", &appdata)
        .output()
        .unwrap();
    assert_eq!(output(configured)["ok"], true);
    assert!(appdata.join("job-harness/settings.json").is_file());
    let selected = Command::new(&installed)
        .current_dir(temp.path())
        .args(["call", "bootstrap"])
        .env_remove("JOB_HARNESS_WORKSPACE")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env("APPDATA", &appdata)
        .output()
        .unwrap();
    assert_eq!(
        output(selected)["data"]["ledger"],
        serde_json::json!(root.canonicalize().unwrap())
    );
}

#[test]
fn a_workspace_pin_refuses_a_copied_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let pinned = temp.path().join("workspace");
    let copy = temp.path().join("copy");
    assert_eq!(output(run(&pinned, &["init"]))["ok"], true);
    assert_eq!(output(run(&copy, &["init"]))["ok"], true);
    let pin = temp.path().join("allowed-workspaces");
    std::fs::write(&pin, format!("{}\n", pinned.display())).unwrap();
    let input = temp.path().join("tool-input.json");
    std::fs::write(&input, "{}").unwrap();
    let pinned_run = |root: &Path, pin: &Path, args: &[&str]| {
        output(
            Command::new(env!("CARGO_BIN_EXE_job-harness"))
                .arg("--root")
                .arg(root)
                .args(args)
                .env("XDG_CONFIG_HOME", temp.path().join("host-settings"))
                .env("JOB_HARNESS_WORKSPACE_PIN", pin)
                .output()
                .unwrap(),
        )
    };
    let bootstrap = ["call", "bootstrap", "--input", input.to_str().unwrap()];
    assert_eq!(pinned_run(&pinned, &pin, &bootstrap)["ok"], true);
    // Every command that opens a workspace honours the pin, not only tool calls.
    let bundle = temp.path().join("copy.jhb");
    for args in [
        &bootstrap[..],
        &["workspace", "search", "anything"],
        &["workspace", "overview"],
        &["workspace", "export", "--output", bundle.to_str().unwrap()],
        &["pipeline", "--check"],
        &["status", "--json"],
        &["sync"],
    ] {
        let refused = pinned_run(&copy, &pin, args);
        assert_eq!(
            refused["error"]["code"], "workspace_not_pinned",
            "{args:?}: {refused}"
        );
    }
    assert!(!bundle.exists());
    // A pin named in the environment must exist; a typo does not open everything.
    let missing = pinned_run(&pinned, &temp.path().join("no-such-pin"), &bootstrap);
    assert_eq!(
        missing["error"]["code"], "workspace_not_pinned",
        "{missing}"
    );
}

#[test]
fn sync_keeps_a_configured_name_and_fills_in_only_the_missing_email() {
    let temp = tempfile::tempdir().unwrap();
    let (remote, root) = (
        temp.path().join("remote.git"),
        temp.path().join("workspace"),
    );
    let empty = temp.path().join("empty-gitconfig");
    std::fs::write(&empty, "").unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &empty)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(
        temp.path(),
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    assert_eq!(output(run(&root, &["init"]))["ok"], true);
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "Ledger Owner"]);
    git(
        &root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(
        &root,
        &[
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ],
    );
    git(&root, &["push", "-q", "-u", "origin", "HEAD"]);
    let synced = Command::new(env!("CARGO_BIN_EXE_job-harness"))
        .arg("--root")
        .arg(&root)
        .arg("sync")
        .env("XDG_CONFIG_HOME", temp.path().join("host-settings"))
        .env("GIT_CONFIG_GLOBAL", &empty)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let synced = output(synced);
    assert_eq!(synced["data"]["committed"], true, "{synced}");
    assert_eq!(
        git(&root, &["log", "-1", "--format=%an <%ae>"]),
        "Ledger Owner <job-harness@localhost>"
    );
}

#[test]
fn configure_remote_names_a_missing_token_file() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("no-such-token");
    let refused = output(
        Command::new(env!("CARGO_BIN_EXE_job-harness"))
            .args([
                "configure",
                "--remote",
                "http://127.0.0.1:9",
                "--token-file",
            ])
            .arg(&missing)
            .env("XDG_CONFIG_HOME", temp.path().join("host-settings"))
            .output()
            .unwrap(),
    );
    assert_eq!(refused["error"]["code"], "token_required", "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no-such-token"),
        "{refused}"
    );
}
