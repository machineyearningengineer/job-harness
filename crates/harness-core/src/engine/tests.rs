use super::*;
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in ["applications", "context/profile", "output", "config"] {
        fs::create_dir_all(dir.path().join(name)).unwrap();
    }
    fs::write(
        dir.path().join("config/career-ledger.json"),
        r#"{"schema_version":1,"timezone":"UTC","primary_cv":"output/cv.pdf","permissions":{"auto_apply_portal":true}}"#,
    )
    .unwrap();
    fs::write(dir.path().join("applications/acme-engineer-123.md"),"# Acme — Senior Engineer\n\n- Stage: saved\n- Source URL: https://jobs.example.com/acme/123?utm_source=old\n").unwrap();
    fs::write(
        dir.path().join("applications/approved-answers.md"),
        "# Answers\n\n## Salary\nConfirmed salary fixture.\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("context/profile/application-decisions.md"),
        "# Decisions\n\n## Experience\nSourced fixture.\n",
    )
    .unwrap();
    fs::write(dir.path().join("output/cv.pdf"), b"%PDF-1.7\nfixture\n").unwrap();
    fs::write(
        dir.path().join("applications/answers.json"),
        r#"{"experience":6}"#,
    )
    .unwrap();
    fs::write(dir.path().join("applications/vacancy.md"), "# Example — Engineer\n\nSynthetic job ad fixture. Build and operate backend services, own features end to end, write tests, review changes and work with product on requirements. Requirements: production experience, testing discipline, clear communication.\n").unwrap();
    dir
}
fn call(h: &Engine, tool: &str, args: Value) -> Value {
    let value = h.call(tool, &args);
    assert_eq!(value["ok"], true, "{tool}: {value}");
    contracts::validate(&contracts::specs()[tool]["outputSchema"], &value).unwrap();
    value["data"].clone()
}
fn start(h: &Engine, max: usize) -> String {
    call(h,"start_run",json!({"operation":"apply","scope":{"sources":["employer"],"max_submissions":max,"dry_run":false},"user_request":"Synthetic test","idempotency_key":"run"}))["run_id"].as_str().unwrap().into()
}
fn prepare(h: &Engine, run: &str, key: &str) -> Value {
    call(
        h,
        "prepare_application",
        json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":key}),
    )
}
fn set_daily_budget(h: &Engine, limit: Value) {
    fs::write(
        h.root.join("config/career-ledger.json"),
        json!({"schema_version":1,"timezone":"UTC","primary_cv":"output/cv.pdf","max_daily_submissions":limit,"permissions":{"auto_apply_portal":true}}).to_string(),
    )
    .unwrap();
}

fn prepare_second_run(h: &Engine) -> Value {
    fs::write(h.root.join("applications/example-engineer-456.md"), "# Example — Engineer\n\n- Stage: saved\n- Source URL: https://jobs.example.com/example/456\n").unwrap();
    let run = call(
        h,
        "start_run",
        json!({"operation":"apply","scope":{"sources":["employer"],"max_submissions":1,"dry_run":false},"user_request":"Second synthetic worker","idempotency_key":"run2"}),
    );
    call(
        h,
        "prepare_application",
        json!({"run_id":run["run_id"],"record":"applications/example-engineer-456.md","company":"Example","vacancy_url":"https://jobs.example.com/example/456","source":"employer","posting_id":"456","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":"prepare2"}),
    )
}

#[test]
fn daily_submission_budget_fails_closed_on_corrupt_begin_history() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let first = prepare(&h, &run, "prepare");
    let second = prepare_second_run(&h);
    call(
        &h,
        "begin_submission",
        json!({"attempt_id":first["attempt_id"],"idempotency_key":"begin-1"}),
    );
    let first_id = first["attempt_id"].as_str().unwrap();
    let original = h.load_state().unwrap();
    set_daily_budget(&h, json!(3));
    let request = json!({"attempt_id":second["attempt_id"],"idempotency_key":"begin-2"});
    for defect in [
        "bad timestamp",
        "missing timestamp",
        "cancelled without timestamp",
        "missing attempt",
        "missing event identity",
    ] {
        let mut state = original.clone();
        match defect {
            "bad timestamp" => state["attempts"][first_id]["began_at"] = json!("not a timestamp"),
            "missing timestamp" | "cancelled without timestamp" => {
                let attempt = &mut state["attempts"][first_id];
                attempt.as_object_mut().unwrap().remove("began_at");
                if defect == "cancelled without timestamp" {
                    attempt["status"] = json!("cancelled");
                }
            }
            "missing attempt" => {
                state["attempts"].as_object_mut().unwrap().remove(first_id);
            }
            "missing event identity" => {
                state["events"].as_array_mut().unwrap().last_mut().unwrap()["result"]["attempt_id"] =
                    Value::Null;
            }
            _ => unreachable!(),
        }
        h.commit(&mut state, &[]).unwrap();
        let before = h.load_state().unwrap();
        let result = h.call("begin_submission", &request);
        assert_eq!(
            result["error"]["code"], "invalid_state",
            "{defect}: {result}"
        );
        assert_eq!(before, h.load_state().unwrap(), "{defect}");
    }
}

#[test]
fn daily_submission_budget_restores_capacity_without_expiring_unknown_attempts() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    set_daily_budget(&h, json!(1));
    let run = start(&h, 1);
    let first = prepare(&h, &run, "prepare");
    let second = prepare_second_run(&h);
    call(
        &h,
        "begin_submission",
        json!({"attempt_id":first["attempt_id"],"idempotency_key":"begin-1"}),
    );
    call(
        &h,
        "finish_submission",
        json!({"attempt_id":first["attempt_id"],"outcome":"outcome_unknown","summary":"Lost response","evidence":[],"idempotency_key":"unknown"}),
    );
    let begin_second = json!({"attempt_id":second["attempt_id"],"idempotency_key":"begin-2"});
    assert_eq!(
        h.call("begin_submission", &begin_second)["error"]["code"],
        "daily_submission_cap_reached"
    );

    // Age this synthetic attempt's begin date, not its outcome or original reservation.
    let mut state = h.load_state().unwrap();
    let first_id = first["attempt_id"].as_str().unwrap();
    state["attempts"][first_id]["began_at"] =
        json!((chrono::Utc::now() - chrono::Days::new(1)).to_rfc3339());
    h.commit(&mut state, &[]).unwrap();
    assert_eq!(
        call(&h, "begin_submission", begin_second)["ready_for_single_submit"],
        true
    );
    assert_eq!(
        h.load_state().unwrap()["attempts"][first_id]["status"],
        "outcome_unknown"
    );
}

#[test]
fn daily_submission_budget_grants_only_one_concurrent_begin_across_runs() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    set_daily_budget(&h, json!(1));
    let run = start(&h, 1);
    let first = prepare(&h, &run, "prepare");
    let second = prepare_second_run(&h);
    let requests = [first, second].map(|p| {
        json!({"attempt_id":p["attempt_id"],"idempotency_key":format!("begin:{}", p["attempt_id"])})
    });
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        requests
            .each_ref()
            .map(|args| {
                scope.spawn(|| {
                    let worker = Engine::open(dir.path()).unwrap();
                    barrier.wait();
                    worker.call("begin_submission", args)
                })
            })
            .map(|handle| handle.join().unwrap())
    });
    let state = h.load_state().unwrap();
    let mut permits = 0;
    for (args, result) in requests.into_iter().zip(results) {
        let attempt = &state["attempts"][args["attempt_id"].as_str().unwrap()];
        if result["ok"] == true {
            permits += 1;
            assert_eq!(result["data"]["ready_for_single_submit"], true);
            assert_eq!(attempt["status"], "submitting");
            assert!(attempt["began_at"].is_string());
            assert_eq!(
                call(&h, "begin_submission", args)["ready_for_single_submit"],
                false
            );
        } else {
            assert_eq!(result["error"]["code"], "daily_submission_cap_reached");
            assert_eq!(attempt["status"], "reserved");
            assert!(attempt.get("began_at").is_none());
        }
    }
    assert_eq!(permits, 1);
    assert_eq!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["tool"] == "begin_submission")
            .count(),
        1
    );
}

