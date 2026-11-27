//! Evidence reads, source-scoped answers, duplicates and tracker health.
use super::*;

pub(super) struct DuplicateRecord {
    row: Value,
    employer_key: String,
    urls: BTreeSet<String>,
    folded_text: String,
    lower_filename: String,
}

impl Engine {
    pub(super) fn read_record(&self, rel: &str) -> Result<Value> {
        let mut origin = "ledger";
        let data = match self.text_path(rel) {
            Ok(path) if path.is_file() => fs::read(path)?,
            other => {
                if let Some(text) = workspace::resource_text(rel) {
                    let alias = self.root.join(rel);
                    // An external alias may only resolve to the exact embedded
                    // generic bytes. This also works for relocated installations.
                    if alias.exists() && fs::read(&alias)? != text.as_bytes() {
                        return Err(Error::new(
                            "path_denied",
                            "Generic resource alias points at different external content",
                        ));
                    }
                    origin = "toolkit";
                    text.as_bytes().to_vec()
                } else {
                    fs::read(other?)?
                }
            }
        };
        if data.len() > 1_000_000 {
            return Err(Error::new(
                "invalid_arguments",
                "Record exceeds one MB; choose a narrower evidence record",
            ));
        }
        let text = String::from_utf8(data.clone())
            .map_err(|_| Error::new("invalid_state", "Record is not UTF-8 text"))?;
        Ok(json!({"path":rel,"origin":origin,"sha256":domain::digest(&data),"text":text}))
    }
    pub(super) fn find_application(&self, args: &Value) -> Result<Value> {
        let words = words(args.get("query").and_then(Value::as_str).unwrap_or(""));
        let rows = pipeline::build(&self.root)?.records;
        let mut found = vec![];
        for mut row in rows {
            if args.get("stage").is_some() && args["stage"] != row["stage"] {
                continue;
            }
            let rel = format!("applications/{}", s(&row, "record")?);
            let text = fs::read_to_string(self.text_path(&rel)?)?;
            let search = domain::casefold(&format!(
                "{} {} {}",
                s(&row, "title")?,
                s(&row, "record")?,
                text
            ));
            if words.iter().all(|w| search.contains(w)) {
                row["path"] = json!(rel);
                found.push(row);
            }
        }
        Ok(json!({"matches":found}))
    }
    pub(super) fn answers(&self, query: &str) -> Result<Value> {
        let words = words(query);
        let mut rows = vec![];
        let regex = Regex::new(r"(?m)^## ").unwrap();
        for source in self.settings()?["answer_sources"].as_array().unwrap() {
            let rel = source.as_str().unwrap();
            let text = fs::read_to_string(self.text_path(rel)?)?;
            let mut start = 0;
            for next in regex
                .find_iter(&text)
                .map(|m| m.start())
                .chain(std::iter::once(text.len()))
            {
                let section = &text[start..next];
                if words.iter().all(|w| domain::casefold(section).contains(w)) {
                    rows.push(json!({"path":rel,"text":section}));
                }
                start = next;
            }
        }
        Ok(json!({"matches":rows}))
    }
    pub(super) fn duplicate(
        &self,
        state: &Value,
        company: &str,
        vacancy_url: &str,
        posting_id: Option<&str>,
        exclude: Option<&str>,
    ) -> Result<Value> {
        let url = domain::normal_url(vacancy_url)?;
        Self::match_duplicates(
            &self.duplicate_records()?,
            state,
            company,
            &url,
            posting_id,
            exclude,
        )
    }

    /// Another record for the same employer and role title that already moved
    /// past preparation blocks a second application through a different URL.
    pub(super) fn role_duplicate(&self, record: &str) -> Result<Option<String>> {
        let separator = Regex::new(r"\s+[—–-]\s+").unwrap();
        let identity = |row: &Value| -> Option<(String, String)> {
            let mut parts = separator.splitn(row["title"].as_str()?, 2);
            let employer = row
                .get("company")
                .and_then(Value::as_str)
                .or(parts.next())?;
            let role: Vec<String> = parts
                .last()?
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(domain::casefold)
                .collect();
            (!role.is_empty()).then(|| (domain::company_key(employer), role.join(" ")))
        };
        let rows = pipeline::build(&self.root)?.records;
        let own = Path::new(record).file_name().and_then(|x| x.to_str());
        let Some(target) = rows
            .iter()
            .find(|row| row["record"].as_str() == own)
            .and_then(identity)
        else {
            return Ok(None);
        };
        Ok(rows
            .iter()
            .filter(|row| row["record"].as_str() != own)
            .filter(|row| {
                // Unclassified legacy prose is not evidence of a live application.
                let stage = row["stage"].as_str().unwrap_or("saved");
                !["saved", "prepared", "closed"].contains(&stage)
                    && (row["tracking_source"] == "structured" || stage != "needs-review")
            })
            .find(|row| identity(row).as_ref() == Some(&target))
            .and_then(|row| row["record"].as_str().map(str::to_owned)))
    }

