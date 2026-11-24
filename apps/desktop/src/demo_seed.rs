//! Built-in pure-Rust synthetic demo workspace seeder so anyone who downloads
//! `Job Harness.app` can click "Try with sample data" with zero Python or CLI setup.
use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use std::{fs, path::PathBuf};

/// Minimal valid 1-page PDF bytes (pure ASCII, zero external dependencies).
fn minimal_pdf(headline: &str, subtitle: &str) -> Vec<u8> {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    };
    let stream = format!(
        "BT\n/F1 18 Tf\n54 740 Td\n({}) Tj\n0 -24 Td\n/F1 11 Tf\n({}) Tj\n0 -22 Td\n(Synthetic sample CV bundled with Job Harness for local preview.) Tj\nET\n",
        esc(headline),
        esc(subtitle)
    );
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    let objs = [
        "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_owned(),
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_owned(),
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n".to_owned(),
        "4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_owned(),
        format!(
            "5 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n",
            stream.len(),
            stream
        ),
    ];
    for obj in &objs {
        offsets.push(pdf.len());
        pdf.push_str(obj);
    }
    let xref_pos = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objs.len() + 1
    ));
    for off in offsets {
        pdf.push_str(&format!("{off:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
        objs.len() + 1
    ));
    pdf.into_bytes()
}

pub fn sample_workspace_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if cfg!(target_os = "macos") {
        Some(home.join("Library/Application Support/Job Harness/sample-workspace"))
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        Some(PathBuf::from(xdg).join("job-harness/sample-workspace"))
    } else {
        Some(home.join(".local/share/job-harness/sample-workspace"))
    }
}