#[test]
fn daily_submission_budget_is_optional_and_validated_by_workspace_and_engine() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    assert!(h.settings().unwrap().get("max_daily_submissions").is_none());
    for limit in [json!(0), json!(20)] {
        set_daily_budget(&h, limit.clone());
        assert_eq!(h.settings().unwrap()["max_daily_submissions"], limit);
        assert_eq!(
            workspace::settings(&h.root).unwrap()["max_daily_submissions"],
            limit
        );
    }
    for limit in [json!(-1), json!(1.5), json!("20"), Value::Null] {
        set_daily_budget(&h, limit);
        assert!(h.settings().is_err());
        assert!(workspace::settings(&h.root).is_err());
    }
}
#[test]
fn native_record_paths_become_valid_portable_journal_paths() {
    let directory = fixture();
    let engine = Engine::open(directory.path()).unwrap();
    let native = engine
        .root
        .join("applications")
        .join("acme-engineer-123.md");
    let mut changes = Vec::new();
    engine
        .change(
            &native,
            "# Synthetic vacancy\n\n<!-- tracking\n{\"stage\":\"saved\"}\n-->\n".into(),
            &mut changes,
        )
        .unwrap();
    assert_eq!(changes[0]["path"], "applications/acme-engineer-123.md");
    assert_eq!(
        crate::state::journal_path(&engine.root, changes[0]["path"].as_str().unwrap()).unwrap(),
        native
    );
    assert!(
        engine
            .change(
                &native,
                "# Another change\n\n<!-- tracking\n{}\n-->\n".into(),
                &mut changes
            )
            .is_err()
    );
    assert!(
        crate::state::journal_path(&engine.root, r"applications\acme-engineer-123.md").is_err()
    );
}
#[test]
fn all_contracts_reject_extra_input_without_writes() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    for name in contracts::specs().keys() {
        let value = h.call(name, &json!({"unexpected":true}));
        assert_eq!(value["error"]["code"], "invalid_arguments", "{name}");
    }
    assert!(!dir.path().join(crate::state::DB_REL_PATH).exists());
}
#[test]
fn batches_chain_values_and_preserve_partial_success() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let steps = json!([{"id":"start","tool":"start_run","arguments":{"operation":"reconcile","scope":{},"user_request":"fixture","idempotency_key":"r"}},{"id":"inspect","tool":"get_run","arguments":{"run_id":{"$ref":"start.data.run_id"}}},{"id":"bad","tool":"read_record","arguments":{"path":"../secret.md"}},{"id":"later","tool":"find_application","arguments":{}}]);
    let result = h.call("execute_batch", &json!({"steps":steps}));
    assert_eq!(result["ok"], false);
    assert_eq!(result["data"]["completed"], 2);
    assert_eq!(result["data"]["skipped"], json!(["later"]));
    assert_eq!(
        h.load_state().unwrap()["runs"].as_object().unwrap().len(),
        1
    );
}
#[test]
fn batches_reject_submit_boundaries_before_any_write() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let result=h.call("execute_batch",&json!({"steps":[{"id":"start","tool":"start_run","arguments":{"operation":"reconcile","scope":{},"user_request":"fixture","idempotency_key":"r"}},{"id":"begin","tool":"begin_submission","arguments":{}}]}));
    assert_eq!(result["error"]["code"], "batch_boundary");
    assert!(!dir.path().join(crate::state::DB_REL_PATH).exists());
}
#[test]
fn compound_preparation_replays_and_begin_is_one_shot() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let prepared = prepare(&h, &run, "prepare");
    assert_eq!(prepared, prepare(&h, &run, "prepare"));
    let args = json!({"attempt_id":prepared["attempt_id"],"idempotency_key":"begin"});
    assert_eq!(
        call(&h, "begin_submission", args.clone())["ready_for_single_submit"],
        true
    );
    assert_eq!(
        call(&h, "begin_submission", args)["ready_for_single_submit"],
        false
    );
    let again = h.call(
        "begin_submission",
        &json!({"attempt_id":prepared["attempt_id"],"idempotency_key":"other"}),
    );
    assert_eq!(again["error"]["code"], "submit_replay_blocked");
}
#[test]
fn unknown_outcome_blocks_retry_and_run_completion_forever() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let p = prepare(&h, &run, "prepare");
    call(
        &h,
        "begin_submission",
        json!({"attempt_id":p["attempt_id"],"idempotency_key":"begin"}),
    );
    call(
        &h,
        "finish_submission",
        json!({"attempt_id":p["attempt_id"],"outcome":"outcome_unknown","summary":"Lost response","evidence":[],"idempotency_key":"unknown"}),
    );
    assert_eq!(
        call(
            &h,
            "check_duplicate",
            json!({"company":"Acme","vacancy_url":"https://jobs.example.com/acme/123"})
        )["blocked"],
        true
    );
    let completed=h.call("finish_run",&json!({"run_id":run,"status":"completed","summary":"done","evidence":[],"idempotency_key":"finish"}));
    assert_eq!(completed["ok"], false);
}
#[test]
fn changed_answers_do_not_start_reserved_submission() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let p = prepare(&h, &run, "prepare");
    fs::write(
        dir.path().join("applications/approved-answers.md"),
        "New sourced answer",
    )
    .unwrap();
    let result = h.call(
        "begin_submission",
        &json!({"attempt_id":p["attempt_id"],"idempotency_key":"begin"}),
    );
    assert_eq!(result["error"]["code"], "answers_changed");
    assert_eq!(
        h.load_state().unwrap()["attempts"][p["attempt_id"].as_str().unwrap()]["status"],
        "reserved"
    );
    assert_eq!(h.call("prepare_application",&json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":"prepare"}))["error"]["code"],"artifact_changed");
}
#[test]
fn cancelled_attempts_still_count_toward_total_budget() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    for i in 0..3 {
        let p = prepare(&h, &run, &format!("prepare{i}"));
        call(
            &h,
            "cancel_reservation",
            json!({"attempt_id":p["attempt_id"],"summary":"Changed form","idempotency_key":format!("cancel{i}")}),
        );
    }
    let args = json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":"prepare4"});
    assert_eq!(
        h.call("prepare_application", &args)["error"]["code"],
        "cap_reached"
    );
}
#[test]
fn missing_linked_record_does_not_resolve_attempt() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let p = prepare(&h, &run, "prepare");
    call(
        &h,
        "begin_submission",
        json!({"attempt_id":p["attempt_id"],"idempotency_key":"begin"}),
    );
    fs::remove_file(dir.path().join("applications/acme-engineer-123.md")).unwrap();
    let result=h.call("finish_submission",&json!({"attempt_id":p["attempt_id"],"outcome":"submitted","summary":"Submitted","evidence":["https://jobs.example.com/receipt"],"idempotency_key":"finish"}));
    assert_eq!(result["error"]["code"], "missing_resource");
    assert_eq!(
        h.load_state().unwrap()["attempts"][p["attempt_id"].as_str().unwrap()]["status"],
        "submitting"
    );
}
#[test]
fn later_interview_stage_survives_old_receipt() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let p = prepare(&h, &run, "prepare");
    call(
        &h,
        "begin_submission",
        json!({"attempt_id":p["attempt_id"],"idempotency_key":"begin"}),
    );
    call(
        &h,
        "update_application",
        json!({"run_id":run,"record":"applications/acme-engineer-123.md","tracking":{"stage":"technical","checked":"2026-10-12","next_action":"Prepare interview"},"summary":"Interview invitation","evidence":["https://jobs.example.com/interview"],"idempotency_key":"invite"}),
    );
    call(
        &h,
        "finish_submission",
        json!({"attempt_id":p["attempt_id"],"outcome":"submitted","summary":"Found original receipt","evidence":["https://jobs.example.com/receipt"],"idempotency_key":"finish"}),
    );
    assert_eq!(
        pipeline::build(dir.path()).unwrap().records[0]["stage"],
        "technical"
    );
}
#[test]
fn journal_conflicts_preserve_manual_edits_and_diagnostic_reads() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let path = dir.path().join("applications/acme-engineer-123.md");
    let before = domain::digest(&fs::read(&path).unwrap());
    let mut state = h.load_state().unwrap();
    state["pending"] = json!([{"path":"applications/acme-engineer-123.md","before_sha256":before,"text":"# Acme\n\n<!-- tracking\n{\"stage\":\"applied\",\"checked\":\"2026-10-12\",\"next_action\":\"Monitor\"}\n-->\n"}]);
    crate::state::save(dir.path(), &state).unwrap();
    fs::write(&path, "# Acme — Engineer\n\nHuman update\n").unwrap();
    let result = h.call("reconcile_tracker", &json!({}));
    assert_eq!(result["error"]["code"], "pending_recovery");
    assert_eq!(result["next"][0]["tool"], "rebuild_pipeline");
    assert_eq!(
        h.call(
            "read_record",
            &json!({"path":"applications/acme-engineer-123.md"})
        )["ok"],
        true
    );
    assert_eq!(
        h.call("rebuild_pipeline", &json!({"idempotency_key":"recover"}))["error"]["code"],
        "recovery_conflict"
    );
    assert!(fs::read_to_string(path).unwrap().contains("Human update"));
}
#[test]
fn packet_tampering_is_typed() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let p = prepare(&h, &run, "prepare");
    let packet = p["packet_id"].as_str().unwrap();
    fs::write(
        dir.path()
            .join(format!("applications/operations/packets/{packet}.json")),
        "{}",
    )
    .unwrap();
    assert_eq!(
        h.call("get_packet", &json!({"packet_id":packet}))["error"]["code"],
        "artifact_changed"
    );
}
#[test]
fn booking_completion_preserves_previous_booking_history() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    for (stage, start, key) in [
        ("booked", Some("2026-10-13T11:00:00+01:00"), "book"),
        ("completed", None, "complete"),
    ] {
        let mut tracking = json!({"stage":"screening","checked":"2026-10-12","next_action":"Await feedback","schedule_state":stage});
        if let Some(start) = start {
            tracking["scheduled_start"] = json!(start);
        }
        call(
            &h,
            "update_application",
            json!({"run_id":run,"record":"applications/acme-engineer-123.md","tracking":tracking,"summary":"Fixture scheduling","evidence":["https://example.com/calendar"],"idempotency_key":key}),
        );
    }
    let text = fs::read_to_string(dir.path().join("applications/acme-engineer-123.md")).unwrap();
    assert!(text.contains("Previous booked time: 2026-10-13T11:00:00+01:00"));
    assert!(
        pipeline::build(dir.path()).unwrap().records[0]
            .get("scheduled_start")
            .is_none()
    );
}
#[test]
fn idempotency_is_bound_to_the_exact_input() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let args = json!({"operation":"reconcile","scope":{},"user_request":"fixture","idempotency_key":"same"});
    let first = call(&h, "start_run", args.clone());
    assert_eq!(first, call(&h, "start_run", args.clone()));
    let mut changed = args;
    changed["user_request"] = json!("changed");
    assert_eq!(
        h.call("start_run", &changed)["error"]["code"],
        "idempotency_conflict"
    );
}
#[test]
fn exact_references_are_data_not_code() {
    let mut values = BTreeMap::new();
    values.insert("first".into(), json!({"data":{"rows":["safe"]}}));
    assert_eq!(
        resolve_refs(&json!({"$ref":"first.data.rows.0"}), &values).unwrap(),
        "safe"
    );
    assert!(resolve_refs(&json!({"$ref":"first","extra":true}), &values).is_err());
    assert!(resolve_refs(&json!({"$ref":"first.data.missing"}), &values).is_err());
}