    pub(super) fn duplicate_records(&self) -> Result<Vec<DuplicateRecord>> {
        let rows = pipeline::build(&self.root)?.records;
        let employer_pattern = Regex::new(r"\s+[—–-]\s+").unwrap();
        let urls_pattern = Regex::new(r#"https?://[^\s<>\)\]\",]+"#).unwrap();
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let filename = s(&row, "record")?;
            let text = fs::read_to_string(self.text_path(&format!("applications/{filename}"))?)?;
            let employer = row
                .get("company")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    employer_pattern
                        .split(row["title"].as_str().unwrap_or_default())
                        .next()
                        .unwrap_or_default()
                });
            let employer_key = domain::company_key(employer);
            let mut urls = BTreeSet::new();
            if let Some(v) = row.get("vacancy_url").and_then(Value::as_str) {
                urls.insert(domain::normal_url(v)?);
            }
            for value in urls_pattern.find_iter(&text) {
                if let Ok(v) = domain::normal_url(value.as_str().trim_end_matches(['.', ';'])) {
                    urls.insert(v);
                }
            }
            let lower_filename = filename.to_lowercase();
            records.push(DuplicateRecord {
                row,
                employer_key,
                urls,
                folded_text: domain::casefold(&text),
                lower_filename,
            });
        }
        Ok(records)
    }

    pub(super) fn match_duplicates(
        records: &[DuplicateRecord],
        state: &Value,
        company: &str,
        url: &str,
        posting_id: Option<&str>,
        exclude: Option<&str>,
    ) -> Result<Value> {
        let employer_key = domain::company_key(company);
        let posting_keys = posting_id.map(|id| (id, domain::casefold(id), id.to_lowercase()));
        let mut same = vec![];
        let mut exact = vec![];
        for record in records {
            let row = &record.row;
            let company_matches = employer_key == record.employer_key;
            let identity = record.urls.contains(url)
                || posting_keys.as_ref().is_some_and(|(id, folded, lower)| {
                    company_matches
                        && (row.get("posting_id").and_then(Value::as_str) == Some(*id)
                            || record.folded_text.contains(folded.as_str())
                            || record.lower_filename.contains(lower.as_str()))
                });
            if company_matches || identity {
                same.push(row.clone());
            }
            if identity {
                exact.push(row.clone());
            }
        }
        let blocked_records: Vec<Value> = exact
            .iter()
            .filter(|r| r["stage"] != "saved" && r["stage"] != "prepared")
            .cloned()
            .collect();
        let mut blockers = vec![];
        for a in state["attempts"].as_object().unwrap().values() {
            if exclude == a["id"].as_str() {
                continue;
            }
            let status = SubmissionState::parse(&a["status"])?;
            let stored_url = domain::normal_url(s(a, "vacancy_url")?)?;
            if (status.unresolved()
                && (stored_url == url
                    || domain::company_key(a["company"].as_str().unwrap_or_default())
                        == employer_key))
                || (status == SubmissionState::Submitted && stored_url == url)
            {
                blockers.push(a.clone());
            }
        }
        Ok(
            json!({"blocked":!blocked_records.is_empty()||!blockers.is_empty(),"exact_matches":exact,"company_matches":same,"blocking_records":blocked_records,"blocking_attempts":blockers}),
        )
    }
    pub(super) fn reconcile(&self, state: &Value) -> Result<Value> {
        let built = pipeline::build(&self.root)?;

        let md_path = self.root.join("applications/pipeline.md");
        let json_path = self.root.join("applications/pipeline.json");
        let stale = fs::read_to_string(md_path).ok().as_deref() != Some(&built.markdown)
            || fs::read(json_path).ok().as_deref()
                != Some(pipeline::json_bytes(&built.records)?.as_slice());
        let unfinished: Vec<_> = state["runs"]
            .as_object()
            .unwrap()
            .values()
            .filter(|r| r["status"] == "running")
            .cloned()
            .collect();
        let unresolved: Vec<_> = state["attempts"]
            .as_object()
            .unwrap()
            .values()
            .filter(|a| SubmissionState::parse(&a["status"]).is_ok_and(SubmissionState::unresolved))
            .cloned()
            .collect();
        Ok(
            json!({"records":built.records.len(),"legacy_records":built.records.iter().filter(|r|r["tracking_source"]=="legacy-prose").count(),"needs_review":built.records.iter().filter(|r|r["stage"]=="needs-review").collect::<Vec<_>>(),"pipeline_stale":stale,"unfinished_runs":unfinished,"unresolved_attempts":unresolved}),
        )
    }
}
