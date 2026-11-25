//! Bounded data-only composition; submit boundaries remain separate.
use super::*;

struct DuplicateSnapshot {
    _lock: storage::WorkspaceLock,
    state: Value,
    records: Vec<query::DuplicateRecord>,
}

impl Engine {
    pub(super) fn batch(&self, args: &Value, readonly: bool) -> Result<(Value, Vec<Value>)> {
        let steps = args["steps"].as_array().unwrap();
        if domain::encoded(&args["steps"])?.len() > 250000 {
            return Err(Error::new("invalid_arguments", "Batch exceeds 250 KB"));
        }
        let specs = contracts::specs();
        let mut seen = BTreeSet::new();
        for step in steps {
            let id = s(step, "id")?;
            if !valid_identifier(id) || !seen.insert(id) {
                return Err(Error::new(
                    "invalid_arguments",
                    "Step IDs must be unique identifiers",
                ));
            }
            let tool = s(step, "tool")?;
            let _spec = specs
                .get(tool)
                .ok_or_else(|| Error::new("unknown_tool", "Unknown batch tool"))?;
            if [
                "query_batch",
                "execute_batch",
                "begin_submission",
                "begin_mail_archive",
            ]
            .contains(&tool)
                || (tool == "daily_update" && step["arguments"]["action"] == "begin_archive")
            {
                return Err(Error::new(
                    "batch_boundary",
                    "Nested batches and one-shot external action boundaries cannot be batched",
                ));
            }
            if readonly && contracts::is_read_only(tool) != Some(true) {
                return Err(Error::new(
                    "invalid_arguments",
                    "query_batch accepts read-only tools only",
                ));
            }
        }
        let mut results = vec![];
        let mut values = BTreeMap::new();
        // Discovery sends duplicate checks together. Keep this snapshot local
        // to a homogeneous read batch so mixed batches never nest locks.
        let duplicate_batch =
            readonly && steps.iter().all(|step| step["tool"] == "check_duplicate");
        let mut duplicates = None;
        for (index, step) in steps.iter().enumerate() {
            let id = s(step, "id")?;
            let tool = s(step, "tool")?;
            let value = match resolve_refs(&step["arguments"], &values) {
                Ok(args) if tool == "daily_update" && args["action"] == "begin_archive" => self
                    .error_envelope(Error::new(
                        "batch_boundary",
                        "begin_archive requires a separate one-shot call",
                    )),
                Ok(args) if duplicate_batch => self.call_enveloped(tool, || {
                    self.check_duplicate_in_batch(&args, &mut duplicates)
                }),
                Ok(args) => self.call(tool, &args),
                Err(error) => self.error_envelope(error),
            };
            results.push(json!({"id":id,"tool":tool,"result":value}));
            if value["ok"] != true {
                let next = value["next"].as_array().cloned().unwrap_or_default();
                return Ok((
                    json!({"results":results,"completed":index,"failed_step":id,"skipped":steps.iter().skip(index+1).map(|s|s["id"].clone()).collect::<Vec<_>>()}),
                    next,
                ));
            }
            values.insert(id.to_owned(), value);
        }
        Ok((
            json!({"results":results,"completed":results.len(),"failed_step":null,"skipped":[]}),
            vec![],
        ))
    }

    fn check_duplicate_in_batch(
        &self,
        args: &Value,
        snapshot: &mut Option<DuplicateSnapshot>,
    ) -> Result<(Value, Vec<Value>)> {
        contracts::validate(&contracts::specs()["check_duplicate"]["inputSchema"], args)?;
        if snapshot.is_none() {
            let lock = storage::WorkspaceLock::acquire(&self.root, false)?;
            let state = crate::state::load_for_read(&self.root)?;
            if state.get("pending").is_some() {
                return Err(Error::new(
                    "pending_recovery",
                    "Pending transaction: call rebuild_pipeline to recover the local update",
                ));
            }
            *snapshot = Some(DuplicateSnapshot {
                _lock: lock,
                state,
                records: self.duplicate_records()?,
            });
        }
        let snapshot = snapshot.as_ref().unwrap();
        let raw = Self::match_duplicates(
            &snapshot.records,
            &snapshot.state,
            s(args, "company")?,
            &domain::normal_url(s(args, "vacancy_url")?)?,
            args.get("posting_id").and_then(Value::as_str),
            None,
        )?;
        Ok((
            self.compact("check_duplicate", raw, args, &snapshot.state)?,
            vec![],
        ))
    }
}