#[test]
fn concurrent_reservations_have_one_canonical_winner() {
    use std::sync::{Arc, Barrier};
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let r1 = start(&h, 1);
    let r2=call(&h,"start_run",json!({"operation":"apply","scope":{"sources":["employer"],"max_submissions":1,"dry_run":false},"user_request":"Second synthetic worker","idempotency_key":"run2"}))["run_id"].as_str().unwrap().to_owned();
    let packet_args = |run: &str, key: &str| json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":key});
    let p1 = call(&h, "prepare_packet", packet_args(&r1, "packet1"));
    let p2 = call(&h, "prepare_packet", packet_args(&r2, "packet2"));
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = vec![];
    for (run, packet) in [(r1, p1), (r2, p2)] {
        let root = dir.path().to_path_buf();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move||{let h=Engine::open(root).unwrap();barrier.wait();h.call("reserve_submission",&json!({"run_id":run,"packet_id":packet["packet_id"],"idempotency_key":format!("reserve:{run}")}))}));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r["ok"] == true).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r["error"]["code"] == "duplicate_blocked")
            .count(),
        1
    );
    assert_eq!(
        h.load_state().unwrap()["attempts"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn interrupted_journal_finishes_exact_local_change() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let path = dir.path().join("applications/acme-engineer-123.md");
    let before = domain::digest(&fs::read(&path).unwrap());
    let text = "# Acme — Engineer\n\n<!-- tracking\n{\"stage\":\"applied\",\"checked\":\"2026-10-12\",\"next_action\":\"Monitor response\"}\n-->\n";
    let mut state = h.load_state().unwrap();
    state["pending"] =
        json!([{"path":"applications/acme-engineer-123.md","before_sha256":before,"text":text}]);
    crate::state::save(dir.path(), &state).unwrap();
    call(
        &h,
        "rebuild_pipeline",
        json!({"idempotency_key":"recovery"}),
    );
    assert_eq!(fs::read_to_string(path).unwrap(), text);
    assert!(h.load_state().unwrap().get("pending").is_none());
    assert_eq!(
        call(&h, "reconcile_tracker", json!({}))["pipeline_stale"],
        false
    );
}

#[test]
fn provider_observations_and_repeated_failure_recovery_are_bounded() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let args = json!({"run_id":run,"record_path":"applications/acme-engineer-123.md","provider":"fixture-board","target_url":"https://board.example.com/123","capabilities":["board.sync"]});
    let plan = call(&h, "plan_adapter", args.clone());
    let action = &plan["actions"][0];
    let mismatch=h.call("record_adapter_result",&json!({"run_id":run,"record_path":"applications/acme-engineer-123.md","provider":"fixture-board","target_url":"https://board.example.com/123","plan_id":plan["plan_id"],"outcome":"verified","observed_stage":"applied","observed_note":action["note"],"evidence":["https://board.example.com/123"],"idempotency_key":"bad"}));
    assert_eq!(mismatch["error"]["code"], "adapter_mismatch");
    call(
        &h,
        "record_adapter_result",
        json!({"run_id":run,"record_path":"applications/acme-engineer-123.md","provider":"fixture-board","target_url":"https://board.example.com/123","plan_id":plan["plan_id"],"outcome":"verified","observed_stage":action["desired_stage"],"observed_note":action["note"],"evidence":["https://board.example.com/123"],"idempotency_key":"sync"}),
    );
    for i in 0..2 {
        call(
            &h,
            "record_adapter_result",
            json!({"run_id":run,"record_path":"applications/acme-engineer-123.md","provider":"fixture-board","target_url":"https://board.example.com/123","plan_id":plan["plan_id"],"outcome":"failed","failure_code":"upload_control_missing","evidence":["https://board.example.com/123"],"idempotency_key":format!("failure{i}")}),
        );
    }
    let mut recovery = args;
    recovery["failure_code"] = json!("upload_control_missing");
    assert_eq!(call(&h, "plan_adapter", recovery)["stop_retrying"], true);
}

#[test]
fn all_capabilities_can_return_their_published_output_shape() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    call(
        &h,
        "bootstrap",
        json!({"operation":"apply","capabilities":["web.read"]}),
    );
    call(&h, "list_operations", json!({}));
    call(&h, "plan_workflow", json!({"operation":"reconcile"}));
    call(&h, "get_approved_answer", json!({"query":"salary"}));
    call(
        &h,
        "inspect_tools",
        json!({"names":["prepare_application"],"include_schemas":true}),
    );
    let run = start(&h, 1);
    call(
        &h,
        "record_event",
        json!({"run_id":run,"kind":"mail_reviewed","summary":"Read source mail","evidence":["https://mail.example.com/thread"],"idempotency_key":"mail"}),
    );
    call(
        &h,
        "create_application",
        json!({"run_id":run,"record":"applications/other-engineer-999.md","company":"Other","role":"Engineer","vacancy_url":"https://jobs.example.com/other/999","source":"employer","fit_reason":"Synthetic fixture fit","evidence":["https://jobs.example.com/other/999"],"idempotency_key":"create"}),
    );
    call(
        &h,
        "save_receipt",
        json!({"run_id":run,"path":"applications/answers.json","idempotency_key":"receipt"}),
    );
    call(&h, "get_run", json!({"run_id":run}));
    call(&h, "list_events", json!({"run_id":run,"limit":50}));
    call(&h, "find_application", json!({"limit":1}));
    call(
        &h,
        "finish_run",
        json!({"run_id":run,"status":"completed","summary":"Synthetic checks finished","evidence":[],"idempotency_key":"done"}),
    );
}

#[test]
fn conflict_resolution_preserves_losing_bytes_and_allows_external_resolution() {
    for keep in ["current", "journal"] {
        let dir = fixture();
        let h = Engine::open(dir.path()).unwrap();
        let run = start(&h, 1);
        let p = prepare(&h, &run, "prepare");
        call(
            &h,
            "begin_submission",
            json!({"attempt_id":p["attempt_id"],"idempotency_key":"begin"}),
        );
        let path = dir.path().join("applications/acme-engineer-123.md");
        let before = domain::digest(&fs::read(&path).unwrap());
        let wanted = "# Acme — Engineer\n\n<!-- tracking\n{\"stage\":\"applied\",\"checked\":\"2026-10-12\",\"next_action\":\"Monitor\",\"company\":\"Acme\",\"vacancy_url\":\"https://jobs.example.com/acme/123\"}\n-->\n";
        let current = "# Acme — Engineer\n\n- Stage: technical\n- Source URL: https://jobs.example.com/acme/123\n\nManual interview note\n";
        let mut state = h.load_state().unwrap();
        state["pending"] = json!([{"path":"applications/acme-engineer-123.md","before_sha256":before,"text":wanted}]);
        crate::state::save(dir.path(), &state).unwrap();
        fs::write(&path, current).unwrap();
        let pending = h.pending_transaction().unwrap();
        let sha = pending["pending_sha256"].as_str().unwrap();
        let evidence = vec!["https://jobs.example.com/reconciliation".to_owned()];
        let result = h.resolve_pending(keep, &evidence, "resolve", sha).unwrap();
        let losing = if keep == "current" { wanted } else { current };
        let copy = result["conflict_copies"][0]["path"].as_str().unwrap();
        assert_eq!(fs::read_to_string(dir.path().join(copy)).unwrap(), losing);
        assert_eq!(
            result,
            h.resolve_pending(keep, &evidence, "resolve", sha).unwrap()
        );
        assert!(h.load_state().unwrap().get("pending").is_none());
        call(
            &h,
            "finish_submission",
            json!({"attempt_id":p["attempt_id"],"outcome":"submitted","summary":"Verified external receipt","evidence":["https://jobs.example.com/receipt"],"idempotency_key":"finish"}),
        );
        assert_eq!(
            h.load_state().unwrap()["attempts"][p["attempt_id"].as_str().unwrap()]["status"],
            "submitted"
        );
    }
}

#[test]
fn duplicate_change_paths_are_rejected_before_journal_commit() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let path = h
        .record_path("applications/acme-engineer-123.md", true)
        .unwrap();
    let text = "# Acme\n\n<!-- tracking\n{\"stage\":\"saved\",\"checked\":\"2026-10-12\",\"next_action\":\"review\"}\n-->\n";
    let mut changes = vec![];
    h.change(&path, text.into(), &mut changes).unwrap();
    assert_eq!(
        h.change(&path, text.into(), &mut changes).unwrap_err().code,
        "invalid_arguments"
    );
    assert_eq!(changes.len(), 1);
    assert!(!dir.path().join(crate::state::DB_REL_PATH).exists());
}

#[test]
fn malformed_state_returns_typed_errors_and_preserves_unknown_fields() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let mut state = h.load_state().unwrap();
    state["future_extension"] = json!({"origin":"external-tool","number":1.0});
    state["runs"][&run]["future"] = json!(["preserve",{"x":true}]);
    crate::state::save(dir.path(), &state).unwrap();
    call(
        &h,
        "record_event",
        json!({"run_id":run,"kind":"reconciled","summary":"Synthetic event","evidence":["https://example.com/source"],"idempotency_key":"event"}),
    );
    let changed = h.load_state().unwrap();
    assert_eq!(changed["future_extension"], state["future_extension"]);
    assert_eq!(
        changed["runs"][&run]["future"],
        state["runs"][&run]["future"]
    );
    let conn = rusqlite::Connection::open(dir.path().join(crate::state::DB_REL_PATH)).unwrap();
    conn.execute(
        "UPDATE runs SET payload = '[]' WHERE id = ?1",
        rusqlite::params![&run],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        h.call("get_run", &json!({"run_id":run}))["error"]["code"],
        "invalid_state"
    );
    let conn = rusqlite::Connection::open(dir.path().join(crate::state::DB_REL_PATH)).unwrap();
    let raw_payload: String = conn
        .query_row(
            "SELECT payload FROM runs WHERE id = ?1",
            rusqlite::params![&run],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(raw_payload, "[]");
}