/// Creates (or refreshes) a realistic 10-role synthetic workspace with submitted CV PDFs
/// and 2-layer interview briefs so strangers can explore Kanban, Sankey, Briefs, and CVs immediately.
pub fn ensure_sample_workspace() -> Result<PathBuf> {
    let root = sample_workspace_dir().context("Could not resolve app data directory")?;
    if root.join("config/career-ledger.json").is_file()
        && root.join("applications/pipeline.json").is_file()
    {
        return Ok(root);
    }
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    job_harness_core::workspace::initialize(&root)
        .map_err(|e| anyhow::anyhow!("Sample init failed: {e}"))?;

    let now = Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let d_ago = |days: i64| (now - Duration::days(days)).format("%Y-%m-%d").to_string();
    let in_2h = (now + Duration::hours(2))
        .format("%Y-%m-%dT%H:%M:00Z")
        .to_string();
    let in_2d = (now + Duration::days(2))
        .format("%Y-%m-%dT14:00:00Z")
        .to_string();

    // Write valid sample CV PDFs (master + 2 tracks)
    fs::create_dir_all(root.join("output/tracks/backend"))?;
    fs::create_dir_all(root.join("output/tracks/product"))?;
    fs::write(
        root.join("output/sample-cv.pdf"),
        minimal_pdf(
            "Alex Rivera — Senior Software Engineer",
            "Master CV (Backend · Product · Distributed Systems)",
        ),
    )?;
    fs::write(
        root.join("output/tracks/backend/sample-cv.pdf"),
        minimal_pdf(
            "Alex Rivera — Senior Backend Engineer",
            "Backend track: Rust, Go, PostgreSQL, event-driven systems",
        ),
    )?;
    fs::write(
        root.join("output/tracks/product/sample-cv.pdf"),
        minimal_pdf(
            "Alex Rivera — Senior Product Engineer",
            "Product track: TypeScript, React, APIs, customer-facing craft",
        ),
    )?;

    // Configure clean, realistic sample defaults (fail-closed permissions = false)
    let cfg = serde_json::json!({
        "schema_version": 1,
        "timezone": "UTC",
        "max_daily_submissions": 5,
        "daily_channel_limits": { "portal": 4, "direct_email": 1, "recruiter_outreach": 0 },
        "permissions": {
            "auto_apply_portal": false,
            "auto_send_first_contact_email": false,
            "auto_reply_threads": false,
            "auto_book_calendar": false,
            "auto_archive_mail": false
        },
        "discovery": {
            "sources": ["official_employer_ats_boards", "hn_who_is_hiring", "cord"],
            "exclude_ats": ["workday"],
            "location_preferences": ["Remote (UK / Europe)", "London hybrid"],
            "min_base_salary": 95000,
            "target_base_salary": 115000,
            "currency": "GBP",
            "notice_period": "1 month"
        },
        "communication": {
            "locale": "en-GB",
            "tone": "direct, concise, plain-language, evidence-backed",
            "application_email": "cv@example.com",
            "sender_email": "alex@example.com",
            "cover_letter_mode": "only_when_required",
            "post_rejection_reply": "off"
        },
        "target_roles": ["Senior Backend Engineer", "Senior Product Engineer"],
        "cv_tracks": ["backend", "product"],
        "time_and_reminders": {
            "brief_lead_time_minutes": 90,
            "pre_interview_reminder_minutes": 90,
            "applied_ghost_days": 30
        },
        "primary_cv": "output/sample-cv.pdf",
        "preferred_browser": null,
        "answer_sources": [
            "applications/approved-answers.md",
            "context/profile/application-decisions.md"
        ]
    });
    fs::write(
        root.join("config/career-ledger.json"),
        serde_json::to_vec_pretty(&cfg)?,
    )?;

    // Sample 2-layer company brief for Northstar Ledger
    fs::create_dir_all(root.join("applications/briefs"))?;
    fs::write(
        root.join("applications/briefs/northstar-ledger.md"),
        "# Northstar Ledger — Company & Round Brief\n\n\
         ## Summary & fit\n\
         - **Product:** Real-time treasury reconciliation engine for European fintechs (~45 engineers, Series B).\n\
         - **Why fit is strong:** Direct overlap with transactional state machines, PostgreSQL locking, and zero-downtime ledger migrations.\n\
         - **Compensation:** £110k–£125k base + equity, London hybrid (2 days/wk).\n\n\
         ## Prep & stories\n\
         - **Story 1 (Idempotent payment pipeline):** Cut duplicate settlement bugs to zero using deterministic idempotency keys and advisory locks.\n\
         - **Story 2 (Latency & observability):** Dropped p99 query latency from 1.4s to 65ms by replacing N+1 ORM loops with indexed SQL.\n\
         - **Interviewer & company hooks:** CTO recently wrote on their engineering blog about deterministic replay testing for ledger state.\n\n\
         ## Questions & risks\n\
         - How do they handle schema evolution across multi-currency ledger partitions?\n\
         - Clarify on-call rotation cadence and ownership boundaries between core ledger and connectors.\n\n\
         ## Timeline\n\
         - Submitted backend track CV (`output/tracks/backend/sample-cv.pdf`, frozen SHA-256 verified).\n\
         - Passed 30-min hiring manager screen; technical systems deep-dive booked today.\n",
    )?;

    fs::write(
        root.join("applications/briefs/kestrel-cloud.md"),
        "# Kestrel Cloud — Company Brief\n\n\
         ## Summary & fit\n\
         - **Product:** Edge compute observability and log pipelines for developer teams.\n\
         - **Role:** Staff-track Product Engineer (£115k, UK remote).\n\n\
         ## Prep & stories\n\
         - Highlight design-system speedups and streaming telemetry UI architecture.\n\n\
         ## Questions & risks\n\
         - Ask how frontend and Rust ingestion teams share query contracts.\n\n\
         ## Timeline\n\
         - Final loop booked in 2 days.\n",
    )?;

    let roles: Vec<(&str, &str, serde_json::Value, &str)> = vec![
        (
            "northstar-ledger-senior-backend.md",
            "Northstar Ledger — Senior Backend Engineer",
            serde_json::json!({
                "company": "Northstar Ledger",
                "company_id": "northstar-ledger",
                "stage": "technical",
                "checked": today,
                "schedule_state": "booked",
                "scheduled_start": in_2h,
                "brief": "briefs/northstar-ledger.md",
                "status_detail": "Strong fit — £110k–£125k, London hybrid. Technical deep-dive in 2h.",
                "next_action": "prep_interview"
            }),
            "## Summary & fit\n- Submitted `output/tracks/backend/sample-cv.pdf` (backend track).\n- Hiring manager praised deterministic ledger experience.\n\n## Timeline\n- Passed intro & screen.\n- Technical architecture round booked today.\n",
        ),
        (
            "kestrel-cloud-product-engineer.md",
            "Kestrel Cloud — Senior Product Engineer",
            serde_json::json!({
                "company": "Kestrel Cloud",
                "company_id": "kestrel-cloud",
                "stage": "final",
                "checked": today,
                "schedule_state": "booked",
                "scheduled_start": in_2d,
                "brief": "briefs/kestrel-cloud.md",
                "status_detail": "Final founder & systems loop — £115k, UK remote.",
                "next_action": "prep_interview"
            }),
            "## Summary & fit\n- Submitted `output/tracks/product/sample-cv.pdf`.\n- Strong feedback on technical demo.\n",
        ),
        (
            "harbor-pay-platform-engineer.md",
            "HarborPay — Platform Engineer",
            serde_json::json!({
                "company": "HarborPay",
                "company_id": "harbor-pay",
                "stage": "offer",
                "checked": d_ago(1),
                "status_detail": "Verbal offer £108k + equity — reviewing written terms.",
                "next_action": "review_fit"
            }),
            "## Summary & fit\n- Verbal offer received; waiting on written contract before deciding.\n",
        ),
        (
            "meridian-ai-applied-engineer.md",
            "Meridian AI — Applied AI Engineer",
            serde_json::json!({
                "company": "Meridian AI",
                "company_id": "meridian-ai",
                "stage": "screening",
                "checked": d_ago(6),
                "status_detail": "Recruiter screen completed — awaiting technical slots.",
                "next_action": "follow_up"
            }),
            "## Summary & fit\n- Completed 25-min intro call; recruiter checking team availability.\n",
        ),
        (
            "verbena-health-backend.md",
            "Verbena Health — Senior Backend Engineer",
            serde_json::json!({
                "company": "Verbena Health",
                "company_id": "verbena-health",
                "stage": "introduction",
                "checked": d_ago(16),
                "status_detail": "Founder replied warmly; thread quiet 16d.",
                "next_action": "follow_up"
            }),
            "## Summary & fit\n- Direct founder reply; follow up or archive via Tidy.\n",
        ),
        (
            "cobalt-data-systems.md",
            "Cobalt Data — Distributed Systems Engineer",
            serde_json::json!({
                "company": "Cobalt Data",
                "company_id": "cobalt-data",
                "stage": "applied",
                "checked": d_ago(4),
                "status_detail": "Submitted backend track CV via Ashby.",
                "next_action": "wait_employer"
            }),
            "## Summary & fit\n- Submitted `output/tracks/backend/sample-cv.pdf`.\n",
        ),
        (
            "lattice-works-fullstack.md",
            "LatticeWorks — Senior Full-Stack Engineer",
            serde_json::json!({
                "company": "LatticeWorks",
                "company_id": "lattice-works",
                "stage": "saved",
                "checked": today,
                "status_detail": "Strong domain match — £105k–£120k, UK remote. Needs packet review.",
                "next_action": "prepare_packet"
            }),
            "## Summary & fit\n- Saved from HN Who is Hiring; verify compensation band and freeze product track CV.\n",
        ),
        (
            "foundry-labs-backend.md",
            "Foundry Labs — Senior Backend Engineer",
            serde_json::json!({
                "company": "Foundry Labs",
                "company_id": "foundry-labs",
                "stage": "closed",
                "outcome": "rejected",
                "checked": d_ago(9),
                "status_detail": "Rejected after technical screen — wanted deeper Kubernetes operator background.",
                "next_action": "none"
            }),
            "## Summary & fit\n- Clear technical feedback saved to career-learning.md.\n",
        ),
        (
            "quasar-finance-engineer.md",
            "Quasar Finance — Staff Engineer",
            serde_json::json!({
                "company": "Quasar Finance",
                "company_id": "quasar-finance",
                "stage": "closed",
                "outcome": "withdrawn",
                "checked": d_ago(12),
                "status_detail": "Withdrawn by candidate — 5 days/week in-office requirement.",
                "next_action": "none"
            }),
            "## Summary & fit\n- Withdrew politely after recruiter confirmed 5d on-site policy.\n",
        ),
        (
            "prism-analytics-backend.md",
            "Prism Analytics — Backend Engineer",
            serde_json::json!({
                "company": "Prism Analytics",
                "company_id": "prism-analytics",
                "stage": "closed",
                "outcome": "expired",
                "checked": d_ago(18),
                "status_detail": "Posting closed before submission.",
                "next_action": "none"
            }),
            "## Summary & fit\n- Role closed on employer board before packet submission.\n",
        ),
    ];

    for (file, title, tracking, body) in roles {
        let content = format!(
            "# {title}\n\n<!-- tracking\n{}\n-->\n\n{body}",
            serde_json::to_string_pretty(&tracking)?
        );
        fs::write(root.join("applications").join(file), content)?;
    }

    job_harness_core::pipeline::write(&root)
        .map_err(|e| anyhow::anyhow!("Sample pipeline build failed: {e}"))?;
    Ok(root)
}