#[test]
fn finish_and_cancel_can_resolve_after_the_run_is_partial() {
    for started in [false, true] {
        let dir = fixture();
        let h = Engine::open(dir.path()).unwrap();
        let run = start(&h, 1);
        let p = prepare(&h, &run, "prepare");
        if started {
            call(
                &h,
                "begin_submission",
                json!({"attempt_id":p["attempt_id"],"idempotency_key":"begin"}),
            );
        }
        call(
            &h,
            "finish_run",
            json!({"run_id":run,"status":"partial","summary":"Session ended","evidence":[],"idempotency_key":"end"}),
        );
        if started {
            call(
                &h,
                "finish_submission",
                json!({"attempt_id":p["attempt_id"],"outcome":"not_submitted","summary":"Verified no submission","evidence":["https://jobs.example.com/form"],"idempotency_key":"resolve"}),
            );
        } else {
            call(
                &h,
                "cancel_reservation",
                json!({"attempt_id":p["attempt_id"],"summary":"Unstarted","idempotency_key":"cancel"}),
            );
        }
        assert_eq!(
            h.load_state().unwrap()["attempts"][p["attempt_id"].as_str().unwrap()]["status"],
            "cancelled"
        );
    }
}

#[test]
fn derived_search_preserves_source_kind_and_does_not_drive_submission_guards() {
    let dir = fixture();
    fs::create_dir_all(dir.path().join("applications/briefs")).unwrap();
    fs::write(
        dir.path().join("context/evidence.md"),
        "# Evidence\nSourced database transaction story.\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("applications/briefs/acme.md"),
        "# Practice\nHypothetical database transaction answer.\n",
    )
    .unwrap();
    let h = Engine::open(dir.path()).unwrap();
    let result = call(
        &h,
        "search_workspace",
        json!({"query":"database transaction","limit":5}),
    );
    assert_eq!(result["authority"], "derived");
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    assert!(
        result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "coaching")
    );
    let before = call(
        &h,
        "check_duplicate",
        json!({"company":"Acme","vacancy_url":"https://jobs.example.com/acme/123"}),
    );
    assert_eq!(before["blocked"], false);
    let tools = call(
        &h,
        "inspect_tools",
        json!({"names":["search_workspace"],"include_schemas":true}),
    );
    assert_eq!(tools["tools"][0]["read_only"], true);
    let batch = call(
        &h,
        "query_batch",
        json!({"steps":[{"id":"search","tool":"search_workspace","arguments":{"query":"database"}}]}),
    );
    assert_eq!(batch["completed"], 1);
}

#[test]
fn duplicate_batches_match_single_checks_and_refresh_after_a_reservation() {
    let directory = fixture();
    let engine = Engine::open(directory.path()).unwrap();
    let run = start(&engine, 1);
    let steps = json!([
        {"id":"acme","tool":"check_duplicate","arguments":{"company":"Acme","vacancy_url":"https://jobs.example.com/acme/123?utm_source=new","posting_id":"123"}},
        {"id":"other","tool":"check_duplicate","arguments":{"company":"Other","vacancy_url":"https://jobs.example.com/other/456"}}
    ]);
    let args = json!({"steps": steps});
    let state_file = directory.path().join(crate::state::DB_REL_PATH);
    for expected_block in [false, true] {
        if expected_block {
            prepare(&engine, &run, "reserve");
        }
        let before = fs::read(&state_file).unwrap();
        let batch = call(&engine, "query_batch", args.clone());
        assert_eq!(batch["completed"], 2);
        for (step, result) in steps
            .as_array()
            .unwrap()
            .iter()
            .zip(batch["results"].as_array().unwrap())
        {
            let single = engine.call("check_duplicate", &step["arguments"]);
            assert_eq!(result["result"]["data"], single["data"]);
        }
        assert_eq!(
            batch["results"][0]["result"]["data"]["blocked"],
            expected_block
        );
        assert_eq!(batch["results"][1]["result"]["data"]["blocked"], false);
        assert_eq!(fs::read(&state_file).unwrap(), before);
    }
}

#[cfg(unix)]
#[test]
fn mutation_refuses_internal_record_symlinks_before_committing_a_journal() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let path = dir.path().join("applications/acme-engineer-123.md");
    let target = dir.path().join("context/linked-record.md");
    fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let before = h.load_state().unwrap();
    assert_eq!(
        h.call(
            "read_record",
            &json!({"path":"applications/acme-engineer-123.md"})
        )["ok"],
        true
    );
    let result=h.call("update_application",&json!({"run_id":run,"record":"applications/acme-engineer-123.md","tracking":{"stage":"prepared","checked":"2026-10-12","next_action":"review"},"summary":"fixture","evidence":["https://example.com/source"],"idempotency_key":"update"}));
    assert_eq!(result["error"]["code"], "path_denied");
    assert_eq!(h.load_state().unwrap(), before);
    assert!(path.is_symlink());
}

#[cfg(unix)]
#[test]
fn coordinator_symlinks_are_refused_without_replacing_them() {
    let dir = fixture();
    fs::create_dir_all(dir.path().join("applications/operations")).unwrap();
    let target = dir.path().join("context/harness.sqlite");
    fs::write(&target, b"not-a-db").unwrap();
    let path = dir.path().join(crate::state::DB_REL_PATH);
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(
        Engine::open(dir.path()).err().unwrap().code,
        "unsupported_layout"
    );
    assert!(path.is_symlink());
}

#[test]
fn attention_queue_only_claims_recorded_or_supplied_state() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let p = prepare(&h, &run, "prepare");
    call(
        &h,
        "begin_submission",
        json!({"attempt_id":p["attempt_id"],"idempotency_key":"begin"}),
    );
    let mail = dir.path().join("context/mail-observation.json");
    fs::write(&mail,r#"{"observed_at":"2026-10-12T12:00:00Z","items":[{"id":"fixture-mail","subject":"Pick a time","thread_url":"https://mail.example.com/thread","needs_reply":true},{"id":"receipt","needs_reply":false}]}"#).unwrap();
    let queue = call(
        &h,
        "review_queue",
        json!({"mail_observations_path":"context/mail-observation.json","limit":50}),
    );
    assert_eq!(queue["live_mail_checked"], false);
    assert_eq!(queue["authority"], "recorded_local_state");
    assert_eq!(queue["counts"]["unknown_submissions"], 1);
    assert_eq!(queue["counts"]["supplied_mail"], 1);
    assert_eq!(
        queue["mail_source"]["path"],
        "context/mail-observation.json"
    );
    assert!(
        queue["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "supplied_mail")
    );
}

#[test]
fn packet_preview_reports_hash_diffs_without_changing_an_attempt() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let p = prepare(&h, &run, "prepare");
    let before = h.load_state().unwrap();
    let original = call(&h, "preview_packet", json!({"packet_id":p["packet_id"]}));
    assert_eq!(original["stale"], false);
    fs::write(
        dir.path().join("applications/answers.json"),
        r#"{"changed":true}"#,
    )
    .unwrap();
    let changed = call(&h, "preview_packet", json!({"packet_id":p["packet_id"]}));
    assert_eq!(changed["stale"], true);
    assert!(
        changed["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["kind"] == "original_artifact" && check["changed"] == true)
    );
    assert_eq!(changed["attempts"][0]["status"], "reserved");
    assert_eq!(h.load_state().unwrap(), before);
}

#[test]
fn keeping_current_supersedes_stale_creation_success() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let args = json!({"run_id":run,"record":"applications/new-engineer-999.md","company":"New","role":"Engineer","vacancy_url":"https://jobs.example.com/new/999","source":"employer","fit_reason":"Synthetic fit","evidence":["https://jobs.example.com/new/999"],"idempotency_key":"create"});
    call(&h, "create_application", args.clone());
    let path = dir.path().join("applications/new-engineer-999.md");
    let wanted = fs::read_to_string(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let mut state = h.load_state().unwrap();
    state["pending"] =
        json!([{"path":"applications/new-engineer-999.md","before_sha256":null,"text":wanted}]);
    storage::commit_state(h.root(), &state).unwrap();
    let pending = h.pending_transaction().unwrap();
    let resolved = h
        .resolve_pending(
            "current",
            &["https://example.com/reconciliation".into()],
            "resolve",
            pending["pending_sha256"].as_str().unwrap(),
        )
        .unwrap();
    assert_eq!(
        resolved["unupdated_paths"],
        json!(["applications/new-engineer-999.md"])
    );
    assert_eq!(resolved["superseded_actions"], json!(["create"]));
    assert_eq!(
        h.call("create_application", &args)["error"]["code"],
        "recovery_conflict"
    );
    assert!(!path.exists());
}

#[test]
fn derived_view_failure_cannot_pin_a_completed_journal() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let record = dir.path().join("applications/acme-engineer-123.md");
    let mut state = h.load_state().unwrap();
    let text = "# Acme — Engineer\n\n<!-- tracking\n{\"stage\":\"saved\",\"checked\":\"2026-10-12\",\"next_action\":\"Review\"}\n-->\n";
    state["pending"] = json!([{"path":"applications/acme-engineer-123.md","before_sha256":domain::digest(&fs::read(&record).unwrap()),"text":text}]);
    storage::commit_state(h.root(), &state).unwrap();
    fs::create_dir_all(dir.path().join("applications/pipeline.md")).unwrap();
    let result = h.call("rebuild_pipeline", &json!({"idempotency_key":"rebuild"}));
    assert_eq!(result["ok"], false);
    assert!(h.load_state().unwrap().get("pending").is_none());
    assert_eq!(fs::read_to_string(record).unwrap(), text);
    assert_eq!(
        call(&h, "reconcile_tracker", json!({}))["pipeline_stale"],
        true
    );
}

#[test]
fn intake_proposals_are_separate_and_acceptance_is_explicit() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let cv_before = fs::read(dir.path().join("output/cv.pdf")).unwrap();
    let answers_before = fs::read(dir.path().join("applications/approved-answers.md")).unwrap();
    let args = json!({"text":"# User notes\nWe discussed database transactions.\n","source_kind":"personal_notes","source_label":"Synthetic meeting","idempotency_key":"note"});
    let note = call(&h, "intake_note", args.clone());
    assert_eq!(note, call(&h, "intake_note", args));
    let proposal = call(
        &h,
        "propose_note",
        json!({"note_id":note["note_id"],"expected_source_sha256":note["source_sha256"],"items":[{"id":"summary1","kind":"summary","text":"Database transactions were discussed.","citations":[{"path":note["original_path"],"sha256":note["source_sha256"],"start_line":2,"end_line":2}]}],"questions":["Confirm the missing meeting date."],"idempotency_key":"proposal"}),
    );
    let review = call(
        &h,
        "review_note",
        json!({"note_id":note["note_id"],"proposal_id":proposal["proposal_id"],"include_original":true}),
    );
    assert_eq!(review["source_fresh"], true);
    assert_eq!(review["accepted_reviews"].as_array().unwrap().len(), 0);
    assert_eq!(review["metadata"]["occurred_at"], Value::Null);
    assert_eq!(review["proposal"]["status"], "proposed");
    let accepted = call(
        &h,
        "accept_note",
        json!({"note_id":note["note_id"],"proposal_id":proposal["proposal_id"],"expected_proposal_sha256":proposal["proposal_sha256"],"selected_item_ids":["summary1"],"approved_by":"Synthetic user","approval_evidence":"Explicit fixture instruction","idempotency_key":"accept"}),
    );
    assert_eq!(accepted["facts_changed"], false);
    assert_eq!(accepted["cv_changed"], false);
    assert_eq!(accepted["next_steps_executed"], false);
    assert!(
        dir.path()
            .join(accepted["accepted_path"].as_str().unwrap())
            .is_file()
    );
    assert_eq!(
        fs::read(dir.path().join("output/cv.pdf")).unwrap(),
        cv_before
    );
    assert_eq!(
        fs::read(dir.path().join("applications/approved-answers.md")).unwrap(),
        answers_before
    );
    let notes = call(&h, "list_notes", json!({}));
    assert_eq!(notes["page"]["total"], 1);
    let archived = call(
        &h,
        "archive_note",
        json!({"note_id":note["note_id"],"expected_metadata_sha256":review["metadata_sha256"],"reason":"Fixture reviewed","evidence":"Explicit fixture archive instruction","idempotency_key":"archive"}),
    );
    assert_eq!(archived["retained"], true);
    assert_eq!(call(&h, "list_notes", json!({}))["page"]["total"], 0);
    assert_eq!(
        call(&h, "list_notes", json!({"include_archived":true}))["page"]["total"],
        1
    );
    assert!(
        dir.path()
            .join(note["original_path"].as_str().unwrap())
            .is_file()
    );
}

#[test]
fn intake_batch_chains_exact_sources_without_automatic_acceptance() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let result = call(
        &h,
        "execute_batch",
        json!({"steps":[{"id":"note","tool":"intake_note","arguments":{"text":"Synthetic original note\n","source_kind":"transcript","idempotency_key":"note"}},{"id":"proposal","tool":"propose_note","arguments":{"note_id":{"$ref":"note.data.note_id"},"expected_source_sha256":{"$ref":"note.data.source_sha256"},"items":[{"id":"next1","kind":"next_step","text":"Prepare a review","citations":[{"path":{"$ref":"note.data.original_path"},"sha256":{"$ref":"note.data.source_sha256"},"start_line":1,"end_line":1}]}],"idempotency_key":"proposal"}},{"id":"review","tool":"review_note","arguments":{"note_id":{"$ref":"note.data.note_id"},"proposal_id":{"$ref":"proposal.data.proposal_id"}}}]}),
    );
    assert_eq!(result["completed"], 3);
    assert_eq!(
        result["results"][2]["result"]["data"]["accepted_reviews"],
        json!([])
    );
    assert!(h.load_state().unwrap().get("pending").is_none());
    let readonly=h.call("query_batch",&json!({"steps":[{"id":"note","tool":"intake_note","arguments":{"text":"No write","source_kind":"personal_notes","idempotency_key":"other"}}]}));
    assert_eq!(readonly["error"]["code"], "invalid_arguments");
}

#[test]
fn intake_stale_source_is_reported_and_cannot_be_accepted() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let note = call(
        &h,
        "intake_note",
        json!({"text":"Original\n","source_kind":"generated_summary","idempotency_key":"note"}),
    );
    let proposal = call(
        &h,
        "propose_note",
        json!({"note_id":note["note_id"],"expected_source_sha256":note["source_sha256"],"items":[{"id":"summary1","kind":"summary","text":"Original","citations":[{"path":note["original_path"],"sha256":note["source_sha256"],"start_line":1,"end_line":1}]}],"idempotency_key":"proposal"}),
    );
    fs::write(
        dir.path().join(note["original_path"].as_str().unwrap()),
        "Manual source change\n",
    )
    .unwrap();
    let review = call(
        &h,
        "review_note",
        json!({"note_id":note["note_id"],"proposal_id":proposal["proposal_id"]}),
    );
    assert_eq!(review["source_fresh"], false);
    assert!(!review["issues"].as_array().unwrap().is_empty());
    let result=h.call("accept_note",&json!({"note_id":note["note_id"],"proposal_id":proposal["proposal_id"],"expected_proposal_sha256":proposal["proposal_sha256"],"selected_item_ids":["summary1"],"approved_by":"Fixture","approval_evidence":"Fixture instruction","idempotency_key":"accept"}));
    assert_eq!(result["error"]["code"], "source_changed");
}

#[test]
fn context_pack_preserves_unknown_time_and_exact_passage_anchors() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let note = call(
        &h,
        "intake_note",
        json!({"text":"# Interview note\nDatabase transactions came up.\nA follow-up is proposed.\n","source_kind":"transcript","occurred_period":"around Q2 2024","source_group":"Example project","idempotency_key":"note"}),
    );
    let pack = call(
        &h,
        "context_pack",
        json!({"task":"Prepare a screening brief","query":"database","paths":[note["original_path"]],"period":{"start":"2024-04-01","end":"2024-06-30","basis":"occurred"},"limit":2}),
    );
    assert_eq!(pack["authority"], "sourced_context_only");
    assert_eq!(pack["complete"], false);
    let source = &pack["sources"][0];
    assert_eq!(source["sha256"], note["source_sha256"]);
    assert_eq!(source["start_line"], 1);
    assert_eq!(source["provenance"]["occurred_period"], "around Q2 2024");
    assert_eq!(source["provenance"]["occurred_at"], Value::Null);
    assert_eq!(
        source["provenance"]["date_confidence"],
        "supplied_approximate"
    );
    assert!(
        pack["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["code"] == "period_membership_unknown")
    );
    assert!(
        source["excerpt"]
            .as_str()
            .unwrap()
            .contains("Database transactions")
    );
    assert_eq!(pack["read_refs"][0]["tool"], "read_record");
}

#[test]
fn context_pack_period_filter_never_uses_recorded_time_as_occurrence() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let note = call(
        &h,
        "intake_note",
        json!({"text":"Known date original\n","source_kind":"personal_notes","occurred_at":"2024-05-01","idempotency_key":"note"}),
    );
    let pack = call(
        &h,
        "context_pack",
        json!({"task":"Review recent evidence","paths":[note["original_path"]],"period":{"start":"2025-01-01","basis":"occurred"}}),
    );
    assert_eq!(pack["selected_count"], 0);
    assert!(
        pack["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["code"] == "no_sources")
    );
    let unfiltered = call(
        &h,
        "context_pack",
        json!({"task":"Review exact evidence","paths":[note["original_path"]]}),
    );
    assert_eq!(
        unfiltered["sources"][0]["provenance"]["date_confidence"],
        "supplied_exact"
    );
}

#[test]
fn context_pack_remains_bounded_and_reports_current_source_drift() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let note = call(
        &h,
        "intake_note",
        json!({"text":"Original\n","source_kind":"personal_notes","idempotency_key":"note"}),
    );
    let target = dir.path().join(note["original_path"].as_str().unwrap());
    fs::write(&target, "Changed source\n".repeat(1000)).unwrap();
    let before = h.load_state().unwrap();
    let pack = call(
        &h,
        "context_pack",
        json!({"task":"Read changed notes","paths":[note["original_path"]],"limit":1}),
    );
    assert_eq!(pack["sources"][0]["stale"], true);
    assert_eq!(pack["sources"][0]["partial"], true);
    assert!(
        pack["sources"][0]["excerpt"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 1400
    );
    assert_eq!(h.load_state().unwrap(), before);
    let batch = call(
        &h,
        "query_batch",
        json!({"steps":[{"id":"context","tool":"context_pack","arguments":{"task":"Prepare interview","company":"Acme"}}]}),
    );
    assert_eq!(batch["completed"], 1);
}

#[test]
fn workflow_check_batches_readback_and_coverage_without_actions() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let checked = call(
        &h,
        "workflow_check",
        json!({"checks":[{"id":"coverage","kind":"coverage","observation":{"source":"fixture-mail","observed_at":"2026-10-12","requested_period":"October","pages_read":1,"pagination_exhausted":false,"failed_reads":["message-2"],"exclusions":[],"checkpoint":"next-page"}},{"id":"form","kind":"form","observation":{"vacancy_url":"https://jobs.example.com/acme/123","page_revision":"old","readback_revision":"new","observed_at":"2026-10-12","fields":[],"uploads":[],"parsed_claim_changes":[]}}]}),
    );
    assert_eq!(checked["canonical_changes"], false);
    assert_eq!(checked["external_actions"], false);
    assert_eq!(checked["checks"][0]["data"]["complete"], false);
    assert_eq!(checked["checks"][1]["data"]["ready_for_preparation"], false);
    assert_eq!(checked["checks"][1]["data"]["submit_permitted"], false);
    let discovery = call(
        &h,
        "inspect_tools",
        json!({"names":["workflow_check"],"include_schemas":true}),
    );
    assert_eq!(discovery["tools"][0]["read_only"], true);
    assert!(!dir.path().join(crate::state::DB_REL_PATH).exists());
}

#[test]
fn explicit_project_seeds_context_and_linked_intake_even_when_query_has_no_hits() {
    let dir = fixture();
    fs::create_dir_all(dir.path().join("context/projects")).unwrap();
    let project = "context/projects/migration.md";
    fs::write(
        dir.path().join(project),
        "# Migration\nTransactions protect imports.\n",
    )
    .unwrap();
    let h = Engine::open(dir.path()).unwrap();
    let note = call(
        &h,
        "intake_note",
        json!({"text":"A collaborator discussed import retries.\n","source_kind":"personal_notes","links":[{"kind":"project","path":project}],"idempotency_key":"linked-note"}),
    );
    let before = h.load_state().unwrap();
    let args = json!({"task":"Prepare project story","project":project,"query":"harbourdragon-no-match","limit":6});
    let pack = call(&h, "context_pack", args.clone());
    assert!(
        pack["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["path"] == project)
    );
    assert!(
        pack["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["path"] == note["original_path"])
    );
    assert_eq!(
        pack["pack_sha256"],
        call(&h, "context_pack", args.clone())["pack_sha256"]
    );
    assert_eq!(h.load_state().unwrap(), before);
    fs::write(
        dir.path().join(project),
        "# Migration\nChanged exact evidence.\n",
    )
    .unwrap();
    assert_ne!(
        pack["pack_sha256"],
        call(&h, "context_pack", args)["pack_sha256"]
    );
}
#[test]
fn review_queue_retains_uncertain_mail_and_only_unreviewed_note_proposals() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let note = call(
        &h,
        "intake_note",
        json!({"text":"Original fixture\n","source_kind":"personal_notes","idempotency_key":"queue-note"}),
    );
    let proposal = call(
        &h,
        "propose_note",
        json!({"note_id":note["note_id"],"expected_source_sha256":note["source_sha256"],"items":[{"id":"summary","kind":"summary","text":"Fixture summary","citations":[{"path":note["original_path"],"sha256":note["source_sha256"],"start_line":1,"end_line":1}]}],"idempotency_key":"queue-proposal"}),
    );
    fs::write(dir.path().join("context/mail-observation.json"), r#"{"items":[{"id":"uncertain","needs_reply":null},{"id":"ambiguous","needs_reply":false,"attribution":"ambiguous"},{"id":"resolved","needs_reply":false,"attribution":"proposed_exact_identity","needs_review":false}]}"#).unwrap();
    let args = json!({"mail_observations_path":"context/mail-observation.json","limit":50});
    let before = h.load_state().unwrap();
    let queue = call(&h, "review_queue", args.clone());
    assert_eq!(queue["counts"]["supplied_mail"], 2);
    assert_eq!(queue["counts"]["pending_note_reviews"], 1);
    assert_eq!(queue["note_reviews_complete"], true);
    assert!(
        queue["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["proposal_id"] == proposal["proposal_id"])
    );
    assert_eq!(h.load_state().unwrap(), before);
    call(
        &h,
        "accept_note",
        json!({"note_id":note["note_id"],"proposal_id":proposal["proposal_id"],"expected_proposal_sha256":proposal["proposal_sha256"],"selected_item_ids":["summary"],"approved_by":"Fixture user","approval_evidence":"Explicit synthetic approval","idempotency_key":"queue-accept"}),
    );
    let reviewed = call(&h, "review_queue", args);
    assert_eq!(reviewed["counts"]["pending_note_reviews"], 0);
}
#[test]
fn review_queue_prioritizes_live_interviews_and_mail_over_historical_unknowns() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 5);
    let prep = prepare(&h, &run, "rq-prio-prep");
    call(
        &h,
        "begin_submission",
        json!({"attempt_id":prep["attempt_id"],"idempotency_key":"rq-prio-begin"}),
    );
    call(
        &h,
        "finish_submission",
        json!({"attempt_id":prep["attempt_id"],"outcome":"outcome_unknown","summary":"Unknown synthetic outcome","evidence":["applications/acme-engineer-123.md"],"idempotency_key":"rq-prio-unk"}),
    );
    let upcoming_time = (chrono::Utc::now() + chrono::Duration::hours(4))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    fs::write(
        dir.path().join("applications/liveco-staff.md"),
        format!(
            "# LiveCo — Staff Engineer\n\n<!-- tracking\n{{\"company\":\"LiveCo\",\"title\":\"Staff Engineer\",\"stage\":\"technical\",\"checked\":\"2026-11-23\",\"schedule_state\":\"booked\",\"scheduled_start\":\"{upcoming_time}\",\"next_action\":\"Prepare system design\"}}\n-->\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.path().join("context/mail-prio.json"),
        r#"{"items":[{"id":"m1","needs_reply":true,"company":"MailCo","subject":"Interview times"}]}"#,
    )
    .unwrap();
    let boot = call(
        &h,
        "bootstrap",
        json!({"operation":"daily_check","capabilities":[]}),
    );
    assert_eq!(boot["daily_budget"]["used"], 1);
    let queue = call(
        &h,
        "review_queue",
        json!({"mail_observations_path":"context/mail-prio.json","limit":10}),
    );
    assert_eq!(queue["needs_attention_now"], true);
    assert_eq!(queue["next_upcoming_interview"]["company"], "LiveCo");
    let items = queue["items"].as_array().unwrap();
    let idx_interview = items.iter().position(|i| i["kind"] == "interview").unwrap();
    let idx_mail = items
        .iter()
        .position(|i| i["kind"] == "supplied_mail")
        .unwrap();
    let idx_unknown = items
        .iter()
        .position(|i| i["kind"] == "unknown_submission")
        .unwrap();
    assert_eq!(items[idx_interview]["urgency"], "urgent_now");
    assert_eq!(items[idx_mail]["urgency"], "urgent_now");
    assert_eq!(items[idx_unknown]["urgency"], "background");
    assert!(idx_interview < idx_mail && idx_mail < idx_unknown);
}
#[test]
fn workflow_child_schemas_are_discoverable_and_validate_before_execution() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let schema = &contracts::specs()["workflow_check"]["inputSchema"];
    let variants = schema["properties"]["checks"]["items"]["anyOf"]
        .as_array()
        .unwrap();
    assert_eq!(variants.len(), 4);
    let mail = variants
        .iter()
        .find(|row| row["properties"]["kind"]["enum"][0] == "mail")
        .unwrap();
    assert!(
        mail["properties"]["observation"]["properties"]
            .get("request_message_ids")
            .is_some()
    );
    let bad = h.call(
        "workflow_check",
        &json!({"checks":[{"id":"bad","kind":"coverage","observation":{"source":"example"}}]}),
    );
    assert_eq!(bad["error"]["code"], "invalid_arguments");
    assert!(!dir.path().join(crate::state::DB_REL_PATH).exists());
}

#[test]
fn reviewed_tracking_move_refuses_manual_edit_and_keeps_canonical_state() {
    let directory = fixture();
    let engine = Engine::open(directory.path()).unwrap();
    let run = start(&engine, 1);
    let record = "applications/acme-engineer-123.md";
    let original = call(&engine, "read_record", json!({"path":record}));
    let path = directory.path().join(record);
    fs::write(
        &path,
        format!(
            "{}\nA newer manual note must survive.\n",
            original["text"].as_str().unwrap()
        ),
    )
    .unwrap();
    let before_record = fs::read(&path).unwrap();
    let before_state = engine.load_state().unwrap();
    let arguments = json!({"run_id":run,"record":record,"tracking":{"stage":"screening","checked":"2026-10-12","next_action":"Prepare confirmed screening"},"summary":"Explicit synthetic reviewed move","evidence":["https://example.test/reviewed-instruction"],"expected_record_sha256":original["sha256"],"idempotency_key":"reviewed-move"});
    let stale = engine.call("update_application", &arguments);
    assert_eq!(stale["error"]["code"], "stale_record");
    assert_eq!(fs::read(&path).unwrap(), before_record);
    assert_eq!(engine.load_state().unwrap(), before_state);
    let fresh = call(&engine, "read_record", json!({"path":record}));
    let mut current = arguments.clone();
    current["expected_record_sha256"] = fresh["sha256"].clone();
    assert_eq!(
        call(&engine, "update_application", current.clone())["tracking"]["stage"],
        "screening"
    );
    let committed = fs::read(&path).unwrap();
    call(&engine, "update_application", current);
    assert_eq!(fs::read(&path).unwrap(), committed);
    let mut invalid = arguments;
    invalid["expected_record_sha256"] = json!("g".repeat(64));
    invalid["idempotency_key"] = json!("invalid-hash");
    assert_eq!(
        engine.call("update_application", &invalid)["error"]["code"],
        "invalid_arguments"
    );
    assert_eq!(fs::read(&path).unwrap(), committed);
}

#[test]
fn suggest_meeting_times_uses_workspace_timezone_and_scheduling() {
    let dir = fixture();
    fs::write(
        dir.path().join("config/career-ledger.json"),
        json!({"schema_version":1,"timezone":"Europe/London","primary_cv":"output/cv.pdf",
            "scheduling":{"max_per_day":2,"kinds":{"interview":{"windows":[{"days":["mon"],"start":"09:45","end":"10:15"}]}}}})
        .to_string(),
    )
    .unwrap();
    let h = Engine::open(dir.path()).unwrap();
    let data = call(
        &h,
        "suggest_meeting_times",
        json!({"kind":"interview","duration_minutes":30,"now":"2026-11-25T13:00:00Z",
            "candidates":[{"start":"2026-11-30T09:45:00Z"},{"start":"2026-12-01T10:30:00+00:00"}],
            "busy":[{"start":"2026-12-01T10:30:00Z","end":"2026-12-01T11:00:00Z","kind":"interview","label":"Sunsave"}]}),
    );
    assert_eq!(data["timezone"], "Europe/London");
    assert_eq!(
        data["suggestions"][0]["label"],
        "Mon 30 Nov 2026, 09:45-10:15 GMT"
    );
    assert_eq!(data["suggestions"][0]["in_window"], true);
    assert_eq!(
        data["rejected"][0]["reasons"][0],
        "Clashes with Sunsave (15-minute buffer)"
    );
    let bad = h.call(
        "suggest_meeting_times",
        &json!({"kind":"screen","duration_minutes":30,"candidates":[{"start":"2026-11-30 09:45"}]}),
    );
    assert_eq!(bad["error"]["code"], "invalid_arguments");
}

#[test]
fn preparation_requires_a_frozen_job_ad_or_an_explicit_reason() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let base = json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0}});
    let with = |extra: Value, key: &str| {
        let mut args = base.clone();
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        args["idempotency_key"] = json!(key);
        args
    };
    let missing = h.call("prepare_application", &with(json!({}), "none"));
    assert_eq!(missing["error"]["code"], "invalid_arguments");
    assert!(
        missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("vacancy_text_path")
    );
    fs::write(dir.path().join("applications/short.md"), "Apply now").unwrap();
    let short = h.call(
        "prepare_application",
        &with(
            json!({"vacancy_text_path":"applications/short.md"}),
            "short",
        ),
    );
    assert_eq!(short["error"]["code"], "invalid_arguments");

    let prepared = call(
        &h,
        "prepare_application",
        with(json!({"vacancy_text_path":"applications/vacancy.md"}), "ad"),
    );
    let ad = &prepared["packet"]["vacancy"];
    assert_eq!(ad["original_path"], "applications/vacancy.md");
    let frozen = fs::read(dir.path().join(ad["path"].as_str().unwrap())).unwrap();
    assert_eq!(
        frozen,
        fs::read(dir.path().join("applications/vacancy.md")).unwrap()
    );
    assert_eq!(json!(domain::digest(&frozen)), ad["sha256"]);
    // Editing the frozen copy after preparation is caught before submit.
    fs::write(dir.path().join(ad["path"].as_str().unwrap()), "changed").unwrap();
    let packet = prepared["packet_id"].as_str().unwrap();
    assert_eq!(
        h.call("get_packet", &json!({"packet_id":packet}))["error"]["code"],
        "artifact_changed"
    );

    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let mut args = with(
        json!({"vacancy_text_unavailable":"Recruiter email with no written spec"}),
        "reason",
    );
    args["run_id"] = json!(run);
    let prepared = call(&h, "prepare_application", args);
    assert_eq!(
        prepared["packet"]["vacancy_unavailable"],
        "Recruiter email with no written spec"
    );
}

fn write_settings(h: &Engine, extra: Value) {
    let mut settings = json!({"schema_version":1,"timezone":"UTC","primary_cv":"output/cv.pdf"});
    for (key, value) in extra.as_object().unwrap() {
        settings[key] = value.clone();
    }
    fs::write(
        h.root.join("config/career-ledger.json"),
        settings.to_string(),
    )
    .unwrap();
}

#[test]
fn begin_needs_the_config_permission_for_its_channel() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 2);
    let prepared = prepare(&h, &run, "prepare");
    let begin = json!({"attempt_id":prepared["attempt_id"],"idempotency_key":"begin"});
    for settings in [
        json!({}),
        json!({"permissions":{}}),
        json!({"permissions":{"auto_apply_portal":false,"auto_send_first_contact_email":true}}),
    ] {
        write_settings(&h, settings);
        let before = crate::state::load(dir.path()).unwrap();
        let refused = h.call("begin_submission", &begin);
        assert_eq!(refused["error"]["code"], "permission_denied", "{refused}");
        assert_eq!(before, crate::state::load(dir.path()).unwrap());
    }
    // Portal permission does not cover sending a first-contact email.
    write_settings(&h, json!({"permissions":{"auto_apply_portal":true}}));
    call(
        &h,
        "cancel_reservation",
        json!({"attempt_id":prepared["attempt_id"],"summary":"Switch channel","idempotency_key":"cancel"}),
    );
    let email = call(
        &h,
        "prepare_application",
        json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","channel":"direct_email","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":"email"}),
    );
    let refused = h.call(
        "begin_submission",
        &json!({"attempt_id":email["attempt_id"],"idempotency_key":"begin-email"}),
    );
    assert_eq!(refused["error"]["code"], "permission_denied", "{refused}");
    write_settings(
        &h,
        json!({"permissions":{"auto_apply_portal":true,"auto_send_first_contact_email":true}}),
    );
    assert_eq!(
        call(
            &h,
            "begin_submission",
            json!({"attempt_id":email["attempt_id"],"idempotency_key":"begin-email"})
        )["ready_for_single_submit"],
        true
    );
}

#[test]
fn excluded_ats_and_avoided_employers_are_refused_at_prepare_and_begin() {
    let dir = fixture();
    fs::write(dir.path().join("applications/acme-workday-9.md"), "# Acme — Platform Engineer\n\n- Stage: saved\n- Source URL: https://acme.wd3.myworkdayjobs.com/en-US/careers/job/9\n").unwrap();
    let h = Engine::open(dir.path()).unwrap();
    write_settings(
        &h,
        json!({"permissions":{"auto_apply_portal":true},"discovery":{"exclude_ats":["Workday"]}}),
    );
    let run = start(&h, 2);
    let refused = h.call(
        "prepare_application",
        &json!({"run_id":run,"record":"applications/acme-workday-9.md","company":"Acme","vacancy_url":"https://acme.wd3.myworkdayjobs.com/en-US/careers/job/9","source":"employer","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":"workday"}),
    );
    assert_eq!(refused["error"]["code"], "excluded_by_filters", "{refused}");
    // A filter added after reservation still stops the begin.
    let prepared = prepare(&h, &run, "prepare");
    write_settings(
        &h,
        json!({"permissions":{"auto_apply_portal":true},"discovery":{"avoid_list":["ACME"]}}),
    );
    let refused = h.call(
        "begin_submission",
        &json!({"attempt_id":prepared["attempt_id"],"idempotency_key":"begin"}),
    );
    assert_eq!(refused["error"]["code"], "excluded_by_filters", "{refused}");
}

#[test]
fn same_employer_and_role_on_another_url_is_a_duplicate() {
    let dir = fixture();
    fs::write(
        dir.path().join("applications/acme-senior-engineer-linkedin.md"),
        "# Acme — Senior  engineer\n\n<!-- tracking\n{\"stage\":\"applied\",\"checked\":\"2026-10-12\",\"next_action\":\"Wait\",\"company\":\"Acme\",\"vacancy_url\":\"https://www.linkedin.com/jobs/view/42\"}\n-->\n",
    )
    .unwrap();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let refused = h.call(
        "prepare_application",
        &json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":"prepare"}),
    );
    assert_eq!(refused["error"]["code"], "duplicate_blocked", "{refused}");
    // A closed earlier attempt (e.g. an expired posting) does not block a new one.
    fs::write(
        dir.path().join("applications/acme-senior-engineer-linkedin.md"),
        "# Acme — Senior Engineer\n\n<!-- tracking\n{\"stage\":\"closed\",\"outcome\":\"expired\",\"checked\":\"2026-10-12\",\"next_action\":\"None\",\"company\":\"Acme\",\"vacancy_url\":\"https://www.linkedin.com/jobs/view/42\"}\n-->\n",
    )
    .unwrap();
    prepare(&h, &run, "prepare");
}

#[test]
fn no_tool_can_write_configuration_or_outside_the_workspace() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let config = fs::read(dir.path().join("config/career-ledger.json")).unwrap();
    let run = start(&h, 1);
    for record in [
        "config/career-ledger.json",
        "config/career-ledger.md",
        "applications/../config/career-ledger.md",
        "/etc/job-harness/allowed-workspaces",
        "../outside.md",
    ] {
        for tool in ["update_application", "record_event"] {
            let result = h.call(
                tool,
                &json!({"run_id":run,"kind":"status_updated","record":record,"tracking":{"stage":"saved"},"summary":"Attempted config write","evidence":["applications/approved-answers.md"],"idempotency_key":format!("{tool}:{record}")}),
            );
            assert_eq!(result["ok"], false, "{tool} {record}: {result}");
        }
        let result = h.call(
            "create_application",
            &json!({"run_id":run,"record":record,"company":"X","role":"Y","vacancy_url":"https://x.example/1","source":"employer","fit_reason":"x","evidence":["applications/approved-answers.md"],"idempotency_key":format!("create:{record}")}),
        );
        assert_eq!(result["ok"], false, "create {record}: {result}");
        let result = h.call(
            "save_receipt",
            &json!({"run_id":run,"path":record,"idempotency_key":format!("receipt:{record}")}),
        );
        assert_eq!(result["ok"], false, "receipt {record}: {result}");
    }
    assert_eq!(
        config,
        fs::read(dir.path().join("config/career-ledger.json")).unwrap()
    );
    assert!(!dir.path().join("config/career-ledger.md").exists());
}

#[test]
fn a_host_pin_refuses_every_other_workspace() {
    let dir = fixture();
    let copy = fixture();
    let pins = tempfile::tempdir().unwrap();
    let pin = pins.path().join("allowed-workspaces");
    let root = fs::canonicalize(dir.path()).unwrap();
    let copy_root = fs::canonicalize(copy.path()).unwrap();
    check_workspace_pin(&root, &pin).unwrap(); // no pin file: no restriction
    fs::write(&pin, format!("# pinned\n{}\n", dir.path().display())).unwrap();
    check_workspace_pin(&root, &pin).unwrap();
    assert_eq!(
        check_workspace_pin(&copy_root, &pin).unwrap_err().code,
        "workspace_not_pinned"
    );
    fs::write(&pin, "").unwrap();
    assert_eq!(
        check_workspace_pin(&root, &pin).unwrap_err().code,
        "workspace_not_pinned"
    );
}

fn prepare_job(h: &Engine, run: &str, key: &str, job: Value) -> Value {
    h.call(
        "prepare_application",
        &json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","job":job,"idempotency_key":key}),
    )
}
const JOB_LIMITS: &str = r#"{"permissions":{"auto_apply_portal":true},"discovery":{"min_base_salary":80000,"currency":"GBP","allowed_locations":["United Kingdom","London"],"allowed_work_arrangements":["remote","hybrid"],"max_office_days_per_week":3}}"#;

#[test]
fn job_terms_must_be_set_from_the_ad_before_preparing() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    for (job, key) in [
        (Value::Null, "none"),
        (
            json!({"salary":"unknown","location":"London","work_arrangement":"remote"}),
            "partial",
        ),
    ] {
        let mut args = json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":"applications/answers.json","vacancy_text_path":"applications/vacancy.md","idempotency_key":key});
        if !job.is_null() {
            args["job"] = job;
        }
        let refused = h.call("prepare_application", &args);
        assert_eq!(refused["error"]["code"], "job_fields_missing", "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("office_days_per_week")
        );
    }
}

#[test]
fn job_terms_outside_the_config_are_refused() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    write_settings(&h, serde_json::from_str(JOB_LIMITS).unwrap());
    let run = start(&h, 1);
    let good = json!({"salary":{"min":70000,"max":90000,"currency":"GBP","period":"year"},"location":"London, United Kingdom","work_arrangement":"hybrid","office_days_per_week":2});
    for (change, key, reason) in [
        (
            json!({"salary":{"min":60000,"max":75000,"currency":"gbp","period":"year"}}),
            "low",
            "below the floor",
        ),
        (
            json!({"location":"Berlin, Germany"}),
            "place",
            "allowed_locations",
        ),
        // Whole words only: "Londonderry" is not "London".
        (
            json!({"location":"Londonderry"}),
            "word",
            "allowed_locations",
        ),
        (
            json!({"work_arrangement":"onsite","office_days_per_week":5}),
            "onsite",
            "allowed_work_arrangements",
        ),
        (json!({"office_days_per_week":4}), "days", "max_office_days"),
    ] {
        let mut job = good.clone();
        for (k, v) in change.as_object().unwrap() {
            job[k] = v.clone();
        }
        let refused = prepare_job(&h, &run, key, job);
        assert_eq!(
            refused["error"]["code"], "excluded_by_filters",
            "{key}: {refused}"
        );
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains(reason),
            "{key}: {refused}"
        );
    }
    let prepared = prepare_job(&h, &run, "good", good);
    assert_eq!(prepared["ok"], true, "{prepared}");
    assert_eq!(prepared["data"]["packet"]["job"]["unchecked"], json!([]));
}

#[test]
fn an_unknown_salary_is_allowed_and_noted() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    write_settings(&h, serde_json::from_str(JOB_LIMITS).unwrap());
    let run = start(&h, 1);
    let prepared = prepare_job(
        &h,
        &run,
        "unknown",
        json!({"salary":"unknown","location":"Remote, United Kingdom","work_arrangement":"remote","office_days_per_week":0}),
    );
    assert_eq!(prepared["ok"], true, "{prepared}");
    assert_eq!(
        prepared["data"]["packet"]["job"]["unchecked"],
        json!(["salary"])
    );
    let attempt = prepared["data"]["attempt_id"].as_str().unwrap();
    // The note travels with the attempt into the coordinator.
    let state = h.load_state().unwrap();
    assert_eq!(state["attempts"][attempt]["job"]["salary"], "unknown");
    assert_eq!(
        state["attempts"][attempt]["job"]["unchecked"],
        json!(["salary"])
    );
    // A day rate cannot be compared with an annual floor; it is noted, not refused.
    let run2 = call(&h,"start_run",json!({"operation":"apply","scope":{"sources":["employer"],"max_submissions":1,"dry_run":false},"user_request":"Synthetic test","idempotency_key":"run2"}))["run_id"].as_str().unwrap().to_owned();
    call(
        &h,
        "cancel_reservation",
        json!({"attempt_id":attempt,"summary":"Retry with a day rate","idempotency_key":"cancel"}),
    );
    let day = prepare_job(
        &h,
        &run2,
        "day",
        json!({"salary":{"min":500,"currency":"GBP","period":"day"},"location":"London","work_arrangement":"hybrid","office_days_per_week":"unknown"}),
    );
    assert_eq!(day["ok"], true, "{day}");
    assert_eq!(
        day["data"]["packet"]["job"]["unchecked"],
        json!(["office_days_per_week", "salary"])
    );
}

#[test]
fn begin_rechecks_job_terms_and_refuses_packets_without_them() {
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 2);
    let prepared = prepare(&h, &run, "prepare");
    // A floor raised after reservation stops the begin.
    let mut limits: Value = serde_json::from_str(JOB_LIMITS).unwrap();
    limits["discovery"]["allowed_work_arrangements"] = json!(["hybrid"]);
    write_settings(&h, limits);
    let refused = h.call(
        "begin_submission",
        &json!({"attempt_id":prepared["attempt_id"],"idempotency_key":"begin"}),
    );
    assert_eq!(refused["error"]["code"], "excluded_by_filters", "{refused}");
    // A reservation made before job terms existed must be prepared again.
    write_settings(&h, json!({"permissions":{"auto_apply_portal":true}}));
    let attempt = prepared["attempt_id"].as_str().unwrap();
    let mut state = h.load_state().unwrap();
    let packet_id = state["attempts"][attempt]["packet_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = dir
        .path()
        .join(format!("applications/operations/packets/{packet_id}.json"));
    let mut packet: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    packet.as_object_mut().unwrap().remove("job");
    let old_id = domain::digest(&domain::encoded(&packet).unwrap());
    fs::write(
        dir.path()
            .join(format!("applications/operations/packets/{old_id}.json")),
        domain::encoded(&packet).unwrap(),
    )
    .unwrap();
    state["attempts"][attempt]["packet_id"] = json!(old_id);
    h.commit(&mut state, &[]).unwrap();
    let refused = h.call(
        "begin_submission",
        &json!({"attempt_id":attempt,"idempotency_key":"begin-old"}),
    );
    assert_eq!(refused["error"]["code"], "job_fields_missing", "{refused}");
}

#[test]
fn an_agent_without_file_access_can_save_and_read_artifacts() {
    use base64::Engine as _;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let dir = fixture();
    let h = Engine::open(dir.path()).unwrap();
    let run = start(&h, 1);
    let ad = fs::read(dir.path().join("applications/vacancy.md")).unwrap();
    let saved = call(
        &h,
        "save_artifact",
        json!({"name":"ad.md","content_base64":b64(&ad),"idempotency_key":"ad"}),
    );
    let ad_path = saved["path"].as_str().unwrap().to_owned();
    assert!(ad_path.starts_with("applications/operations/uploads/"));
    let answers = call(
        &h,
        "save_artifact",
        json!({"name":"answers.json","content_base64":b64(br#"{"experience":6}"#),"idempotency_key":"answers"}),
    );
    let prepared = call(
        &h,
        "prepare_application",
        json!({"run_id":run,"record":"applications/acme-engineer-123.md","company":"Acme","vacancy_url":"https://jobs.example.com/acme/123","source":"employer","posting_id":"123","cv_path":"output/cv.pdf","answers_path":answers["path"],"vacancy_text_path":ad_path,"job":{"salary":"unknown","location":"London, UK","work_arrangement":"remote","office_days_per_week":0},"idempotency_key":"prepare"}),
    );
    let cv = &prepared["packet"]["cv"];
    let read = call(&h, "read_artifact", json!({"path":cv["path"]}));
    assert_eq!(read["sha256"], cv["sha256"]);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(read["content_base64"].as_str().unwrap())
            .unwrap(),
        fs::read(dir.path().join("output/cv.pdf")).unwrap()
    );
    for (tool, args) in [
        (
            "save_artifact",
            json!({"name":"run.sh","content_base64":b64(b"x"),"idempotency_key":"sh"}),
        ),
        (
            "save_artifact",
            json!({"name":"x.md","content_base64":"not base64!","idempotency_key":"bad"}),
        ),
        ("read_artifact", json!({"path":"config/career-ledger.json"})),
        (
            "read_artifact",
            json!({"path":"applications/operations/harness.sqlite"}),
        ),
        ("read_artifact", json!({"path":"../outside.md"})),
    ] {
        assert_eq!(h.call(tool, &args)["ok"], false, "{tool} {args}");
    }
}
